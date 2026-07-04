use std::path::{Path, PathBuf};

use directories::BaseDirs;

use crate::error::{FontbrewError, Result};
use crate::model::{PackageId, PackageVersion};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontbrewPaths {
    data_root: PathBuf,
    config_root: PathBuf,
    home_root: PathBuf,
}

impl FontbrewPaths {
    pub fn resolve() -> Result<Self> {
        let base_dirs = BaseDirs::new().ok_or_else(|| FontbrewError::PathResolution {
            message: "could not resolve user home directory".to_string(),
        })?;
        Ok(Self {
            data_root: base_dirs.home_dir().join(".local/share/fontbrew"),
            config_root: base_dirs.home_dir().join(".config/fontbrew"),
            home_root: base_dirs.home_dir().to_path_buf(),
        })
    }

    pub fn for_tests(
        data_root: impl Into<PathBuf>,
        config_root: impl Into<PathBuf>,
        home_root: impl Into<PathBuf>,
    ) -> Self {
        Self {
            data_root: data_root.into(),
            config_root: config_root.into(),
            home_root: home_root.into(),
        }
    }

    pub fn managed_store_dir(&self) -> PathBuf {
        self.data_root.clone()
    }

    pub fn package_store_dir(&self, package_id: &PackageId, version: &PackageVersion) -> PathBuf {
        self.data_root
            .join("packages")
            .join(package_id.as_str())
            .join(version.as_str())
    }

    pub fn manifest_path(&self) -> PathBuf {
        self.data_root.join("manifest.json")
    }

    pub fn provider_metadata_dir(&self) -> PathBuf {
        self.data_root.join("providers")
    }

    pub fn config_path(&self) -> PathBuf {
        config_path_from_root(&self.config_root)
    }

    pub fn staging_dir(&self) -> PathBuf {
        self.data_root.join("staging")
    }

    pub fn activation_dir(&self) -> PathBuf {
        self.home_root.join("Library/Fonts/Fontbrew")
    }
}

fn config_path_from_root(config_root: &Path) -> PathBuf {
    if config_root
        .file_name()
        .is_some_and(|name| name == "fontbrew")
    {
        config_root.join("config.toml")
    } else {
        config_root.join("fontbrew/config.toml")
    }
}
