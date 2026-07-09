use std::path::{Path, PathBuf};

use directories::BaseDirs;

use crate::error::{FontbrewError, Result};
use crate::model::{PackageId, PackageVersion};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontbrewPaths {
    data_root: PathBuf,
    config_path: PathBuf,
    activation_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DefaultFontbrewLocations {
    pub(crate) store_dir: PathBuf,
    pub(crate) config_path: PathBuf,
    pub(crate) activation_dir: PathBuf,
}

impl FontbrewPaths {
    pub fn resolve() -> Result<Self> {
        let defaults = Self::default_locations()?;
        Ok(Self {
            data_root: defaults.store_dir,
            config_path: defaults.config_path,
            activation_dir: defaults.activation_dir,
        })
    }

    pub(crate) fn default_locations() -> Result<DefaultFontbrewLocations> {
        let base_dirs = BaseDirs::new().ok_or_else(|| FontbrewError::PathResolution {
            message: "could not resolve user home directory".to_string(),
        })?;
        let home_root = base_dirs.home_dir();
        Ok(DefaultFontbrewLocations {
            store_dir: home_root.join(".local/share/fontbrew"),
            config_path: config_path_from_root(&home_root.join(".config/fontbrew")),
            activation_dir: home_root.join("Library/Fonts/Fontbrew"),
        })
    }

    pub fn from_locations(
        data_root: impl Into<PathBuf>,
        config_path: impl Into<PathBuf>,
        activation_dir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            data_root: data_root.into(),
            config_path: config_path.into(),
            activation_dir: activation_dir.into(),
        }
    }

    pub fn for_tests(
        data_root: impl Into<PathBuf>,
        config_root: impl Into<PathBuf>,
        home_root: impl Into<PathBuf>,
    ) -> Self {
        let config_root = config_root.into();
        let home_root = home_root.into();
        Self {
            data_root: data_root.into(),
            config_path: config_path_from_root(&config_root),
            activation_dir: home_root.join("Library/Fonts/Fontbrew"),
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
        self.config_path.clone()
    }

    pub fn staging_dir(&self) -> PathBuf {
        self.data_root.join("staging")
    }

    pub fn activation_dir(&self) -> PathBuf {
        self.activation_dir.clone()
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
