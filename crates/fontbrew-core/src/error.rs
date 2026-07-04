use std::path::PathBuf;

use crate::model::{FamilyName, PackageId};

pub type Result<T> = std::result::Result<T, FontbrewError>;

#[derive(Debug, thiserror::Error)]
pub enum FontbrewError {
    #[error("package is already installed: {package_id:?}")]
    PackageAlreadyInstalled { package_id: PackageId },

    #[error("multiple installable assets matched for {package_id:?}: {assets:?}")]
    AmbiguousAssets {
        package_id: PackageId,
        assets: Vec<String>,
    },

    #[error("conflict for {package_id:?}: {message}")]
    Conflict {
        package_id: PackageId,
        message: String,
    },

    #[error("execution policy required for risk: {risk}")]
    ExecutionPolicyRequired { risk: String },

    #[error("package has no update source: {package_id:?}")]
    NoUpdateSource { package_id: PackageId },

    #[error(
        "package identity mismatch for {package_id:?}: expected {expected:?}, found {found:?}"
    )]
    PackageIdentityMismatch {
        package_id: PackageId,
        expected: FamilyName,
        found: FamilyName,
    },

    #[error("operation cancelled")]
    Cancelled,

    #[error("archive rejected: {reason}")]
    ArchiveRejected { reason: String },

    #[error("registry validation failed: {message}")]
    RegistryValidationFailed { message: String },

    #[error("invalid package id {input:?}: {reason}")]
    InvalidPackageId { input: String, reason: String },

    #[error("configuration error: {message}")]
    Config { message: String },

    #[error("path resolution error: {message}")]
    PathResolution { message: String },

    #[error("manifest error: {message}")]
    Manifest { message: String },

    #[error("unsupported manifest schema version: found {found:?}, supported {supported}")]
    ManifestSchema { found: Option<u64>, supported: u64 },

    #[error("could not acquire lock at {path:?}: {source}")]
    Lock {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("network error: {message}")]
    Network { message: String },

    #[error("font parse error: {message}")]
    FontParse { message: String },

    #[error("{operation} is not implemented yet")]
    NotImplemented { operation: &'static str },
}
