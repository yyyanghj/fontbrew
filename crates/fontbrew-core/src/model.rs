use std::path::PathBuf;

use crate::activation::{ActivationArtifact, ActivationStrategy};
use crate::error::{FontbrewError, Result};
use serde::{de::Error as _, Deserialize, Deserializer, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct PackageId(String);

impl PackageId {
    pub fn parse(id: impl AsRef<str>) -> Result<Self> {
        let input = id.as_ref();

        validate_package_id(input)?;

        Ok(Self(input.to_string()))
    }

    pub fn normalize(display_name: impl AsRef<str>) -> Result<Self> {
        let input = display_name.as_ref();
        let mut slug = String::new();
        let mut previous_was_separator = false;

        if input.is_empty() {
            return invalid_package_id(input, "package id cannot be empty");
        }

        for character in input.chars() {
            if character.is_ascii_alphanumeric() {
                slug.push(character.to_ascii_lowercase());
                previous_was_separator = false;
                continue;
            }

            if character.is_ascii_whitespace() || character == '-' {
                if slug.is_empty() || previous_was_separator {
                    return invalid_package_id(input, "package id contains an empty component");
                }

                slug.push('-');
                previous_was_separator = true;
                continue;
            }

            return invalid_package_id(input, "package id contains an unsafe character");
        }

        validate_package_id(&slug)?;

        Ok(Self(slug))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for PackageId {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let id = String::deserialize(deserializer)?;

        Self::parse(&id).map_err(D::Error::custom)
    }
}

fn validate_package_id(input: &str) -> Result<()> {
    if input.is_empty() {
        return invalid_package_id(input, "package id cannot be empty");
    }

    if !input.is_ascii() {
        return invalid_package_id(input, "package id must be ASCII");
    }

    let bytes = input.as_bytes();
    if !is_ascii_lowercase_alnum(bytes[0]) || !is_ascii_lowercase_alnum(bytes[bytes.len() - 1]) {
        return invalid_package_id(
            input,
            "package id must start and end with a lowercase letter or digit",
        );
    }

    let mut previous_was_hyphen = false;
    for byte in bytes {
        match *byte {
            b'a'..=b'z' | b'0'..=b'9' => previous_was_hyphen = false,
            b'-' if !previous_was_hyphen => previous_was_hyphen = true,
            b'-' => return invalid_package_id(input, "package id contains an empty component"),
            b'A'..=b'Z' => {
                return invalid_package_id(input, "package id must be lowercase");
            }
            _ => return invalid_package_id(input, "package id contains an unsafe character"),
        }
    }

    Ok(())
}

fn is_ascii_lowercase_alnum(byte: u8) -> bool {
    byte.is_ascii_lowercase() || byte.is_ascii_digit()
}

fn invalid_package_id<T>(input: &str, reason: &str) -> Result<T> {
    Err(FontbrewError::InvalidPackageId {
        input: input.to_string(),
        reason: reason.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use crate::PackageId;

    fn package_id(id: &str) -> PackageId {
        PackageId::parse(id).expect("test package id should be valid")
    }

    #[test]
    fn package_id_parse_accepts_lowercase_ascii_kebab_case() {
        let id = PackageId::parse("source-sans-3").expect("valid package id");

        assert_eq!(id.as_str(), "source-sans-3");
    }

    #[test]
    fn package_id_parse_rejects_unsafe_ids() {
        for input in [
            "",
            "Inter",
            "inter/",
            "inter\\mono",
            "inter.mono",
            "inter_mono",
            "-inter",
            "inter-",
            "inter--mono",
            "inter-新",
            "inter mono",
        ] {
            assert!(
                PackageId::parse(input).is_err(),
                "{input:?} should be rejected"
            );
        }
    }

    #[test]
    fn package_id_normalize_converts_display_names_to_slugs() {
        for (input, expected) in [
            ("Inter", "inter"),
            ("JetBrains Mono", "jetbrains-mono"),
            ("Source Sans 3", "source-sans-3"),
            ("Maple Mono NF CN", "maple-mono-nf-cn"),
        ] {
            let id = PackageId::normalize(input).expect("display name should normalize");

            assert_eq!(id.as_str(), expected);
        }
    }

    #[test]
    fn package_id_normalize_rejects_unsafe_display_names() {
        for input in ["", "Inter/Mono", "Inter_Mono", "Inter..Mono", "字体"] {
            assert!(
                PackageId::normalize(input).is_err(),
                "{input:?} should be rejected"
            );
        }
    }

    #[test]
    fn package_id_deserialize_rejects_unsafe_ids() {
        let error = serde_json::from_str::<PackageId>("\"Inter/Mono\"")
            .expect_err("invalid package id should not deserialize");

        assert!(error.to_string().contains("invalid package id"));
    }

    #[test]
    fn package_id_deserialize_accepts_valid_ids() {
        let id: PackageId =
            serde_json::from_str("\"jetbrains-mono\"").expect("valid package id should parse");

        assert_eq!(id, package_id("jetbrains-mono"));
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct PackageVersion(String);

impl PackageVersion {
    pub fn new(version: impl Into<String>) -> Self {
        Self(version.into())
    }

    pub fn parse(version: impl AsRef<str>) -> Result<Self> {
        let version = version.as_ref();
        validate_package_version_path_segment(version)?;
        Ok(Self(version.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for PackageVersion {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let version = String::deserialize(deserializer)?;

        validate_package_version_path_segment(&version).map_err(D::Error::custom)?;

        Ok(Self(version))
    }
}

fn validate_package_version_path_segment(version: &str) -> Result<()> {
    if version.is_empty() {
        return Err(FontbrewError::Manifest {
            message: "package version cannot be empty".to_string(),
        });
    }

    if version == "." || version == ".." {
        return Err(FontbrewError::Manifest {
            message: format!("package version is not a safe path segment: {version:?}"),
        });
    }

    if version.contains('/') || version.contains('\\') || version.contains('\0') {
        return Err(FontbrewError::Manifest {
            message: format!("package version contains an unsafe path separator: {version:?}"),
        });
    }

    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FamilyName(String);

impl FamilyName {
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OperationId(String);

impl OperationId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProviderKind {
    Fontsource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InstallSource {
    Provider { provider: ProviderKind, id: String },
    GitHubRepo { owner: String, repo: String },
    LocalPath(PathBuf),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum FontFormat {
    Otf,
    Ttf,
    Ttc,
    Otc,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallRequest {
    pub source: InstallSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_id_override: Option<PackageId>,
    pub format_preference: Vec<FontFormat>,
    pub asset_selector: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub selected_families: Vec<FamilyName>,
    pub reinstall: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoveRequest {
    pub package_id: PackageId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InfoRequest {
    pub package_id: PackageId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutdatedRequest {
    pub package_ids: Vec<PackageId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateRequest {
    pub package_ids: Vec<PackageId>,
    pub jobs: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchRequest {
    pub query: String,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigGetRequest {
    pub key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigSetRequest {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigReport {
    pub key: String,
    pub value: ConfigValue,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ConfigValue {
    List(Vec<String>),
    String(String),
    Bool(bool),
    Integer(u64),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutionPolicy {
    SafeOnly,
    AllowUserApprovedRisk,
    AssumeYes,
    DryRun,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlanRisk {
    Conflict {
        package_id: PackageId,
        description: String,
    },
    AmbiguousAsset {
        package_id: PackageId,
        description: String,
    },
    UnmanagedFontOverlap {
        family_name: FamilyName,
        description: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedChange {
    pub package_id: PackageId,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProgressEvent {
    ResolvingSource {
        source: String,
    },
    DownloadStarted {
        package_id: PackageId,
        bytes: Option<u64>,
    },
    DownloadProgress {
        package_id: PackageId,
        downloaded: u64,
        total: Option<u64>,
    },
    ExtractingArchive {
        package_id: PackageId,
    },
    ParsingFonts {
        package_id: PackageId,
    },
    CheckingInstallRisks {
        package_id: PackageId,
    },
    PreparingUpdate {
        package_id: PackageId,
    },
    ApplyingUpdate {
        package_id: PackageId,
    },
    FinishedPackage {
        package_id: PackageId,
    },
}

pub trait ProgressSink {
    fn emit(&mut self, event: ProgressEvent);
}

#[derive(Debug, Default, Clone, Copy)]
pub struct NoProgress;

impl ProgressSink for NoProgress {
    fn emit(&mut self, _event: ProgressEvent) {}
}

pub trait CancellationToken: Send + Sync {
    fn is_cancelled(&self) -> bool;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct NoCancellation;

impl CancellationToken for NoCancellation {
    fn is_cancelled(&self) -> bool {
        false
    }
}

pub(crate) fn ensure_not_cancelled(cancellation: &dyn CancellationToken) -> Result<()> {
    if cancellation.is_cancelled() {
        return Err(FontbrewError::Cancelled);
    }

    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallPlan {
    pub package_id: PackageId,
    pub target_version: Option<PackageVersion>,
    pub changes: Vec<PlannedChange>,
    pub risks: Vec<PlanRisk>,
    pub already_installed: bool,
    #[serde(skip)]
    pub(crate) prepared: Option<PreparedInstallPackage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedInstallPackage {
    pub package_id: PackageId,
    pub version: PackageVersion,
    pub source: PreparedInstallSource,
    pub families: Vec<FamilyName>,
    pub font_files: Vec<PreparedFontFile>,
    pub activation_dir: PathBuf,
    pub activation_strategy: ActivationStrategy,
    pub activation_artifacts: Vec<ActivationArtifact>,
    pub activation_risks: Vec<PlanRisk>,
    pub staging_dir: PathBuf,
    pub files_dir: PathBuf,
    pub package_store_dir: PathBuf,
    pub reinstall: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PreparedInstallSource {
    LocalArchive { path: PathBuf },
    GitHub { owner: String, repo: String },
    Provider { provider: ProviderKind, id: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedFontFile {
    pub staging_path: PathBuf,
    pub stored_path: PathBuf,
    pub faces: Vec<PreparedFontFace>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedFontFace {
    pub family: FamilyName,
    pub style: String,
    pub weight: u16,
    pub format: FontFormat,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallReport {
    pub package_id: PackageId,
    pub installed_version: PackageVersion,
    pub families: Vec<FamilyName>,
    pub installed: bool,
    pub already_installed: bool,
    pub activated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallBatchReport {
    pub packages: Vec<InstallReport>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemovePlan {
    pub package_id: PackageId,
    pub changes: Vec<PlannedChange>,
    pub risks: Vec<PlanRisk>,
    pub font_files: Vec<ManagedFontFile>,
    pub activation_artifacts: Vec<ManagedActivationArtifact>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoveReport {
    pub package_id: PackageId,
    pub removed: bool,
    pub planned: bool,
    pub font_files: Vec<ManagedFontFile>,
    pub activation_artifacts: Vec<ManagedActivationArtifact>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListReport {
    pub packages: Vec<ListPackage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListPackage {
    pub package_id: PackageId,
    pub version: PackageVersion,
    pub families: Vec<FamilyName>,
    pub activated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageInfo {
    pub package_id: PackageId,
    pub version: PackageVersion,
    pub families: Vec<FamilyName>,
    pub source: String,
    pub activated: bool,
    pub update_source: Option<String>,
    pub managed: bool,
    pub update_available: Option<bool>,
    pub font_files: Vec<ManagedFontFile>,
    pub activation_artifacts: Vec<ManagedActivationArtifact>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InfoReport {
    pub package: PackageInfo,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedFontFile {
    pub path: PathBuf,
    pub family: FamilyName,
    pub style: String,
    pub weight: u16,
    pub format: FontFormat,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedActivationArtifact {
    pub path: PathBuf,
    pub source_path: PathBuf,
    pub strategy: ActivationStrategy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutdatedReport {
    pub packages: Vec<OutdatedPackage>,
    pub not_updatable: Vec<NotUpdatablePackage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutdatedPackage {
    pub package_id: PackageId,
    pub current_version: PackageVersion,
    pub latest_version: PackageVersion,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotUpdatablePackage {
    pub package_id: PackageId,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdatePlan {
    pub operation_id: OperationId,
    pub changes: Vec<PlannedChange>,
    pub risks: Vec<PlanRisk>,
    pub prepared: Vec<UpdatePlanPackage>,
    pub failed: Vec<UpdatePlanFailure>,
    #[serde(skip)]
    pub(crate) prepared_packages: Vec<PreparedUpdatePackage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdatePlanPackage {
    pub package_id: PackageId,
    pub current_version: PackageVersion,
    pub target_version: PackageVersion,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdatePlanFailure {
    pub package_id: PackageId,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedUpdatePackage {
    pub summary: UpdatePlanPackage,
    pub prepared: PreparedInstallPackage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateReport {
    pub operation_id: OperationId,
    pub planned: Vec<UpdatePlanPackage>,
    pub updated: Vec<UpdatedPackage>,
    pub skipped: Vec<UpdatePlanFailure>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdatedPackage {
    pub package_id: PackageId,
    pub previous_version: PackageVersion,
    pub installed_version: PackageVersion,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchReport {
    pub results: Vec<SearchResult>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchResult {
    pub package_id: PackageId,
    pub display_name: String,
    pub source: String,
    pub version: Option<PackageVersion>,
    pub families: Vec<FamilyName>,
}
