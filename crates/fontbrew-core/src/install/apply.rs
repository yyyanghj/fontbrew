use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use crate::fs::AtomicWriteCommitStatus;

use super::{
    cleanup_staging, deactivate, deactivate_transactionally, dedupe_family_names,
    ensure_existing_path_does_not_cross_symlink, ensure_not_cancelled, ensure_path_inside,
    family_matches_any, font_format_from_manifest_format, installed_at_now, manifest_font_format,
    operation_suffix, prepared_package_id, ActivationArtifact, ActivationPlan, ActivationPlanner,
    ActivationRequest, CancellationToken, DeactivationTransaction, ExecutionPolicy, FamilyName,
    FontMetadataReader, FontbrewError, FontbrewPaths, InstallPlan, InstallReport,
    ManagedActivationArtifact, ManagedFontFile, ManifestActivationArtifactRecord,
    ManifestFontFileRecord, ManifestPackageRecord, ManifestSource, ManifestStore, ManifestV1,
    PackageId, PackageVersion, PlanRisk, PreparedInstallPackage, PreparedInstallSource,
    ProgressEvent, ProgressSink, ProviderKind, Result, TtfParserMetadataReader,
    LOCAL_ARCHIVE_VERSION,
};

pub(super) fn apply_prepared_install(
    paths: &FontbrewPaths,
    manifest: &mut crate::manifest::ManifestV1,
    prepared: &PreparedInstallPackage,
    policy: ExecutionPolicy,
    progress: &mut dyn ProgressSink,
    cancellation: &dyn CancellationToken,
) -> Result<InstallReport> {
    ensure_not_cancelled(cancellation)?;
    reject_unmanaged_package_store(paths, manifest, prepared)?;
    ensure_not_cancelled(cancellation)?;

    let previous_record = if prepared.reinstall {
        manifest
            .get_package(&prepared_package_id(prepared))
            .cloned()
    } else {
        None
    };
    let previous_activation_artifacts = previous_record
        .as_ref()
        .map(activation_artifacts_from_record)
        .unwrap_or_default();
    let previous_package_store_dir = previous_record
        .as_ref()
        .map(|record| paths.package_store_dir(&record.package_id, &record.version));
    if let Some(previous_package_store_dir) = &previous_package_store_dir {
        ensure_existing_path_does_not_cross_symlink(
            &paths.managed_store_dir(),
            previous_package_store_dir,
        )?;
    }

    let deactivation_transaction = deactivate_transactionally(
        &paths.activation_dir(),
        &prepared_package_id(prepared),
        &previous_activation_artifacts,
    )?;

    if let Err(error) = ensure_not_cancelled(cancellation) {
        return Err(with_recovery_error(
            "install cancelled after deactivating the previous package",
            error,
            deactivation_transaction.rollback(),
        ));
    }

    let backup_result = match previous_record
        .as_ref()
        .zip(previous_package_store_dir.as_ref())
    {
        Some((record, previous_store_dir))
            if previous_store_dir == &prepared.package_store_dir && previous_store_dir.exists() =>
        {
            backup_package_store(
                paths,
                &record.package_id,
                &record.version,
                previous_store_dir,
            )
            .map(Some)
        }
        _ => Ok(None),
    };
    let backup_dir = match backup_result {
        Ok(backup_dir) => backup_dir,
        Err(error) => {
            return Err(with_recovery_error(
                "package store backup failed after deactivating the previous package",
                error,
                deactivation_transaction.rollback(),
            ));
        }
    };

    if let Err(error) = ensure_not_cancelled(cancellation) {
        let recovery = rollback_package_state(
            paths,
            &[],
            &prepared.package_store_dir,
            backup_dir.as_deref(),
            deactivation_transaction,
        );
        return Err(with_recovery_error("install cancelled", error, recovery));
    }
    if let Err(error) = copy_prepared_files(paths, prepared) {
        let recovery = rollback_package_state(
            paths,
            &[],
            &prepared.package_store_dir,
            backup_dir.as_deref(),
            deactivation_transaction,
        );
        return Err(with_recovery_error(
            "copy prepared package files failed",
            error,
            recovery,
        ));
    }

    let activation_plan = ActivationPlan {
        package_id: prepared_package_id(prepared),
        activation_dir: prepared.activation_dir.clone(),
        artifacts: prepared.activation_artifacts.clone(),
        risks: Vec::new(),
    };
    if let Err(error) = ensure_not_cancelled(cancellation) {
        let recovery = rollback_package_state(
            paths,
            &[],
            &prepared.package_store_dir,
            backup_dir.as_deref(),
            deactivation_transaction,
        );
        return Err(with_recovery_error("install cancelled", error, recovery));
    }
    let activation_artifacts = match activation_plan.apply(policy) {
        Ok(artifacts) => artifacts,
        Err(error) => {
            let recovery = rollback_package_state(
                paths,
                &[],
                &prepared.package_store_dir,
                backup_dir.as_deref(),
                deactivation_transaction,
            );
            return Err(with_recovery_error(
                "activate package failed",
                error,
                recovery,
            ));
        }
    };
    let manifest_record = manifest_record_from_prepared(prepared, activation_artifacts.clone())?;

    if let Err(error) = ensure_not_cancelled(cancellation) {
        let recovery = rollback_package_state(
            paths,
            &activation_artifacts,
            &prepared.package_store_dir,
            backup_dir.as_deref(),
            deactivation_transaction,
        );
        return Err(with_recovery_error("install cancelled", error, recovery));
    }
    manifest.insert_package(manifest_record.clone())?;
    if let Err(error) = ManifestStore::write_with_commit_status(&paths.manifest_path(), manifest) {
        match error.commit_status {
            AtomicWriteCommitStatus::NotCommitted => {
                let recovery = rollback_package_state(
                    paths,
                    &activation_artifacts,
                    &prepared.package_store_dir,
                    backup_dir.as_deref(),
                    deactivation_transaction,
                );
                return Err(with_recovery_error(
                    "manifest write was not committed",
                    error.error,
                    recovery,
                ));
            }
            AtomicWriteCommitStatus::Uncertain => {
                return Err(FontbrewError::CommitUncertain {
                    operation: "installation",
                    package_ids: vec![manifest_record.package_id.clone()],
                    message: format!(
                        "manifest write failed after installing package files and activation; kept the new state: {}",
                        error.error
                    ),
                });
            }
        }
    }

    let mut package_cleanup_dirs = backup_dir.into_iter().collect::<Vec<_>>();
    if let Some(previous_store_dir) = previous_package_store_dir {
        if previous_store_dir != prepared.package_store_dir {
            package_cleanup_dirs.push(previous_store_dir);
        }
    }
    cleanup_committed_backups(
        "installation",
        &manifest_record.package_id,
        package_cleanup_dirs,
        deactivation_transaction,
    )?;

    progress.emit(ProgressEvent::FinishedPackage {
        package_id: manifest_record.package_id.clone(),
    });

    Ok(install_report_from_record(&manifest_record, true, false))
}

