use std::{
    collections::BTreeMap,
    fmt, fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use globset::Glob;
use serde::{
    de::{self, MapAccess, Visitor},
    Deserialize, Deserializer, Serialize,
};

use crate::{
    error::{FontbrewError, Result},
    fs::{write_atomically, GlobalFileLock},
    model::{FontFormat, RegistryStatusReport, RegistryUpdateReport},
    platform::FontbrewPaths,
    sources::GitHubRepo,
    FamilyName, PackageId,
};

const REGISTRY_SCHEMA_VERSION: u64 = 1;
const SUPPORTED_REQUIRED_BEHAVIORS: &[&str] = &["github", "asset-globs"];

pub const OFFICIAL_REGISTRY_URL: &str = "https://fontbrew.dev/registry.json";
pub const REGISTRY_URL_ENV_VAR: &str = "FONTBREW_REGISTRY_URL";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistrySnapshotStore {
    paths: FontbrewPaths,
}

impl RegistrySnapshotStore {
    pub fn new(paths: FontbrewPaths) -> Self {
        Self { paths }
    }

    pub fn parse(content: &str) -> Result<RegistrySnapshotV1> {
        RegistrySnapshotV1::parse(content)
    }

    pub fn read_snapshot(&self) -> Result<RegistrySnapshotV1> {
        let path = self.paths.registry_snapshot_path();
        let content = fs::read_to_string(&path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                return FontbrewError::RegistryValidationFailed {
                    message: format!(
                        "registry snapshot not found at {}; run `fontbrew registry update`",
                        path.display()
                    ),
                };
            }

            FontbrewError::Io(error)
        })?;

        RegistrySnapshotV1::parse(&content)
    }

    pub fn write_snapshot(&self, snapshot: &RegistrySnapshotV1) -> Result<()> {
        snapshot.validate()?;
        let content =
            serde_json::to_vec_pretty(snapshot).map_err(|source| registry_validation(source))?;
        write_atomically(&self.paths.registry_snapshot_path(), &content)
    }

    pub fn update_from_client(
        &self,
        client: &dyn RegistryHttpClient,
        registry_url: &str,
    ) -> Result<RegistryUpdateReport> {
        let content = client.get_text(registry_url)?;
        let snapshot = RegistrySnapshotV1::parse(&content)?;
        let _lock = GlobalFileLock::try_exclusive(&write_lock_path(&self.paths))?;
        self.write_snapshot(&snapshot)?;

        Ok(RegistryUpdateReport {
            registry_url: registry_url.to_string(),
            snapshot_path: self.paths.registry_snapshot_path(),
            registry_updated_at: snapshot.updated_at,
            package_count: snapshot.packages.len(),
        })
    }

    pub fn status(&self) -> Result<RegistryStatusReport> {
        let snapshot_path = self.paths.registry_snapshot_path();

        if !snapshot_path.exists() {
            return Ok(RegistryStatusReport {
                available: false,
                snapshot_path,
                registry_updated_at: None,
                snapshot_modified_at: None,
                package_count: 0,
            });
        }

        let snapshot = self.read_snapshot()?;
        let modified_at = fs::metadata(&snapshot_path)
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(format_system_time);

        Ok(RegistryStatusReport {
            available: true,
            snapshot_path,
            registry_updated_at: Some(snapshot.updated_at),
            snapshot_modified_at: modified_at,
            package_count: snapshot.packages.len(),
        })
    }

    pub fn resolve_short_name(&self, short_name: &str) -> Result<RegistryPackageRecipe> {
        self.read_snapshot()?.resolve_short_name(short_name)
    }
}

pub trait RegistryHttpClient {
    fn get_text(&self, url: &str) -> Result<String>;
}

#[derive(Debug, Default)]
pub struct ReqwestRegistryHttpClient {
    client: reqwest::blocking::Client,
}

impl RegistryHttpClient for ReqwestRegistryHttpClient {
    fn get_text(&self, url: &str) -> Result<String> {
        if let Some(path) = url.strip_prefix("file://") {
            return fs::read_to_string(PathBuf::from(path)).map_err(FontbrewError::from);
        }

        let response = self
            .client
            .get(url)
            .send()
            .map_err(|source| FontbrewError::Network {
                message: format!("could not fetch registry at {url}: {source}"),
            })?;
        let status = response.status();

        if !status.is_success() {
            return Err(FontbrewError::Network {
                message: format!("registry request failed with HTTP {status} for {url}"),
            });
        }

        response.text().map_err(|source| FontbrewError::Network {
            message: format!("could not read registry response from {url}: {source}"),
        })
    }
}

