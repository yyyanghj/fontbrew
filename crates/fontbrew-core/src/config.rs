use std::fs;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub use crate::activation::ActivationStrategy;
use crate::error::{FontbrewError, Result};
use crate::fs::write_atomically;
use crate::model::{ConfigGetRequest, ConfigReport, ConfigSetRequest, ConfigValue, FontFormat};

const CURRENT_SCHEMA_VERSION: u32 = 1;
const DEFAULT_METADATA_TTL_HOURS: u64 = 24;
const DEFAULT_UPDATE_CONCURRENCY: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontbrewConfig {
    pub schema_version: u32,
    pub format_preference: Vec<FontFormat>,
    pub activation_strategy: ActivationStrategy,
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
        Self::parse_raw(content)?.into_config()
    }

    pub(crate) fn load_with_sources(path: &Path) -> Result<LoadedFontbrewConfig> {
        match fs::read_to_string(path) {
            Ok(content) => {
                let raw = Self::parse_raw(&content)?;
                let has_format_preference = raw.has_format_preference();
                Ok(LoadedFontbrewConfig {
                    config: raw.into_config()?,
                    has_format_preference,
                })
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(LoadedFontbrewConfig {
                    config: Self::default(),
                    has_format_preference: false,
                })
            }
            Err(error) => Err(error.into()),
        }
    }

    fn parse_raw(content: &str) -> Result<RawConfig> {
        toml::from_str(content).map_err(|error| FontbrewError::Config {
            message: error.to_string(),
        })
    }

    pub(crate) fn get(path: &Path, request: ConfigGetRequest) -> Result<ConfigReport> {
        let key = ConfigKey::parse(&request.key)?;
        let config = Self::load(path)?;

        Ok(config.report(key))
    }

    pub(crate) fn set(path: &Path, request: ConfigSetRequest) -> Result<ConfigReport> {
        let key = ConfigKey::parse(&request.key)?;
        let mut config = Self::load(path)?;

        key.set_value(&mut config, &request.value)?;
        config.schema_version = CURRENT_SCHEMA_VERSION;
        let content = config.to_toml_string()?;
        write_atomically(path, content.as_bytes())?;

        Ok(config.report(key))
    }

    fn report(&self, key: ConfigKey) -> ConfigReport {
        ConfigReport {
            key: key.as_str().to_string(),
            value: key.value(self),
        }
    }

    fn to_toml_string(&self) -> Result<String> {
        toml::to_string_pretty(&PersistedConfig::from_config(self)).map_err(|error| {
            FontbrewError::Config {
                message: error.to_string(),
            }
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LoadedFontbrewConfig {
    pub(crate) config: FontbrewConfig,
    pub(crate) has_format_preference: bool,
}

impl Default for FontbrewConfig {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            format_preference: default_format_preference(),
            activation_strategy: ActivationStrategy::Symlink,
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
struct RawNetworkConfig {
    metadata_ttl_hours: Option<u64>,
    update_concurrency: Option<usize>,
}

impl RawConfig {
    fn has_format_preference(&self) -> bool {
        self.install
            .as_ref()
            .is_some_and(|install| install.format_preference.is_some())
    }

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
        let network = self.network;

        let format_preference = install
            .as_ref()
            .and_then(|install| install.format_preference.clone())
            .map(|formats| {
                validate_format_preference(dedupe_formats(
                    formats.into_iter().map(RawFontFormat::into_font_format),
                ))
            })
            .transpose()?
            .unwrap_or_else(default_format_preference);
        let metadata_ttl_hours = network
            .as_ref()
            .and_then(|network| network.metadata_ttl_hours)
            .unwrap_or(DEFAULT_METADATA_TTL_HOURS);
        let update_concurrency = network
            .as_ref()
            .and_then(|network| network.update_concurrency)
            .unwrap_or(DEFAULT_UPDATE_CONCURRENCY);

        Ok(FontbrewConfig {
            schema_version,
            format_preference,
            activation_strategy: install
                .and_then(|install| install.activation_strategy)
                .map(RawActivationStrategy::into_activation_strategy)
                .transpose()?
                .unwrap_or(ActivationStrategy::Symlink),
            metadata_ttl: metadata_ttl_from_hours(metadata_ttl_hours)?,
            update_concurrency: validate_update_concurrency(update_concurrency)?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfigKey {
    InstallFormatPreference,
    InstallActivationStrategy,
    NetworkMetadataTtlHours,
    NetworkUpdateConcurrency,
}

impl ConfigKey {
    fn parse(key: &str) -> Result<Self> {
        match key {
            "install.format_preference" => Ok(Self::InstallFormatPreference),
            "install.activation_strategy" => Ok(Self::InstallActivationStrategy),
            "network.metadata_ttl_hours" => Ok(Self::NetworkMetadataTtlHours),
            "network.update_concurrency" => Ok(Self::NetworkUpdateConcurrency),
            _ => Err(FontbrewError::Config {
                message: format!("unknown config key: {key}"),
            }),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::InstallFormatPreference => "install.format_preference",
            Self::InstallActivationStrategy => "install.activation_strategy",
            Self::NetworkMetadataTtlHours => "network.metadata_ttl_hours",
            Self::NetworkUpdateConcurrency => "network.update_concurrency",
        }
    }

    fn value(self, config: &FontbrewConfig) -> ConfigValue {
        match self {
            Self::InstallFormatPreference => ConfigValue::List(
                config
                    .format_preference
                    .iter()
                    .map(font_format_label)
                    .map(str::to_string)
                    .collect(),
            ),
            Self::InstallActivationStrategy => ConfigValue::String(
                activation_strategy_label(config.activation_strategy).to_string(),
            ),
            Self::NetworkMetadataTtlHours => {
                ConfigValue::Integer(config.metadata_ttl.as_secs() / 60 / 60)
            }
            Self::NetworkUpdateConcurrency => {
                ConfigValue::Integer(config.update_concurrency as u64)
            }
        }
    }

    fn set_value(self, config: &mut FontbrewConfig, raw_value: &str) -> Result<()> {
        match self {
            Self::InstallFormatPreference => {
                config.format_preference = parse_format_preference(raw_value)?;
            }
            Self::InstallActivationStrategy => {
                config.activation_strategy = parse_activation_strategy(raw_value)?;
            }
            Self::NetworkMetadataTtlHours => {
                let hours = parse_positive_u64(raw_value, self)?;
                config.metadata_ttl = metadata_ttl_from_hours(hours)?;
            }
            Self::NetworkUpdateConcurrency => {
                let concurrency = parse_positive_u64(raw_value, self)?;
                config.update_concurrency =
                    usize::try_from(concurrency).map_err(|_| FontbrewError::Config {
                        message: format!("value for {} is too large", self.as_str()),
                    })?;
            }
        }

        Ok(())
    }
}

#[derive(Debug, Deserialize)]
struct RawFormatPreferenceValue {
    value: Vec<RawFontFormat>,
}

fn parse_format_preference(raw_value: &str) -> Result<Vec<FontFormat>> {
    let value = raw_value.trim();
    if value.is_empty() {
        return invalid_value("install.format_preference");
    }

    let formats = if value.starts_with('[') {
        let parsed: RawFormatPreferenceValue = toml::from_str(&format!("value = {value}"))
            .map_err(|error| FontbrewError::Config {
                message: format!("invalid value for install.format_preference: {}", error),
            })?;
        parsed
            .value
            .into_iter()
            .map(RawFontFormat::into_font_format)
            .collect()
    } else {
        let mut parsed = Vec::new();
        for part in value.split(',') {
            let label = part.trim();
            if label.is_empty() {
                return invalid_value("install.format_preference");
            }
            parsed.push(parse_font_format(label)?);
        }
        parsed
    };

    validate_format_preference(formats)
}

fn parse_font_format(value: &str) -> Result<FontFormat> {
    match value.to_ascii_lowercase().as_str() {
        "otf" => Ok(FontFormat::Otf),
        "ttf" => Ok(FontFormat::Ttf),
        "ttc" => Ok(FontFormat::Ttc),
        "otc" => Ok(FontFormat::Otc),
        _ => invalid_value("install.format_preference"),
    }
}

fn parse_activation_strategy(value: &str) -> Result<ActivationStrategy> {
    match value.trim().to_ascii_lowercase().as_str() {
        "symlink" => Ok(ActivationStrategy::Symlink),
        "copy" => reserved_copy_activation_error(),
        _ => invalid_value("install.activation_strategy"),
    }
}

fn parse_positive_u64(value: &str, key: ConfigKey) -> Result<u64> {
    let parsed = value
        .trim()
        .parse::<u64>()
        .map_err(|_| FontbrewError::Config {
            message: format!("invalid value for {}", key.as_str()),
        })?;

    if parsed == 0 {
        return invalid_value(key.as_str());
    }

    Ok(parsed)
}

fn validate_format_preference(formats: Vec<FontFormat>) -> Result<Vec<FontFormat>> {
    let formats = dedupe_formats(formats);
    if formats.is_empty() {
        return invalid_value("install.format_preference");
    }

    Ok(formats)
}

fn metadata_ttl_from_hours(hours: u64) -> Result<Duration> {
    if hours == 0 {
        return invalid_value("network.metadata_ttl_hours");
    }

    let seconds = hours
        .checked_mul(60)
        .and_then(|minutes| minutes.checked_mul(60))
        .ok_or_else(|| FontbrewError::Config {
            message: "network.metadata_ttl_hours is too large".to_string(),
        })?;

    Ok(Duration::from_secs(seconds))
}

fn validate_update_concurrency(update_concurrency: usize) -> Result<usize> {
    if update_concurrency == 0 {
        return invalid_value("network.update_concurrency");
    }

    Ok(update_concurrency)
}

fn invalid_value<T>(key: &str) -> Result<T> {
    Err(FontbrewError::Config {
        message: format!("invalid value for {key}"),
    })
}

fn reserved_copy_activation_error<T>() -> Result<T> {
    Err(FontbrewError::Config {
        message: "copy activation is reserved but not supported; use install.activation_strategy = \"symlink\""
            .to_string(),
    })
}

pub(crate) fn dedupe_formats(formats: impl IntoIterator<Item = FontFormat>) -> Vec<FontFormat> {
    let mut deduped = Vec::new();

    for format in formats {
        if !deduped.contains(&format) {
            deduped.push(format);
        }
    }

    deduped
}

pub(crate) fn font_format_label(format: &FontFormat) -> &'static str {
    match format {
        FontFormat::Otf => "otf",
        FontFormat::Ttf => "ttf",
        FontFormat::Ttc => "ttc",
        FontFormat::Otc => "otc",
    }
}

fn activation_strategy_label(strategy: ActivationStrategy) -> &'static str {
    match strategy {
        ActivationStrategy::Symlink => "symlink",
        ActivationStrategy::Copy => "copy",
    }
}

#[derive(Debug, Serialize)]
struct PersistedConfig {
    schema_version: u32,
    install: PersistedInstallConfig,
    network: PersistedNetworkConfig,
}

impl PersistedConfig {
    fn from_config(config: &FontbrewConfig) -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            install: PersistedInstallConfig {
                format_preference: config
                    .format_preference
                    .iter()
                    .map(PersistedFontFormat::from_font_format)
                    .collect(),
                activation_strategy: PersistedActivationStrategy::from_activation_strategy(
                    config.activation_strategy,
                ),
            },
            network: PersistedNetworkConfig {
                metadata_ttl_hours: config.metadata_ttl.as_secs() / 60 / 60,
                update_concurrency: config.update_concurrency,
            },
        }
    }
}

#[derive(Debug, Serialize)]
struct PersistedInstallConfig {
    format_preference: Vec<PersistedFontFormat>,
    activation_strategy: PersistedActivationStrategy,
}

#[derive(Debug, Serialize)]
struct PersistedNetworkConfig {
    metadata_ttl_hours: u64,
    update_concurrency: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "lowercase")]
enum PersistedFontFormat {
    Otf,
    Ttf,
    Ttc,
    Otc,
}

impl PersistedFontFormat {
    fn from_font_format(format: &FontFormat) -> Self {
        match format {
            FontFormat::Otf => Self::Otf,
            FontFormat::Ttf => Self::Ttf,
            FontFormat::Ttc => Self::Ttc,
            FontFormat::Otc => Self::Otc,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "lowercase")]
enum PersistedActivationStrategy {
    Symlink,
    Copy,
}

impl PersistedActivationStrategy {
    fn from_activation_strategy(strategy: ActivationStrategy) -> Self {
        match strategy {
            ActivationStrategy::Symlink => Self::Symlink,
            ActivationStrategy::Copy => Self::Copy,
        }
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
    fn into_activation_strategy(self) -> Result<ActivationStrategy> {
        match self {
            Self::Symlink => Ok(ActivationStrategy::Symlink),
            Self::Copy => reserved_copy_activation_error(),
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