pub(crate) fn copy_prepared_files(
    paths: &FontbrewPaths,
    prepared: &PreparedInstallPackage,
) -> Result<()> {
    ensure_existing_path_does_not_cross_symlink(
        &paths.managed_store_dir(),
        &prepared.package_store_dir,
    )?;
    ensure_existing_path_does_not_cross_symlink(&paths.managed_store_dir(), &prepared.files_dir)?;
    fs::create_dir_all(&prepared.files_dir)?;

    for font_file in &prepared.font_files {
        ensure_path_inside(&prepared.staging_dir, &font_file.staging_path)?;
        ensure_path_inside(&prepared.files_dir, &font_file.stored_path)?;
        ensure_existing_path_does_not_cross_symlink(
            &paths.managed_store_dir(),
            &font_file.stored_path,
        )?;

        if let Some(parent) = font_file.stored_path.parent() {
            fs::create_dir_all(parent)?;
        }

        fs::copy(&font_file.staging_path, &font_file.stored_path)?;
    }

    Ok(())
}

fn reject_unmanaged_package_store(
    paths: &FontbrewPaths,
    manifest: &ManifestV1,
    prepared: &PreparedInstallPackage,
) -> Result<()> {
    ensure_existing_path_does_not_cross_symlink(
        &paths.managed_store_dir(),
        &prepared.package_store_dir,
    )?;

    let package_id = prepared_package_id(prepared);
    if prepared.package_store_dir.exists() && manifest.get_package(&package_id).is_none() {
        return Err(FontbrewError::Conflict {
            package_id,
            message: format!(
                "package store directory exists outside manifest management: {}",
                prepared.package_store_dir.display()
            ),
        });
    }

    Ok(())
}

