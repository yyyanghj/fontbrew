use std::{
    fs::{self, File, OpenOptions},
    io,
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use serde::{Deserialize, Serialize};

use crate::{
    error::{FontbrewError, Result},
    manifest::ManifestActivationStrategy,
    ExecutionPolicy, PackageId, PlanRisk,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivationRequest {
    pub package_id: PackageId,
    pub font_files: Vec<PathBuf>,
    pub activation_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivationArtifact {
    pub package_id: PackageId,
    pub path: PathBuf,
    pub source_path: PathBuf,
    pub strategy: ManifestActivationStrategy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivationPlan {
    pub package_id: PackageId,
    pub activation_dir: PathBuf,
    pub artifacts: Vec<ActivationArtifact>,
    pub risks: Vec<PlanRisk>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArtifactPresence {
    Present,
    Missing,
}

#[derive(Debug)]
struct StagedActivationArtifact {
    original_path: PathBuf,
    backup_path: PathBuf,
}

#[derive(Debug)]
pub(crate) struct DeactivationTransaction {
    package_id: PackageId,
    backup_dir: Option<PathBuf>,
    staged_artifacts: Vec<StagedActivationArtifact>,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct ActivationPlanner;

impl ActivationPlanner {
    pub fn plan(request: ActivationRequest) -> Result<ActivationPlan> {
        let mut artifacts = Vec::with_capacity(request.font_files.len());
        let mut risks = Vec::new();

        for font_file in request.font_files {
            let file_name = font_file
                .file_name()
                .ok_or_else(|| FontbrewError::PathResolution {
                    message: format!("activation source path has no filename: {font_file:?}"),
                })?;
            let artifact_path = request.activation_dir.join(file_name);

            ensure_path_is_inside(&request.activation_dir, &artifact_path)?;

            let artifact = ActivationArtifact {
                package_id: request.package_id.clone(),
                path: artifact_path,
                source_path: font_file,
                strategy: ManifestActivationStrategy::Copy,
            };

            if has_unmanaged_conflict(&artifact)? {
                risks.push(PlanRisk::Conflict {
                    package_id: request.package_id.clone(),
                    description: format!(
                        "activation artifact already exists and is not managed by this plan: {}",
                        artifact.path.display()
                    ),
                });
            }

            artifacts.push(artifact);
        }

        Ok(ActivationPlan {
            package_id: request.package_id,
            activation_dir: request.activation_dir,
            artifacts,
            risks,
        })
    }
}

impl ActivationPlan {
    pub fn apply(&self, policy: ExecutionPolicy) -> Result<Vec<ActivationArtifact>> {
        if !self.risks.is_empty() {
            return match policy {
                ExecutionPolicy::SafeOnly | ExecutionPolicy::DryRun => {
                    Err(FontbrewError::ExecutionPolicyRequired {
                        risk: format!("{:?}", self.risks),
                    })
                }
                ExecutionPolicy::AllowUserApprovedRisk | ExecutionPolicy::AssumeYes => {
                    Err(FontbrewError::Conflict {
                        package_id: self.package_id.clone(),
                        message: "activation conflicts are not yet automatically resolved"
                            .to_string(),
                    })
                }
            };
        }

        if matches!(policy, ExecutionPolicy::DryRun) {
            return Ok(self.artifacts.clone());
        }

        reject_existing_symlink(&self.activation_dir)?;
        fs::create_dir_all(&self.activation_dir)?;

        let mut created_artifacts = Vec::new();
        for artifact in &self.artifacts {
            let result = ensure_path_is_inside(&self.activation_dir, &artifact.path)
                .and_then(|()| {
                    ensure_existing_ancestors_do_not_cross_symlinks(
                        &self.activation_dir,
                        &artifact.path,
                    )
                })
                .and_then(|()| activate_copy(artifact));

            if let Err(error) = result {
                let cleanup_error = deactivate(&self.activation_dir, &created_artifacts).err();
                return match cleanup_error {
                    Some(cleanup_error) => Err(activation_transaction_error(
                        &self.package_id,
                        "activate managed copies",
                        error,
                        Some(cleanup_error),
                        None,
                    )),
                    None => Err(error),
                };
            }

            created_artifacts.push(artifact.clone());
        }

        Ok(created_artifacts)
    }
}

pub fn deactivate(activation_dir: &Path, artifacts: &[ActivationArtifact]) -> Result<()> {
    let Some(first_artifact) = artifacts.first() else {
        return Ok(());
    };

    if artifacts
        .iter()
        .any(|artifact| artifact.package_id != first_artifact.package_id)
    {
        return Err(FontbrewError::Conflict {
            package_id: first_artifact.package_id.clone(),
            message: "activation artifacts from different packages cannot be deactivated together"
                .to_string(),
        });
    }

    let package_id = first_artifact.package_id.clone();
    deactivate_transactionally(activation_dir, &package_id, artifacts)?
        .commit()
        .map_err(|error| FontbrewError::CommittedCleanup {
            operation: "deactivation",
            package_ids: vec![package_id],
            message: format!("could not remove activation backup: {error}"),
        })
}

pub(crate) fn deactivate_transactionally(
    activation_dir: &Path,
    package_id: &PackageId,
    artifacts: &[ActivationArtifact],
) -> Result<DeactivationTransaction> {
    let mut present_artifacts = Vec::new();
    for artifact in artifacts {
        match validate_deactivation_artifact(activation_dir, artifact)? {
            ArtifactPresence::Present => present_artifacts.push(artifact),
            ArtifactPresence::Missing => {}
        }
    }

    if present_artifacts.is_empty() {
        return Ok(DeactivationTransaction::empty(package_id.clone()));
    }

    let backup_dir = create_deactivation_backup_dir(activation_dir)?;
    let mut transaction = DeactivationTransaction {
        package_id: package_id.clone(),
        backup_dir: Some(backup_dir.clone()),
        staged_artifacts: Vec::with_capacity(present_artifacts.len()),
    };

    for (index, artifact) in present_artifacts.into_iter().enumerate() {
        let backup_path = backup_dir.join(index.to_string());
        if let Err(error) = fs::rename(&artifact.path, &backup_path) {
            let recovery_error = transaction.rollback().err();
            return Err(activation_transaction_error(
                package_id,
                "stage managed activation",
                error.into(),
                None,
                recovery_error,
            ));
        }

        transaction.staged_artifacts.push(StagedActivationArtifact {
            original_path: artifact.path.clone(),
            backup_path,
        });
    }

    Ok(transaction)
}

impl DeactivationTransaction {
    fn empty(package_id: PackageId) -> Self {
        Self {
            package_id,
            backup_dir: None,
            staged_artifacts: Vec::new(),
        }
    }

    pub(crate) fn rollback(self) -> Result<()> {
        let mut failures = Vec::new();

        for staged in self.staged_artifacts.iter().rev() {
            match fs::symlink_metadata(&staged.original_path) {
                Ok(_) => {
                    failures.push(format!(
                        "activation path became occupied during rollback: {}",
                        staged.original_path.display()
                    ));
                    continue;
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => {
                    failures.push(format!(
                        "could not inspect activation path {}: {error}",
                        staged.original_path.display()
                    ));
                    continue;
                }
            }

            if let Err(error) = fs::rename(&staged.backup_path, &staged.original_path) {
                failures.push(format!(
                    "could not restore activation path {}: {error}",
                    staged.original_path.display()
                ));
            }
        }

        if failures.is_empty() {
            if let Some(backup_dir) = self.backup_dir {
                match fs::remove_dir(backup_dir) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => failures.push(format!(
                        "could not remove empty activation backup directory: {error}"
                    )),
                }
            }
        }

        if failures.is_empty() {
            Ok(())
        } else {
            Err(FontbrewError::Conflict {
                package_id: self.package_id,
                message: format!("activation rollback failed: {}", failures.join("; ")),
            })
        }
    }

    pub(crate) fn commit(self) -> Result<()> {
        let Some(backup_dir) = self.backup_dir else {
            return Ok(());
        };

        fs::remove_dir_all(&backup_dir).map_err(|error| FontbrewError::Conflict {
            package_id: self.package_id,
            message: format!(
                "could not remove activation transaction backup at {}: {error}",
                backup_dir.display()
            ),
        })
    }
}

pub(crate) fn activation_transaction_error(
    package_id: &PackageId,
    phase: &str,
    primary_error: FontbrewError,
    cleanup_error: Option<FontbrewError>,
    restore_error: Option<FontbrewError>,
) -> FontbrewError {
    if cleanup_error.is_none() && restore_error.is_none() {
        return primary_error;
    }

    let mut message = format!("{phase} failed: {primary_error}");

    if let Some(error) = cleanup_error {
        message.push_str(&format!("; cleanup failed: {error}"));
    }

    if let Some(error) = restore_error {
        message.push_str(&format!("; restore old activation failed: {error}"));
    }

    FontbrewError::Conflict {
        package_id: package_id.clone(),
        message,
    }
}

fn validate_deactivation_artifact(
    activation_dir: &Path,
    artifact: &ActivationArtifact,
) -> Result<ArtifactPresence> {
    ensure_path_is_inside(activation_dir, &artifact.path)?;
    ensure_existing_ancestors_do_not_cross_symlinks(activation_dir, &artifact.path)?;
    let metadata = match fs::symlink_metadata(&artifact.path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(ArtifactPresence::Missing);
        }
        Err(error) => return Err(error.into()),
    };

    match artifact.strategy {
        ManifestActivationStrategy::Copy => {
            if !metadata.file_type().is_file() {
                return Err(FontbrewError::Conflict {
                    package_id: artifact.package_id.clone(),
                    message: format!(
                        "activation artifact is recorded as a copy but is not a regular file: {}",
                        artifact.path.display()
                    ),
                });
            }

            if !copy_matches_source(artifact)? {
                return Err(FontbrewError::Conflict {
                    package_id: artifact.package_id.clone(),
                    message: format!(
                        "activation copy no longer matches managed source: {}",
                        artifact.path.display()
                    ),
                });
            }
        }
        ManifestActivationStrategy::Symlink => {
            if !metadata.file_type().is_symlink() {
                return Err(FontbrewError::Conflict {
                    package_id: artifact.package_id.clone(),
                    message: format!(
                        "legacy activation artifact is recorded as a symlink but is not a symlink: {}",
                        artifact.path.display()
                    ),
                });
            }

            if fs::read_link(&artifact.path)? != artifact.source_path {
                return Err(FontbrewError::Conflict {
                    package_id: artifact.package_id.clone(),
                    message: format!(
                        "legacy activation symlink points to a different source: {}",
                        artifact.path.display()
                    ),
                });
            }
        }
    }

    Ok(ArtifactPresence::Present)
}

fn create_deactivation_backup_dir(activation_dir: &Path) -> Result<PathBuf> {
    static NEXT_TRANSACTION_ID: AtomicU64 = AtomicU64::new(0);

    reject_existing_symlink(activation_dir)?;
    fs::create_dir_all(activation_dir)?;

    loop {
        let id = NEXT_TRANSACTION_ID.fetch_add(1, Ordering::Relaxed);
        let backup_dir = activation_dir.join(format!(
            ".fontbrew-deactivation-{}-{id}",
            std::process::id()
        ));
        match fs::create_dir(&backup_dir) {
            Ok(()) => return Ok(backup_dir),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
    }
}

fn activate_copy(artifact: &ActivationArtifact) -> Result<()> {
    if artifact.strategy != ManifestActivationStrategy::Copy {
        return Err(FontbrewError::Conflict {
            package_id: artifact.package_id.clone(),
            message: "new activation artifacts must use the copy strategy".to_string(),
        });
    }

    if let Some(parent) = artifact.path.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut source = File::open(&artifact.source_path)?;
    let mut destination = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&artifact.path)
    {
        Ok(destination) => destination,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            return Err(FontbrewError::Conflict {
                package_id: artifact.package_id.clone(),
                message: format!(
                    "activation artifact already exists and is not managed by this plan: {}",
                    artifact.path.display()
                ),
            });
        }
        Err(error) => return Err(error.into()),
    };

    if let Err(error) = io::copy(&mut source, &mut destination) {
        let cleanup_error = fs::remove_file(&artifact.path)
            .err()
            .map(FontbrewError::from);
        return match cleanup_error {
            Some(cleanup_error) => Err(activation_transaction_error(
                &artifact.package_id,
                "copy activation file",
                error.into(),
                Some(cleanup_error),
                None,
            )),
            None => Err(error.into()),
        };
    }

    Ok(())
}

fn has_unmanaged_conflict(artifact: &ActivationArtifact) -> Result<bool> {
    match fs::symlink_metadata(&artifact.path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn copy_matches_source(artifact: &ActivationArtifact) -> Result<bool> {
    let copy_bytes = fs::read(&artifact.path)?;
    let source_bytes = fs::read(&artifact.source_path)?;
    Ok(copy_bytes == source_bytes)
}

fn ensure_path_is_inside(parent: &Path, child: &Path) -> Result<()> {
    let relative_path = child
        .strip_prefix(parent)
        .map_err(|_| FontbrewError::PathResolution {
            message: format!(
                "activation artifact must be inside activation directory: {} is not under {}",
                child.display(),
                parent.display()
            ),
        })?;

    if relative_path.as_os_str().is_empty() {
        return Err(FontbrewError::PathResolution {
            message: format!(
                "activation artifact must be inside activation directory: {} is not under {}",
                child.display(),
                parent.display()
            ),
        });
    }

    if relative_path
        .components()
        .all(|component| matches!(component, Component::Normal(_)))
    {
        Ok(())
    } else {
        Err(FontbrewError::PathResolution {
            message: format!(
                "activation artifact path contains an unsafe component: {}",
                child.display()
            ),
        })
    }
}

fn ensure_existing_ancestors_do_not_cross_symlinks(
    root: &Path,
    artifact_path: &Path,
) -> Result<()> {
    reject_existing_symlink(root)?;

    let parent = artifact_path
        .parent()
        .ok_or_else(|| FontbrewError::PathResolution {
            message: format!(
                "activation artifact has no parent: {}",
                artifact_path.display()
            ),
        })?;
    let relative_parent = parent
        .strip_prefix(root)
        .map_err(|_| FontbrewError::PathResolution {
            message: format!(
                "activation artifact parent must be under activation directory: {}",
                parent.display()
            ),
        })?;

    let mut current = root.to_path_buf();
    for component in relative_parent.components() {
        if let Component::Normal(name) = component {
            current.push(name);
            reject_existing_symlink(&current)?;
        }
    }

    Ok(())
}

fn reject_existing_symlink(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(FontbrewError::PathResolution {
            message: format!(
                "activation path must not cross a symlink ancestor: {}",
                path.display()
            ),
        }),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package_id() -> PackageId {
        PackageId::parse("inter").expect("valid package id")
    }

    #[test]
    fn transaction_error_preserves_primary_error_without_recovery_failure() {
        let error = activation_transaction_error(
            &package_id(),
            "test transaction",
            FontbrewError::PathResolution {
                message: "primary path error".to_string(),
            },
            None,
            None,
        );

        assert!(matches!(error, FontbrewError::PathResolution { .. }));
    }

    #[cfg(unix)]
    #[test]
    fn transaction_rollback_restores_legacy_symlink_unchanged() {
        let temp = tempfile::tempdir().expect("tempdir");
        let activation_dir = temp.path().join("activation");
        let source_path = temp.path().join("source.ttf");
        let artifact_path = activation_dir.join("Inter-Regular.ttf");
        fs::create_dir_all(&activation_dir).expect("create activation dir");
        fs::write(&source_path, b"font").expect("write source");
        std::os::unix::fs::symlink(&source_path, &artifact_path)
            .expect("create legacy activation symlink");
        let artifact = ActivationArtifact {
            package_id: package_id(),
            path: artifact_path.clone(),
            source_path: source_path.clone(),
            strategy: ManifestActivationStrategy::Symlink,
        };

        let transaction = deactivate_transactionally(
            &activation_dir,
            &artifact.package_id,
            std::slice::from_ref(&artifact),
        )
        .expect("stage legacy activation");
        assert!(fs::symlink_metadata(&artifact_path).is_err());

        transaction.rollback().expect("restore legacy activation");

        assert!(fs::symlink_metadata(&artifact_path)
            .expect("restored artifact metadata")
            .file_type()
            .is_symlink());
        assert_eq!(
            fs::read_link(&artifact_path).expect("restored symlink target"),
            source_path
        );
    }
}
