use std::fs;
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;

pub use crate::activation::ActivationStrategy;
use crate::error::{FontbrewError, Result};
use crate::model::FontFormat;

const CURRENT_SCHEMA_VERSION: u32 = 1;
const DEFAULT_METADATA_TTL_HOURS: u64 = 24;
const DEFAULT_UPDATE_CONCURRENCY: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontbrewConfig {
    pub schema_version: u32,
    pub format_preference: Vec<FontFormat>,
    pub activation_strategy: ActivationStrategy,
    pub registry_auto_update: bool,
    pub metadata_ttl: Duration,
    pub update_concurrency: usize,
}

impl FontbrewConfig {
    pub fn load(path: &Path) -> Result<Self> {
        match fs::read_to_string(path) {
            Ok(content) => Self::parse(&content),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error.into()),
        }
    }

    pub fn parse(content: &str) -> Result<Self> {
        let raw: RawConfig = toml::from_str(content).map_err(|error| FontbrewError::Config {
            message: error.to_string(),
        })?;
        raw.into_config()
    }
}

impl Default for FontbrewConfig {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            format_preference: default_format_preference(),
            activation_strategy: ActivationStrategy::Symlink,
            registry_auto_update: true,
            metadata_ttl: Duration::from_secs(DEFAULT_METADATA_TTL_HOURS * 60 * 60),
            update_concurrency: DEFAULT_UPDATE_CONCURRENCY,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    schema_version: Option<u32>,
    install: Option<RawInstallConfig>,
    registry: Option<RawRegistryConfig>,
    network: Option<RawNetworkConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawInstallConfig {
    format_preference: Option<Vec<RawFontFormat>>,
    activation_strategy: Option<RawActivationStrategy>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRegistryConfig {
    auto_update: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawNetworkConfig {
    metadata_ttl_hours: Option<u64>,
    update_concurrency: Option<usize>,
}

impl RawConfig {
    fn into_config(self) -> Result<FontbrewConfig> {
        let schema_version = self.schema_version.ok_or_else(|| FontbrewError::Config {
            message: "missing required schema_version".to_string(),
        })?;

        if schema_version != CURRENT_SCHEMA_VERSION {
            return Err(FontbrewError::Config {
                message: format!(
                    "unsupported config schema_version {schema_version}; expected {CURRENT_SCHEMA_VERSION}"
                ),
            });
        }

        let install = self.install;
        let registry = self.registry;
        let network = self.network;

        Ok(FontbrewConfig {
            schema_version,
            format_preference: install
                .as_ref()
                .and_then(|install| install.format_preference.clone())
                .map(|formats| {
                    formats
                        .into_iter()
                        .map(RawFontFormat::into_font_format)
                        .collect()
                })
                .unwrap_or_else(default_format_preference),
            activation_strategy: install
                .and_then(|install| install.activation_strategy)
                .map(RawActivationStrategy::into_activation_strategy)
                .unwrap_or(ActivationStrategy::Symlink),
            registry_auto_update: registry
                .and_then(|registry| registry.auto_update)
                .unwrap_or(true),
            metadata_ttl: Duration::from_secs(
                network
                    .as_ref()
                    .and_then(|network| network.metadata_ttl_hours)
                    .unwrap_or(DEFAULT_METADATA_TTL_HOURS)
                    * 60
                    * 60,
            ),
            update_concurrency: network
                .and_then(|network| network.update_concurrency)
                .unwrap_or(DEFAULT_UPDATE_CONCURRENCY),
        })
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "lowercase")]
enum RawFontFormat {
    Otf,
    Ttf,
    Ttc,
    Otc,
}

impl RawFontFormat {
    fn into_font_format(self) -> FontFormat {
        match self {
            Self::Otf => FontFormat::Otf,
            Self::Ttf => FontFormat::Ttf,
            Self::Ttc => FontFormat::Ttc,
            Self::Otc => FontFormat::Otc,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum RawActivationStrategy {
    Symlink,
    Copy,
}

impl RawActivationStrategy {
    fn into_activation_strategy(self) -> ActivationStrategy {
        match self {
            Self::Symlink => ActivationStrategy::Symlink,
            Self::Copy => ActivationStrategy::Copy,
        }
    }
}

fn default_format_preference() -> Vec<FontFormat> {
    vec![
        FontFormat::Otf,
        FontFormat::Ttf,
        FontFormat::Ttc,
        FontFormat::Otc,
    ]
}