pub(super) fn backup_package_store(
    paths: &FontbrewPaths,
    package_id: &PackageId,
    version: &PackageVersion,
    package_store_dir: &Path,
) -> Result<PathBuf> {
    ensure_existing_path_does_not_cross_symlink(&paths.managed_store_dir(), package_store_dir)?;

    let backup_dir = package_store_dir
        .parent()
        .ok_or_else(|| FontbrewError::PathResolution {
            message: format!(
                "package store path has no parent: {}",
                package_store_dir.display()
            ),
        })?
        .join(format!(
            ".{}-{}-backup-{}",
            package_id.as_str(),
            version.as_str(),
            operation_suffix()?
        ));
    ensure_existing_path_does_not_cross_symlink(&paths.managed_store_dir(), &backup_dir)?;
    fs::rename(package_store_dir, &backup_dir)?;

    Ok(backup_dir)
}

pub(super) fn rollback_package_state(
    paths: &FontbrewPaths,
    activation_artifacts: &[ActivationArtifact],
    package_store_dir: &Path,
    backup_dir: Option<&Path>,
    deactivation_transaction: DeactivationTransaction,
) -> Result<()> {
    let mut failures = Vec::new();

    if let Err(error) = deactivate(&paths.activation_dir(), activation_artifacts) {
        failures.push(format!("cleanup new activation failed: {error}"));
    }
    if let Err(error) = rollback_package_store(package_store_dir, backup_dir) {
        failures.push(format!("restore package store failed: {error}"));
    }
    if let Err(error) = deactivation_transaction.rollback() {
        failures.push(format!("restore previous activation failed: {error}"));
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(FontbrewError::Manifest {
            message: format!("package state rollback failed: {}", failures.join("; ")),
        })
    }
}

fn rollback_package_store(package_store_dir: &Path, backup_dir: Option<&Path>) -> Result<()> {
    remove_dir_all_if_exists(package_store_dir)?;

    if let Some(backup_dir) = backup_dir {
        fs::rename(backup_dir, package_store_dir)?;
    }

    Ok(())
}

pub(super) fn with_recovery_error(
    context: &str,
    primary_error: FontbrewError,
    recovery: Result<()>,
) -> FontbrewError {
    match recovery {
        Ok(()) => primary_error,
        Err(recovery_error) => FontbrewError::Manifest {
            message: format!("{context}: {primary_error}; recovery also failed: {recovery_error}"),
        },
    }
}

pub(super) fn cleanup_committed_backups(
    operation: &'static str,
    package_id: &PackageId,
    package_cleanup_dirs: Vec<PathBuf>,
    deactivation_transaction: DeactivationTransaction,
) -> Result<()> {
    let mut failures = Vec::new();

    for cleanup_dir in package_cleanup_dirs {
        if let Err(error) = remove_dir_all_if_exists(&cleanup_dir) {
            failures.push(format!(
                "could not remove package cleanup directory at {}: {error}",
                cleanup_dir.display()
            ));
        }
    }
    if let Err(error) = deactivation_transaction.commit() {
        failures.push(format!("could not remove activation backup: {error}"));
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(FontbrewError::CommittedCleanup {
            operation,
            package_ids: vec![package_id.clone()],
            message: failures.join("; "),
        })
    }
}

fn remove_dir_all_if_exists(path: &Path) -> std::io::Result<()> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

