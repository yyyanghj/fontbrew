use std::{
    fs,
    path::{Component, Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{
    error::{FontbrewError, Result},
    ExecutionPolicy, PackageId, PlanRisk,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActivationStrategy {
    Symlink,
    Copy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivationRequest {
    pub package_id: PackageId,
    pub font_files: Vec<PathBuf>,
    pub activation_dir: PathBuf,
    pub strategy: ActivationStrategy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivationArtifact {
    pub package_id: PackageId,
    pub path: PathBuf,
    pub source_path: PathBuf,
    pub strategy: ActivationStrategy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivationPlan {
    pub package_id: PackageId,
    pub activation_dir: PathBuf,
    pub strategy: ActivationStrategy,
    pub artifacts: Vec<ActivationArtifact>,
    pub risks: Vec<PlanRisk>,
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
                strategy: request.strategy,
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
            strategy: request.strategy,
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

        for artifact in &self.artifacts {
            ensure_path_is_inside(&self.activation_dir, &artifact.path)?;
            ensure_existing_ancestors_do_not_cross_symlinks(&self.activation_dir, &artifact.path)?;

            match artifact.strategy {
                ActivationStrategy::Symlink => activate_symlink(artifact)?,
                ActivationStrategy::Copy => activate_copy(artifact)?,
            }
        }

        Ok(self.artifacts.clone())
    }
}

pub fn deactivate(activation_dir: &Path, artifacts: &[ActivationArtifact]) -> Result<()> {
    for artifact in artifacts {
        ensure_path_is_inside(activation_dir, &artifact.path)?;
        ensure_existing_ancestors_do_not_cross_symlinks(activation_dir, &artifact.path)?;

        match artifact.strategy {
            ActivationStrategy::Symlink => deactivate_symlink(artifact)?,
            ActivationStrategy::Copy => deactivate_copy(artifact)?,
        }
    }

    Ok(())
}

pub(crate) fn replace_activation(
    old_artifacts: &[ActivationArtifact],
    new_plan: &ActivationPlan,
    policy: ExecutionPolicy,
) -> Result<Vec<ActivationArtifact>> {
    let mut removed_old_artifacts = Vec::new();
    for old_artifact in old_artifacts {
        if let Err(error) = deactivate(&new_plan.activation_dir, std::slice::from_ref(old_artifact))
        {
            let restore_error = restore_activation(
                &new_plan.activation_dir,
                &new_plan.package_id,
                &removed_old_artifacts,
            )
            .err();
            return Err(activation_transaction_error(
                &new_plan.package_id,
                "deactivate old activation",
                error,
                None,
                restore_error,
            ));
        }

        removed_old_artifacts.push(old_artifact.clone());
    }

    let mut created_new_artifacts = Vec::new();
    for new_artifact in &new_plan.artifacts {
        let single_plan = ActivationPlan {
            package_id: new_plan.package_id.clone(),
            activation_dir: new_plan.activation_dir.clone(),
            strategy: new_plan.strategy,
            artifacts: vec![new_artifact.clone()],
            risks: Vec::new(),
        };

        match single_plan.apply(policy.clone()) {
            Ok(mut artifacts) => created_new_artifacts.append(&mut artifacts),
            Err(error) => {
                let cleanup_error =
                    deactivate(&new_plan.activation_dir, &created_new_artifacts).err();
                let restore_error = restore_activation(
                    &new_plan.activation_dir,
                    &new_plan.package_id,
                    old_artifacts,
                )
                .err();
                return Err(activation_transaction_error(
                    &new_plan.package_id,
                    "activate new activation",
                    error,
                    cleanup_error,
                    restore_error,
                ));
            }
        }
    }

    Ok(created_new_artifacts)
}

pub(crate) fn restore_activation(
    activation_dir: &Path,
    package_id: &PackageId,
    artifacts: &[ActivationArtifact],
) -> Result<()> {
    if artifacts.is_empty() {
        return Ok(());
    }

    let plan = ActivationPlan {
        package_id: package_id.clone(),
        activation_dir: activation_dir.to_path_buf(),
        strategy: artifacts[0].strategy,
        artifacts: artifacts.to_vec(),
        risks: Vec::new(),
    };
    plan.apply(ExecutionPolicy::AssumeYes)?;
    Ok(())
}

fn activation_transaction_error(
    package_id: &PackageId,
    phase: &str,
    primary_error: FontbrewError,
    cleanup_error: Option<FontbrewError>,
    restore_error: Option<FontbrewError>,
) -> FontbrewError {
    let mut message = format!("{phase} failed: {primary_error}");

    if let Some(error) = cleanup_error {
        message.push_str(&format!("; cleanup new activation failed: {error}"));
    }

    if let Some(error) = restore_error {
        message.push_str(&format!("; restore old activation failed: {error}"));
    }

    FontbrewError::Conflict {
        package_id: package_id.clone(),
        message,
    }
}

fn deactivate_symlink(artifact: &ActivationArtifact) -> Result<()> {
    match fs::read_link(&artifact.path) {
        Ok(target) if target == artifact.source_path => fs::remove_file(&artifact.path)?,
        Ok(_) => {
            return Err(FontbrewError::Conflict {
                package_id: artifact.package_id.clone(),
                message: format!(
                    "activation symlink points to a different source: {}",
                    artifact.path.display()
                ),
            });
        }
        Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => {
            return Err(FontbrewError::Conflict {
                package_id: artifact.package_id.clone(),
                message: format!(
                    "activation artifact already exists and is not a symlink: {}",
                    artifact.path.display()
                ),
            });
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    Ok(())
}

fn deactivate_copy(artifact: &ActivationArtifact) -> Result<()> {
    let metadata = match fs::symlink_metadata(&artifact.path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };

    if !metadata.file_type().is_file() {
        return Err(FontbrewError::Conflict {
            package_id: artifact.package_id.clone(),
            message: format!(
                "activation artifact already exists and is not a managed copy: {}",
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

    fs::remove_file(&artifact.path)?;
    Ok(())
}

fn activate_symlink(artifact: &ActivationArtifact) -> Result<()> {
    match fs::read_link(&artifact.path) {
        Ok(target) if target == artifact.source_path => return Ok(()),
        Ok(_) => {
            return Err(FontbrewError::Conflict {
                package_id: artifact.package_id.clone(),
                message: format!(
                    "activation symlink points to a different source: {}",
                    artifact.path.display()
                ),
            });
        }
        Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => {
            return Err(FontbrewError::Conflict {
                package_id: artifact.package_id.clone(),
                message: format!(
                    "activation artifact already exists and is not a symlink: {}",
                    artifact.path.display()
                ),
            });
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&artifact.source_path, &artifact.path)?;
        Ok(())
    }

    #[cfg(not(unix))]
    {
        let _ = artifact;
        Err(FontbrewError::NotImplemented {
            operation: "symlink_activation_on_non_unix",
        })
    }
}

fn activate_copy(artifact: &ActivationArtifact) -> Result<()> {
    match fs::symlink_metadata(&artifact.path) {
        Ok(_) => {
            return Err(FontbrewError::Conflict {
                package_id: artifact.package_id.clone(),
                message: format!(
                    "activation artifact already exists and is not managed by this plan: {}",
                    artifact.path.display()
                ),
            });
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    if let Some(parent) = artifact.path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(&artifact.source_path, &artifact.path)?;
    Ok(())
}

fn has_unmanaged_conflict(artifact: &ActivationArtifact) -> Result<bool> {
    match artifact.strategy {
        ActivationStrategy::Symlink => has_unmanaged_symlink_conflict(artifact),
        ActivationStrategy::Copy => has_unmanaged_copy_conflict(artifact),
    }
}

fn has_unmanaged_symlink_conflict(artifact: &ActivationArtifact) -> Result<bool> {
    match fs::read_link(&artifact.path) {
        Ok(target) => Ok(target != artifact.source_path),
        Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn has_unmanaged_copy_conflict(artifact: &ActivationArtifact) -> Result<bool> {
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
