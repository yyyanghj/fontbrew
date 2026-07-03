use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{
    error::{FontbrewError, Result},
    fs::write_atomically,
    FamilyName, PackageId, PackageVersion, ProviderKind,
};

const MANIFEST_SCHEMA_VERSION: u64 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestV1 {
    pub schema_version: u64,
    pub packages: BTreeMap<PackageId, ManifestPackageRecord>,
}

impl ManifestV1 {
    pub fn empty() -> Self {
        Self {
            schema_version: MANIFEST_SCHEMA_VERSION,
            packages: BTreeMap::new(),
        }
    }

    pub fn insert_package(&mut self, record: ManifestPackageRecord) -> Result<()> {
        let package_id = record.package_id.clone();
        self.packages.insert(package_id, record);
        Ok(())
    }

    pub fn remove_package(&mut self, package_id: &PackageId) -> Option<ManifestPackageRecord> {
        self.packages.remove(package_id)
    }

    pub fn get_package(&self, package_id: &PackageId) -> Option<&ManifestPackageRecord> {
        self.packages.get(package_id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestPackageRecord {
    pub package_id: PackageId,
    pub version: PackageVersion,
    pub source: ManifestSource,
    pub update_source: Option<ManifestSource>,
    pub families: Vec<FamilyName>,
    pub font_files: Vec<ManifestFontFileRecord>,
    pub activation_artifacts: Vec<PathBuf>,
    pub installed_at: String,
    pub active_version: Option<PackageVersion>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ManifestSource {
    Registry { id: String },
    GitHub { owner: String, repo: String },
    Provider { provider: ProviderKind, id: String },
    LocalArchive { path: PathBuf },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestFontFileRecord {
    pub path: PathBuf,
    pub family: FamilyName,
    pub style: String,
    pub weight: u16,
    pub format: ManifestFontFileFormat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ManifestFontFileFormat {
    Ttf,
    Otf,
    Ttc,
    Otc,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct ManifestStore;

impl ManifestStore {
    pub fn read_or_empty(path: &Path) -> Result<ManifestV1> {
        let content = match fs::read_to_string(path) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(ManifestV1::empty());
            }
            Err(error) => return Err(error.into()),
        };

        validate_schema_version(&content)?;

        let manifest: ManifestV1 =
            serde_json::from_str(&content).map_err(|source| FontbrewError::Manifest {
                message: format!("could not parse manifest at {}: {source}", path.display()),
            })?;

        validate_package_keys(&manifest)?;

        Ok(manifest)
    }

    pub fn write(path: &Path, manifest: &ManifestV1) -> Result<()> {
        validate_manifest(manifest)?;

        let content =
            serde_json::to_vec_pretty(manifest).map_err(|source| FontbrewError::Manifest {
                message: format!("could not serialize manifest: {source}"),
            })?;

        write_atomically(path, &content)
    }
}

fn validate_manifest(manifest: &ManifestV1) -> Result<()> {
    if manifest.schema_version != MANIFEST_SCHEMA_VERSION {
        return Err(FontbrewError::ManifestSchema {
            found: Some(manifest.schema_version),
            supported: MANIFEST_SCHEMA_VERSION,
        });
    }

    validate_package_keys(manifest)
}

fn validate_package_keys(manifest: &ManifestV1) -> Result<()> {
    for (package_id, record) in &manifest.packages {
        if package_id != &record.package_id {
            return Err(FontbrewError::Manifest {
                message: format!(
                    "manifest package key {:?} does not match record package id {:?}",
                    package_id, record.package_id
                ),
            });
        }
    }

    Ok(())
}

fn validate_schema_version(content: &str) -> Result<()> {
    let value: serde_json::Value =
        serde_json::from_str(content).map_err(|source| FontbrewError::Manifest {
            message: format!("could not parse manifest JSON: {source}"),
        })?;

    let found = value
        .get("schemaVersion")
        .and_then(serde_json::Value::as_u64);

    match found {
        Some(MANIFEST_SCHEMA_VERSION) => Ok(()),
        _ => Err(FontbrewError::ManifestSchema {
            found,
            supported: MANIFEST_SCHEMA_VERSION,
        }),
    }
}