pub(crate) fn manifest_record_from_prepared(
    prepared: &PreparedInstallPackage,
    activation_artifacts: Vec<ActivationArtifact>,
) -> Result<ManifestPackageRecord> {
    Ok(ManifestPackageRecord {
        package_id: prepared_package_id(prepared),
        version: prepared.version.clone(),
        source: manifest_source_from_prepared(&prepared.source),
        update_source: manifest_update_source_from_prepared(&prepared.source),
        families: prepared.families.clone(),
        font_files: manifest_font_files_from_prepared(prepared),
        activation_artifacts: activation_artifacts
            .into_iter()
            .map(|artifact| ManifestActivationArtifactRecord {
                path: artifact.path,
                source_path: artifact.source_path,
                strategy: artifact.strategy,
            })
            .collect(),
        installed_at: installed_at_now(),
        active_version: Some(prepared.version.clone()),
    })
}

pub(super) fn manifest_source_from_prepared(source: &PreparedInstallSource) -> ManifestSource {
    match source {
        PreparedInstallSource::LocalArchive { path } => {
            ManifestSource::LocalArchive { path: path.clone() }
        }
        PreparedInstallSource::GitHub { owner, repo } => ManifestSource::GitHub {
            owner: owner.clone(),
            repo: repo.clone(),
        },
        PreparedInstallSource::Provider { provider, id } => ManifestSource::Provider {
            provider: provider.clone(),
            id: id.clone(),
        },
    }
}

pub(super) fn manifest_update_source_from_prepared(
    source: &PreparedInstallSource,
) -> Option<ManifestSource> {
    match source {
        PreparedInstallSource::LocalArchive { .. } => None,
        PreparedInstallSource::GitHub { owner, repo } => Some(ManifestSource::GitHub {
            owner: owner.clone(),
            repo: repo.clone(),
        }),
        PreparedInstallSource::Provider { .. } => None,
    }
}

fn manifest_font_files_from_prepared(
    prepared: &PreparedInstallPackage,
) -> Vec<ManifestFontFileRecord> {
    let mut records = Vec::new();

    for font_file in &prepared.font_files {
        for face in &font_file.faces {
            records.push(ManifestFontFileRecord {
                path: font_file.stored_path.clone(),
                family: face.family.clone(),
                style: face.style.clone(),
                weight: face.weight,
                format: manifest_font_format(&face.format),
            });
        }
    }

    records
}

pub(crate) fn activation_artifacts_from_record(
    record: &ManifestPackageRecord,
) -> Vec<ActivationArtifact> {
    record
        .activation_artifacts
        .iter()
        .map(|artifact| ActivationArtifact {
            package_id: record.package_id.clone(),
            path: artifact.path.clone(),
            source_path: artifact.source_path.clone(),
            strategy: artifact.strategy,
        })
        .collect()
}

pub(super) fn managed_font_files_from_record(
    record: &ManifestPackageRecord,
) -> Vec<ManagedFontFile> {
    record
        .font_files
        .iter()
        .map(|font_file| ManagedFontFile {
            path: font_file.path.clone(),
            family: font_file.family.clone(),
            style: font_file.style.clone(),
            weight: managed_font_weight(record, font_file),
            format: font_format_from_manifest_format(font_file.format),
        })
        .collect()
}

fn managed_font_weight(record: &ManifestPackageRecord, font_file: &ManifestFontFileRecord) -> u16 {
    if let ManifestSource::Provider {
        provider: ProviderKind::Fontsource,
        id,
    } = &record.source
    {
        if let Some(weight) = fontsource_variant_weight_from_path(id, &font_file.path) {
            return weight;
        }
    }

    font_file.weight
}

fn fontsource_variant_weight_from_path(provider_id: &str, path: &Path) -> Option<u16> {
    let stem = path.file_stem()?.to_str()?;
    let variant_part = stem.strip_prefix(provider_id)?.strip_prefix('-')?;
    let mut parts = variant_part.rsplit('-');
    parts.next()?;
    let weight = parts.next()?.parse::<u16>().ok()?;

    (1..=1000).contains(&weight).then_some(weight)
}

pub(super) fn managed_activation_artifacts_from_record(
    record: &ManifestPackageRecord,
) -> Vec<ManagedActivationArtifact> {
    record
        .activation_artifacts
        .iter()
        .map(|artifact| ManagedActivationArtifact {
            path: artifact.path.clone(),
            source_path: artifact.source_path.clone(),
            strategy: artifact.strategy,
        })
        .collect()
}

