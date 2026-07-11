use std::io;

use fontbrew_core::{FamilyName, FontbrewError, PlanRisk};

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
    AssetSelectionRequired { source: String, assets: Vec<String> },
    UpdateAssetSelectionRequired { source: String, assets: Vec<String> },
    FamilySelectionRequired { families: Vec<FamilyName> },
    SelfUpdateApprovalRequired { message: String },
    SelfUpdatePromptUnavailable { message: String },
    SelfUpdateUnavailable { message: String },
    SelfUpdateInvalidRelease { message: String },
    SelfUpdateChecksumMismatch { message: String },
    SelfUpdateFailed { message: String },
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
            Self::AssetSelectionRequired { .. } | Self::UpdateAssetSelectionRequired { .. } => {
                "ambiguous_assets"
            }
            Self::FamilySelectionRequired { .. } => "family_selection_required",
            Self::SelfUpdateApprovalRequired { .. } => "approval_required",
            Self::SelfUpdatePromptUnavailable { .. } => "prompt_unavailable",
            Self::SelfUpdateUnavailable { .. } => "self_update_unavailable",
            Self::SelfUpdateInvalidRelease { .. } => "self_update_invalid_release",
            Self::SelfUpdateChecksumMismatch { .. } => "self_update_checksum_mismatch",
            Self::SelfUpdateFailed { .. } => "self_update_failed",
            Self::Cancelled => "cancelled",
            Self::Usage { .. } => "usage",
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::Core(FontbrewError::FamilySelectionRequired { families }) => {
                family_selection_message(families)
            }
            Self::Core(FontbrewError::AmbiguousAssets {
                source_label,
                assets,
            }) => ambiguous_assets_message(source_label, assets),
            Self::Core(error) => error.to_string(),
            Self::Io(error) => error.to_string(),
            Self::Json(error) => error.to_string(),
            Self::ApprovalRequired { risks } => approval_message(risks),
            Self::PromptUnavailable { risks } => format!(
                "{}; rerun with --yes or --dry-run, or use an interactive terminal",
                approval_message(risks)
            ),
            Self::AssetSelectionRequired { source, assets } => {
                ambiguous_assets_message(source, assets)
            }
            Self::UpdateAssetSelectionRequired { source, assets } => {
                update_ambiguous_assets_message(source, assets)
            }
            Self::FamilySelectionRequired { families } => family_selection_message(families),
            Self::SelfUpdateApprovalRequired { message }
            | Self::SelfUpdatePromptUnavailable { message }
            | Self::SelfUpdateUnavailable { message }
            | Self::SelfUpdateInvalidRelease { message }
            | Self::SelfUpdateChecksumMismatch { message }
            | Self::SelfUpdateFailed { message } => message.clone(),
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

    pub fn families(&self) -> Option<&[FamilyName]> {
        match self {
            Self::FamilySelectionRequired { families } => Some(families),
            Self::Core(FontbrewError::FamilySelectionRequired { families }) => Some(families),
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
        FontbrewError::FamilySelectionRequired { .. } => "family_selection_required",
        FontbrewError::InvalidSource { .. } => "invalid_source",
        FontbrewError::InvalidPackageId { .. } => "invalid_package_id",
        FontbrewError::Config { .. } => "config",
        FontbrewError::PathResolution { .. } => "path_resolution",
        FontbrewError::Manifest { .. } => "manifest",
        FontbrewError::CommittedCleanup { .. } => "committed_cleanup",
        FontbrewError::CommitUncertain { .. } => "commit_uncertain",
        FontbrewError::ManifestSchema { .. } => "manifest_schema",
        FontbrewError::Lock { .. } => "lock",
        FontbrewError::Io(_) => "io",
        FontbrewError::Network { .. } => "network",
        FontbrewError::FontParse { .. } => "font_parse",
        FontbrewError::NotImplemented { .. } => "not_implemented",
    }
}

fn family_selection_message(families: &[FamilyName]) -> String {
    let family_list = families
        .iter()
        .map(|family| family.as_str())
        .collect::<Vec<_>>()
        .join(", ");

    format!(
        "font family selection is required; select one or more with --family, or install all discovered families with --all: {family_list}"
    )
}

fn ambiguous_assets_message(source: &str, assets: &[String]) -> String {
    let mut message = format!(
        "multiple release assets matched for {}; choose one with --asset <name-or-glob>",
        source
    );

    if !assets.is_empty() {
        message.push_str(". Matching assets: ");
        message.push_str(&assets.join(", "));
        message.push_str(". Example: --asset \"");
        message.push_str(&assets[0]);
        message.push('"');
    }

    message
}

fn update_ambiguous_assets_message(source: &str, assets: &[String]) -> String {
    let mut message = format!(
        "multiple release assets matched for {source}; run fontbrew update in an interactive terminal to choose one"
    );

    if !assets.is_empty() {
        message.push_str(". Matching assets: ");
        message.push_str(&assets.join(", "));
    }

    message
}

#[cfg(test)]
mod tests {
    use fontbrew_core::{FontbrewError, PackageId};

    use super::CliError;

    #[test]
    fn ambiguous_assets_error_points_to_asset_selector_without_debug_shape() {
        let error = CliError::from(FontbrewError::AmbiguousAssets {
            source_label: "github:githubnext/monaspace".to_string(),
            assets: vec![
                "monaspace-static-v1.400.zip".to_string(),
                "monaspace-variable-v1.400.zip".to_string(),
            ],
        });

        let message = error.message();

        assert_eq!(error.kind(), "ambiguous_assets");
        assert!(message.contains("--asset <name-or-glob>"));
        assert!(message.contains("--asset \"monaspace-static-v1.400.zip\""));
        assert!(message.contains("monaspace-variable-v1.400.zip"));
        assert!(!message.contains("PackageId"));
    }

    #[test]
    fn update_ambiguous_assets_error_requires_interactive_terminal() {
        let error = CliError::UpdateAssetSelectionRequired {
            source: "githubnext/monaspace (monaspace-argon)".to_string(),
            assets: vec![
                "monaspace-static-v1.400.zip".to_string(),
                "monaspace-variable-v1.400.zip".to_string(),
            ],
        };

        let message = error.message();

        assert_eq!(error.kind(), "ambiguous_assets");
        assert!(message.contains("interactive terminal"));
        assert!(message.contains("monaspace-static-v1.400.zip"));
        assert!(!message.contains("--asset"));
    }

    #[test]
    fn committed_cleanup_error_has_stable_json_kind() {
        let error = CliError::from(FontbrewError::CommittedCleanup {
            operation: "update",
            package_ids: vec![PackageId::parse("inter").expect("valid package id")],
            message: "could not remove old package store".to_string(),
        });

        assert_eq!(error.kind(), "committed_cleanup");
        assert!(error.message().contains("update committed"));
    }

    #[test]
    fn commit_uncertain_error_has_stable_json_kind() {
        let error = CliError::from(FontbrewError::CommitUncertain {
            operation: "update",
            package_ids: vec![PackageId::parse("inter").expect("valid package id")],
            message: "manifest durability could not be confirmed".to_string(),
        });

        assert_eq!(error.kind(), "commit_uncertain");
        assert!(error.message().contains("commit state is uncertain"));
    }
}
