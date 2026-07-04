use std::io;

use fontbrew_core::{FontbrewError, PlanRisk};

pub const SUCCESS: u8 = 0;
pub const FAILURE: u8 = 1;

pub type CliResult<T> = Result<T, CliError>;

#[derive(Debug)]
pub enum CliError {
    Core(FontbrewError),
    Io(io::Error),
    Json(serde_json::Error),
    ApprovalRequired { risks: Vec<PlanRisk> },
    PromptUnavailable { risks: Vec<PlanRisk> },
    Cancelled,
    Usage { message: String },
}

impl CliError {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Core(error) => core_error_kind(error),
            Self::Io(_) => "io",
            Self::Json(_) => "json",
            Self::ApprovalRequired { .. } => "approval_required",
            Self::PromptUnavailable { .. } => "prompt_unavailable",
            Self::Cancelled => "cancelled",
            Self::Usage { .. } => "usage",
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::Core(error) => error.to_string(),
            Self::Io(error) => error.to_string(),
            Self::Json(error) => error.to_string(),
            Self::ApprovalRequired { risks } => approval_message(risks),
            Self::PromptUnavailable { risks } => format!(
                "{}; rerun with --yes or --dry-run, or use an interactive terminal",
                approval_message(risks)
            ),
            Self::Cancelled => "operation cancelled".to_string(),
            Self::Usage { message } => message.clone(),
        }
    }

    pub fn risks(&self) -> Option<&[PlanRisk]> {
        match self {
            Self::ApprovalRequired { risks } | Self::PromptUnavailable { risks } => Some(risks),
            _ => None,
        }
    }

    pub fn exit_code(&self) -> u8 {
        FAILURE
    }
}

impl From<FontbrewError> for CliError {
    fn from(error: FontbrewError) -> Self {
        Self::Core(error)
    }
}

impl From<io::Error> for CliError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for CliError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

fn approval_message(risks: &[PlanRisk]) -> String {
    let suffix = if risks.len() == 1 { "" } else { "s" };
    format!(
        "approval is required before applying {} plan risk{}",
        risks.len(),
        suffix
    )
}

fn core_error_kind(error: &FontbrewError) -> &'static str {
    match error {
        FontbrewError::PackageAlreadyInstalled { .. } => "package_already_installed",
        FontbrewError::AmbiguousAssets { .. } => "ambiguous_assets",
        FontbrewError::Conflict { .. } => "conflict",
        FontbrewError::ExecutionPolicyRequired { .. } => "execution_policy_required",
        FontbrewError::NoUpdateSource { .. } => "no_update_source",
        FontbrewError::PackageIdentityMismatch { .. } => "package_identity_mismatch",
        FontbrewError::Cancelled => "cancelled",
        FontbrewError::ArchiveRejected { .. } => "archive_rejected",
        FontbrewError::RegistryValidationFailed { .. } => "registry_validation_failed",
        FontbrewError::InvalidPackageId { .. } => "invalid_package_id",
        FontbrewError::Config { .. } => "config",
        FontbrewError::PathResolution { .. } => "path_resolution",
        FontbrewError::Manifest { .. } => "manifest",
        FontbrewError::ManifestSchema { .. } => "manifest_schema",
        FontbrewError::Lock { .. } => "lock",
        FontbrewError::Io(_) => "io",
        FontbrewError::Network { .. } => "network",
        FontbrewError::FontParse { .. } => "font_parse",
        FontbrewError::NotImplemented { .. } => "not_implemented",
    }
}