pub(super) fn install_report_from_record(
    record: &ManifestPackageRecord,
    installed: bool,
    already_installed: bool,
) -> InstallReport {
    InstallReport {
        package_id: record.package_id.clone(),
        installed_version: record.version.clone(),
        families: record.families.clone(),
        installed,
        already_installed,
        activated: record.active_version.is_some(),
    }
}

pub(super) fn dry_run_install_report(plan: InstallPlan) -> Result<InstallReport> {
    if let Some(prepared) = plan.prepared {
        cleanup_staging(&prepared.staging_dir);
        return Ok(InstallReport {
            package_id: plan.package_id,
            installed_version: prepared.version,
            families: prepared.families,
            installed: false,
            already_installed: false,
            activated: false,
        });
    }

    Ok(InstallReport {
        package_id: plan.package_id,
        installed_version: plan
            .target_version
            .unwrap_or_else(|| PackageVersion::new(LOCAL_ARCHIVE_VERSION)),
        families: Vec::new(),
        installed: false,
        already_installed: plan.already_installed,
        activated: false,
    })
}

pub(super) fn cleanup_install_plan_staging(plan: &InstallPlan) {
    if let Some(prepared) = &plan.prepared {
        cleanup_staging(&prepared.staging_dir);
    }
}

pub(super) fn first_blocking_conflict_description(risks: &[PlanRisk]) -> Option<String> {
    risks.iter().find_map(|risk| match risk {
        PlanRisk::Conflict { description, .. } => Some(description.clone()),
        PlanRisk::AmbiguousAsset { .. } | PlanRisk::UnmanagedFontOverlap { .. } => None,
    })
}

pub(super) fn conflict_error_from_risk(
    default_package_id: &PackageId,
    risk: &PlanRisk,
) -> FontbrewError {
    match risk {
        PlanRisk::Conflict {
            package_id,
            description,
        } => FontbrewError::Conflict {
            package_id: package_id.clone(),
            message: description.clone(),
        },
        PlanRisk::AmbiguousAsset {
            package_id,
            description,
        } => FontbrewError::Conflict {
            package_id: package_id.clone(),
            message: description.clone(),
        },
        PlanRisk::UnmanagedFontOverlap { description, .. } => FontbrewError::Conflict {
            package_id: default_package_id.clone(),
            message: description.clone(),
        },
    }
}

pub(super) fn source_label(source: &ManifestSource) -> String {
    match source {
        ManifestSource::GitHub { owner, repo } => format!("github:{owner}/{repo}"),
        ManifestSource::Provider {
            provider: ProviderKind::Fontsource,
            id,
        } => format!("fontsource:{id}"),
        ManifestSource::LocalArchive { path } => format!("local archive:{}", path.display()),
    }
}

fn optional_source_label(source: Option<&ManifestSource>) -> String {
    source
        .map(source_label)
        .unwrap_or_else(|| "none".to_string())
}

pub(super) fn source_conflict_risk(
    record: &ManifestPackageRecord,
    requested_source: &ManifestSource,
    requested_update_source: Option<&ManifestSource>,
) -> Option<PlanRisk> {
    if &record.source == requested_source
        && record.update_source.as_ref() == requested_update_source
    {
        return None;
    }

    Some(PlanRisk::Conflict {
        package_id: record.package_id.clone(),
        description: format!(
            "package is already managed from a different source; installed source: {}; requested source: {}; installed update source: {}; requested update source: {}",
            source_label(&record.source),
            source_label(requested_source),
            optional_source_label(record.update_source.as_ref()),
            optional_source_label(requested_update_source),
        ),
    })
}

pub(super) fn current_install_risks(
    paths: &FontbrewPaths,
    manifest: &ManifestV1,
    prepared: &PreparedInstallPackage,
) -> Result<Vec<PlanRisk>> {
    let unmanaged_overlap_risks = unmanaged_same_family_overlap_risks(paths, manifest, prepared)?;
    current_install_risks_with_unmanaged_overlap_risks(manifest, prepared, unmanaged_overlap_risks)
}

