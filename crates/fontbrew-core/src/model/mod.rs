use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PackageId(String);

impl PackageId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PackageVersion(String);

impl PackageVersion {
    pub fn new(version: impl Into<String>) -> Self {
        Self(version.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
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
    Google,
    Fontsource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InstallSource {
    RegistryName(String),
    Provider { provider: ProviderKind, id: String },
    GitHubRepo { owner: String, repo: String },
    LocalPath(PathBuf),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FontFormat {
    Otf,
    Ttf,
    Ttc,
    Otc,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallRequest {
    pub source: InstallSource,
    pub format_preference: Vec<FontFormat>,
    pub asset_selector: Option<String>,
    pub reinstall: bool,
    pub refresh: bool,
    pub offline: bool,
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchRequest {
    pub query: String,
    pub limit: Option<usize>,
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

pub trait CancellationToken {
    fn is_cancelled(&self) -> bool;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallPlan {
    pub package_id: PackageId,
    pub target_version: Option<PackageVersion>,
    pub changes: Vec<PlannedChange>,
    pub risks: Vec<PlanRisk>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallReport {
    pub package_id: PackageId,
    pub installed_version: PackageVersion,
    pub families: Vec<FamilyName>,
    pub activated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemovePlan {
    pub package_id: PackageId,
    pub changes: Vec<PlannedChange>,
    pub risks: Vec<PlanRisk>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoveReport {
    pub package_id: PackageId,
    pub removed: bool,
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InfoReport {
    pub package: PackageInfo,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateReport {
    pub operation_id: OperationId,
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