pub fn registry_url_from_env() -> String {
    std::env::var(REGISTRY_URL_ENV_VAR)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| OFFICIAL_REGISTRY_URL.to_string())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RegistrySnapshotV1 {
    pub schema_version: u64,
    pub updated_at: String,
    #[serde(default)]
    pub required: Vec<String>,
    #[serde(deserialize_with = "deserialize_package_map")]
    pub packages: BTreeMap<PackageId, RegistryPackageRecord>,
}

impl RegistrySnapshotV1 {
    pub fn parse(content: &str) -> Result<Self> {
        validate_schema_version(content)?;
        let snapshot: Self =
            serde_json::from_str(content).map_err(|source| registry_validation(source))?;
        snapshot.validate()?;

        Ok(snapshot)
    }

    pub fn resolve_short_name(&self, short_name: &str) -> Result<RegistryPackageRecipe> {
        let package_id = PackageId::parse(short_name)?;
        let package = self.packages.get(&package_id).ok_or_else(|| {
            FontbrewError::RegistryValidationFailed {
                message: format!("registry package not found: {}", package_id.as_str()),
            }
        })?;

        package.recipe(package_id)
    }

    fn validate(&self) -> Result<()> {
        if self.schema_version != REGISTRY_SCHEMA_VERSION {
            return registry_invalid(format!(
                "unsupported registry schemaVersion {}; expected {REGISTRY_SCHEMA_VERSION}",
                self.schema_version
            ));
        }

        if self.updated_at.trim().is_empty() {
            return registry_invalid("registry updatedAt cannot be empty");
        }

        validate_required_behaviors(&self.required, "required")?;

        for (package_id, package) in &self.packages {
            package.validate(package_id)?;
        }

        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RegistryPackageRecord {
    pub name: String,
    #[serde(default)]
    pub required: Vec<String>,
    pub source: RegistrySource,
    pub families: Vec<FamilyName>,
    pub release: Option<RegistryReleaseSelection>,
    pub asset: Option<RegistryAssetSelection>,
    pub install: Option<RegistryInstallOptions>,
}

impl RegistryPackageRecord {
    fn validate(&self, package_id: &PackageId) -> Result<()> {
        if self.name.trim().is_empty() {
            return registry_invalid(format!(
                "registry package {} has an empty name",
                package_id.as_str()
            ));
        }

        validate_required_behaviors(
            &self.required,
            &format!("packages.{}.required", package_id.as_str()),
        )?;

        match &self.source {
            RegistrySource::Github { repo } => {
                GitHubRepo::parse(repo).map_err(|error| {
                    FontbrewError::RegistryValidationFailed {
                        message: format!(
                            "registry package {} has invalid GitHub repo: {error}",
                            package_id.as_str()
                        ),
                    }
                })?;
            }
        }

        if self.families.is_empty() {
            return registry_invalid(format!(
                "registry package {} must list at least one family",
                package_id.as_str()
            ));
        }

        for family in &self.families {
            if family.as_str().trim().is_empty() {
                return registry_invalid(format!(
                    "registry package {} has an empty family name",
                    package_id.as_str()
                ));
            }
        }

        if let Some(asset) = &self.asset {
            asset.validate(package_id)?;
        }

        Ok(())
    }

    fn recipe(&self, package_id: PackageId) -> Result<RegistryPackageRecipe> {
        let github_repo = match &self.source {
            RegistrySource::Github { repo } => GitHubRepo::parse(repo)?,
        };
        let format_preference = self
            .install
            .as_ref()
            .map(RegistryInstallOptions::font_formats)
            .unwrap_or_default();

        Ok(RegistryPackageRecipe {
            package_id,
            name: self.name.clone(),
            github_repo,
            families: self.families.clone(),
            release: self.release.clone(),
            asset: self.asset.clone(),
            format_preference,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum RegistrySource {
    #[serde(rename = "github")]
    Github { repo: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RegistryReleaseSelection {
    pub channel: RegistryReleaseChannel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RegistryReleaseChannel {
    Stable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RegistryAssetSelection {
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
}

impl RegistryAssetSelection {
    fn validate(&self, package_id: &PackageId) -> Result<()> {
        validate_globs(package_id, "asset.include", &self.include)?;
        validate_globs(package_id, "asset.exclude", &self.exclude)?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RegistryInstallOptions {
    #[serde(default)]
    pub format_preference: Vec<RegistryFontFormat>,
}

impl RegistryInstallOptions {
    fn font_formats(&self) -> Vec<FontFormat> {
        self.format_preference
            .iter()
            .map(RegistryFontFormat::font_format)
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RegistryFontFormat {
    Otf,
    Ttf,
    Ttc,
    Otc,
}

impl RegistryFontFormat {
    fn font_format(&self) -> FontFormat {
        match self {
            Self::Otf => FontFormat::Otf,
            Self::Ttf => FontFormat::Ttf,
            Self::Ttc => FontFormat::Ttc,
            Self::Otc => FontFormat::Otc,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryPackageRecipe {
    pub package_id: PackageId,
    pub name: String,
    pub github_repo: GitHubRepo,
    pub families: Vec<FamilyName>,
    pub release: Option<RegistryReleaseSelection>,
    pub asset: Option<RegistryAssetSelection>,
    pub format_preference: Vec<FontFormat>,
}

fn deserialize_package_map<'de, D>(
    deserializer: D,
) -> std::result::Result<BTreeMap<PackageId, RegistryPackageRecord>, D::Error>
where
    D: Deserializer<'de>,
{
    deserializer.deserialize_map(PackageMapVisitor)
}

struct PackageMapVisitor;

impl<'de> Visitor<'de> for PackageMapVisitor {
    type Value = BTreeMap<PackageId, RegistryPackageRecord>;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a map of registry package ids to package records")
    }

    fn visit_map<A>(self, mut access: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut packages = BTreeMap::new();

        while let Some((package_id, package)) =
            access.next_entry::<PackageId, RegistryPackageRecord>()?
        {
            if packages.insert(package_id.clone(), package).is_some() {
                return Err(de::Error::custom(format!(
                    "duplicate registry package id {}",
                    package_id.as_str()
                )));
            }
        }

        Ok(packages)
    }
}

fn validate_schema_version(content: &str) -> Result<()> {
    let value: serde_json::Value =
        serde_json::from_str(content).map_err(|source| registry_validation(source))?;
    let found = value
        .get("schemaVersion")
        .and_then(serde_json::Value::as_u64);

    match found {
        Some(REGISTRY_SCHEMA_VERSION) => Ok(()),
        _ => registry_invalid(format!(
            "unsupported registry schemaVersion {found:?}; expected {REGISTRY_SCHEMA_VERSION}"
        )),
    }
}

fn validate_required_behaviors(required: &[String], field: &str) -> Result<()> {
    for behavior in required {
        if SUPPORTED_REQUIRED_BEHAVIORS.contains(&behavior.as_str()) {
            continue;
        }

        return registry_invalid(format!(
            "unsupported required registry behavior {behavior:?} in {field}"
        ));
    }

    Ok(())
}

fn validate_globs(package_id: &PackageId, field: &str, patterns: &[String]) -> Result<()> {
    for pattern in patterns {
        if pattern.trim().is_empty() {
            return registry_invalid(format!(
                "registry package {} has an empty {field} glob",
                package_id.as_str()
            ));
        }

        Glob::new(pattern).map_err(|source| FontbrewError::RegistryValidationFailed {
            message: format!(
                "registry package {} has invalid {field} glob {pattern:?}: {source}",
                package_id.as_str()
            ),
        })?;
    }

    Ok(())
}

fn registry_validation(source: impl std::error::Error) -> FontbrewError {
    FontbrewError::RegistryValidationFailed {
        message: source.to_string(),
    }
}

fn registry_invalid<T>(message: impl Into<String>) -> Result<T> {
    Err(FontbrewError::RegistryValidationFailed {
        message: message.into(),
    })
}

fn write_lock_path(paths: &FontbrewPaths) -> PathBuf {
    paths.managed_store_dir().join(".fontbrew.lock")
}

fn format_system_time(time: SystemTime) -> Option<String> {
    let seconds = time.duration_since(UNIX_EPOCH).ok()?.as_secs();
    Some(format!("unix:{seconds}"))
}