pub(super) fn current_install_risks_with_unmanaged_overlap_risks(
    manifest: &ManifestV1,
    prepared: &PreparedInstallPackage,
    unmanaged_overlap_risks: Vec<PlanRisk>,
) -> Result<Vec<PlanRisk>> {
    let package_id = prepared_package_id(prepared);
    let mut risks = managed_activation_path_conflict_risks(manifest, prepared);
    risks.extend(current_activation_artifact_risks(manifest, prepared)?);
    risks.extend(unmanaged_overlap_risks);

    if prepared.package_store_dir.exists() && manifest.get_package(&package_id).is_none() {
        risks.push(PlanRisk::Conflict {
            package_id,
            description: format!(
                "package store directory exists outside manifest management: {}",
                prepared.package_store_dir.display()
            ),
        });
    }

    Ok(risks)
}

fn current_activation_artifact_risks(
    manifest: &ManifestV1,
    prepared: &PreparedInstallPackage,
) -> Result<Vec<PlanRisk>> {
    let package_id = prepared_package_id(prepared);
    let activation_plan = ActivationPlanner::plan(ActivationRequest {
        package_id: package_id.clone(),
        font_files: prepared
            .activation_artifacts
            .iter()
            .filter(|artifact| {
                !activation_path_is_managed_by_package(manifest, &package_id, &artifact.path)
            })
            .map(|artifact| artifact.source_path.clone())
            .collect(),
        activation_dir: prepared.activation_dir.clone(),
    })?;

    Ok(activation_plan.risks)
}

fn activation_path_is_managed_by_package(
    manifest: &ManifestV1,
    package_id: &PackageId,
    path: &Path,
) -> bool {
    manifest.get_package(package_id).is_some_and(|record| {
        record
            .activation_artifacts
            .iter()
            .any(|artifact| artifact.path == path)
    })
}

fn managed_activation_path_conflict_risks(
    manifest: &ManifestV1,
    prepared: &PreparedInstallPackage,
) -> Vec<PlanRisk> {
    let mut risks = Vec::new();
    let package_id = prepared_package_id(prepared);

    for artifact in &prepared.activation_artifacts {
        for record in manifest.packages.values() {
            if record.package_id == package_id {
                continue;
            }

            if record
                .activation_artifacts
                .iter()
                .any(|existing| existing.path == artifact.path)
            {
                risks.push(PlanRisk::Conflict {
                    package_id: package_id.clone(),
                    description: format!(
                        "activation artifact path is already managed by package {}: {}",
                        record.package_id.as_str(),
                        artifact.path.display()
                    ),
                });
            }
        }
    }

    risks
}

pub(super) fn unmanaged_same_family_overlap_risks(
    paths: &FontbrewPaths,
    manifest: &ManifestV1,
    prepared: &PreparedInstallPackage,
) -> Result<Vec<PlanRisk>> {
    let mut managed_paths = managed_activation_artifact_paths(manifest);
    managed_paths.extend(
        prepared
            .activation_artifacts
            .iter()
            .map(|artifact| artifact.path.clone()),
    );
    unmanaged_same_family_overlap_risks_for_families(paths, &managed_paths, &prepared.families)
}

pub(super) fn unmanaged_same_family_overlap_risks_for_prepared_packages(
    paths: &FontbrewPaths,
    manifest: &ManifestV1,
    prepared_packages: &[PreparedInstallPackage],
) -> Result<Vec<PlanRisk>> {
    let mut managed_paths = managed_activation_artifact_paths(manifest);
    let mut families = Vec::new();
    for prepared in prepared_packages {
        managed_paths.extend(
            prepared
                .activation_artifacts
                .iter()
                .map(|artifact| artifact.path.clone()),
        );
        families.extend(prepared.families.iter().cloned());
    }

    let families = dedupe_family_names(families);
    unmanaged_same_family_overlap_risks_for_families(paths, &managed_paths, &families)
}

