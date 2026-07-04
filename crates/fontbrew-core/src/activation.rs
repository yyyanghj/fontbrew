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
                ActivationStrategy::Copy => {
                    return Err(FontbrewError::NotImplemented {
                        operation: "copy_activation",
                    });
                }
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
            ActivationStrategy::Copy => {
                return Err(FontbrewError::NotImplemented {
                    operation: "copy_deactivation",
                });
            }
        }
    }

    Ok(())
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

fn has_unmanaged_conflict(artifact: &ActivationArtifact) -> Result<bool> {
    match fs::read_link(&artifact.path) {
        Ok(target) => Ok(target != artifact.source_path),
        Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
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