pub(super) fn unmanaged_overlap_risks_for_families(
    risks: &[PlanRisk],
    families: &[FamilyName],
) -> Vec<PlanRisk> {
    risks
        .iter()
        .filter(|risk| match risk {
            PlanRisk::UnmanagedFontOverlap { family_name, .. } => {
                family_matches_any(families, family_name)
            }
            PlanRisk::Conflict { .. } | PlanRisk::AmbiguousAsset { .. } => false,
        })
        .cloned()
        .collect()
}

fn managed_activation_artifact_paths(manifest: &ManifestV1) -> BTreeSet<PathBuf> {
    manifest
        .packages
        .values()
        .flat_map(|record| record.activation_artifacts.iter())
        .map(|artifact| artifact.path.clone())
        .collect()
}

fn unmanaged_same_family_overlap_risks_for_families(
    paths: &FontbrewPaths,
    managed_paths: &BTreeSet<PathBuf>,
    families: &[FamilyName],
) -> Result<Vec<PlanRisk>> {
    let mut scan_dirs = BTreeSet::new();
    scan_dirs.insert(paths.activation_dir());
    if let Some(user_fonts_dir) = paths.activation_dir().parent() {
        scan_dirs.insert(user_fonts_dir.to_path_buf());
    }

    let reader = TtfParserMetadataReader;
    let mut risks = Vec::new();
    let mut seen = BTreeSet::new();

    for scan_dir in scan_dirs {
        let entries = match fs::read_dir(&scan_dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };

        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if path == paths.activation_dir() || managed_paths.contains(&path) {
                continue;
            }

            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            if !is_scannable_font_artifact(&path, &metadata)? {
                continue;
            }

            if let Some(family) = overlapping_family(&reader, &path, families) {
                let key = (family.as_str().to_string(), path.clone());
                if seen.insert(key) {
                    risks.push(PlanRisk::UnmanagedFontOverlap {
                        family_name: family.clone(),
                        description: format!(
                            "unmanaged font file may overlap family {}: {}",
                            family.as_str(),
                            path.display()
                        ),
                    });
                }
            }
        }
    }

    Ok(risks)
}

fn is_scannable_font_artifact(path: &Path, metadata: &fs::Metadata) -> Result<bool> {
    if !is_desktop_font_path(path) {
        return Ok(false);
    }

    if metadata.file_type().is_file() {
        return Ok(true);
    }

    if !metadata.file_type().is_symlink() {
        return Ok(false);
    }

    match fs::metadata(path) {
        Ok(target_metadata) => Ok(target_metadata.file_type().is_file()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn is_desktop_font_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "ttf" | "otf" | "ttc" | "otc"
            )
        })
        .unwrap_or(false)
}

fn overlapping_family(
    reader: &TtfParserMetadataReader,
    path: &Path,
    installed_families: &[FamilyName],
) -> Option<FamilyName> {
    if let Ok(faces) = reader.read_file(path) {
        for face in faces {
            if let Some(family) = installed_families
                .iter()
                .find(|family| same_family_name(&face.family_name, family))
            {
                return Some(family.clone());
            }
        }
    }

    installed_families
        .iter()
        .find(|family| path_name_matches_family(path, family))
        .cloned()
}

fn same_family_name(left: &FamilyName, right: &FamilyName) -> bool {
    normalized_font_name(left.as_str()) == normalized_font_name(right.as_str())
}

fn path_name_matches_family(path: &Path, family: &FamilyName) -> bool {
    let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
        return false;
    };
    let stem = normalized_font_name(stem);
    let family = normalized_font_name(family.as_str());

    !family.is_empty() && stem.contains(&family)
}

fn normalized_font_name(name: &str) -> String {
    name.chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

pub(super) fn require_policy_for_risks(risks: &[PlanRisk], policy: &ExecutionPolicy) -> Result<()> {
    if risks.is_empty() {
        return Ok(());
    }

    match policy {
        ExecutionPolicy::SafeOnly | ExecutionPolicy::DryRun => {
            Err(FontbrewError::ExecutionPolicyRequired {
                risk: format!("{risks:?}"),
            })
        }
        ExecutionPolicy::AllowUserApprovedRisk | ExecutionPolicy::AssumeYes => Ok(()),
    }
}
