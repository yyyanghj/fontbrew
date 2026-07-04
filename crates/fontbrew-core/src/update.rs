use crate::{
    activation::{deactivate, ActivationArtifact, ActivationPlan},
    config::FontbrewConfig,
    error::{FontbrewError, Result},
    fetch::HttpClient,
    fs::{ensure_existing_path_does_not_cross_symlink, AtomicWriteCommitStatus, GlobalFileLock},
    github, install,
    manifest::{ManifestPackageRecord, ManifestSource, ManifestStore, ManifestV1},
    model::{
        ensure_not_cancelled, CancellationToken, ExecutionPolicy, OperationId, PlannedChange,
        PreparedInstallPackage, PreparedInstallSource, PreparedUpdatePackage, ProgressEvent,
        ProgressSink, UpdatePlan, UpdatePlanFailure, UpdatePlanPackage, UpdateReport,
        UpdateRequest, UpdatedPackage,
    },
    model::{NotUpdatablePackage, OutdatedPackage, OutdatedReport, OutdatedRequest},
    platform::FontbrewPaths,
    providers::FontsourceProvider,
    sources::GitHubRepo,
    tasks,
    version::{compare_versions, VersionComparison},
    FamilyName, PackageId, PackageVersion, PlanRisk, ProviderKind,
};
use std::{
    fs,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

static UPDATE_OPERATION_COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn outdated(
    paths: &FontbrewPaths,
    request: OutdatedRequest,
    http_client: &dyn HttpClient,
) -> Result<OutdatedReport> {
    let manifest = ManifestStore::read_or_empty(&paths.manifest_path())?;
    let records = selected_records(&manifest, &request.package_ids)?;
    let mut packages = Vec::new();
    let mut not_updatable = Vec::new();

    for record in records {
        let Some(latest_version) = latest_update_version(paths, &record, http_client)? else {
            not_updatable.push(not_updatable_package(&record, "no update source"));
            continue;
        };

        match compare_versions(&record.version, &latest_version) {
            VersionComparison::CandidateIsNewer => packages.push(OutdatedPackage {
                package_id: record.package_id.clone(),
                current_version: record.version.clone(),
                latest_version,
            }),
            VersionComparison::Equal | VersionComparison::CurrentIsNewer => {}
            VersionComparison::Unknown => not_updatable.push(not_updatable_package(
                &record,
                format!(
                    "could not compare current version {} with latest version {}",
                    record.version.as_str(),
                    latest_version.as_str()
                ),
            )),
        }
    }

    Ok(OutdatedReport {
        packages,
        not_updatable,
    })
}

fn latest_update_version(
    paths: &FontbrewPaths,
    record: &ManifestPackageRecord,
    http_client: &dyn HttpClient,
) -> Result<Option<PackageVersion>> {
    if let Some(provider_id) = fontsource_provider_id(record) {
        return Ok(Some(
            FontsourceProvider::new(paths, http_client)
                .resolve_install_package(provider_id)?
                .version,
        ));
    }

    let Some(repo) = github_update_repo(record)? else {
        return Ok(None);
    };

    Ok(Some(github::resolve_latest_stable_release_version(
        http_client,
        &repo,
    )?))
}

fn selected_records(
    manifest: &crate::manifest::ManifestV1,
    package_ids: &[PackageId],
) -> Result<Vec<ManifestPackageRecord>> {
    if package_ids.is_empty() {
        return Ok(manifest.packages.values().cloned().collect());
    }

    let mut records = Vec::with_capacity(package_ids.len());
    for package_id in package_ids {
        let record = manifest
            .get_package(package_id)
            .ok_or_else(|| package_not_installed_error(package_id))?;
        records.push(record.clone());
    }

    Ok(records)
}

pub fn update_plan(
    paths: &FontbrewPaths,
    request: UpdateRequest,
    http_client: &dyn HttpClient,
    progress: &mut dyn ProgressSink,
    cancellation: &dyn CancellationToken,
) -> Result<UpdatePlan> {
    ensure_not_cancelled(cancellation)?;
    install::cleanup_stale_install_staging(paths)?;
    ensure_not_cancelled(cancellation)?;

    let manifest = ManifestStore::read_or_empty(&paths.manifest_path())?;
    let records = selected_records(&manifest, &request.package_ids)?;
    let config_jobs = FontbrewConfig::load(&paths.config_path())?.update_concurrency;
    let jobs = request.jobs.unwrap_or(config_jobs).max(1);
    ensure_not_cancelled(cancellation)?;

    for record in &records {
        progress.emit(ProgressEvent::PreparingUpdate {
            package_id: record.package_id.clone(),
        });
    }

    let outcomes = tasks::map_bounded(records, jobs, |record| {
        prepare_update_package(paths, record, http_client, cancellation)
    });

    let mut prepared = Vec::new();
    let mut prepared_packages = Vec::new();
    let mut failed = Vec::new();
    let mut risks = Vec::new();

    for outcome in outcomes {
        match outcome {
            PrepareOutcome::Prepared(prepared_update) => {
                let prepared_update = *prepared_update;
                risks.extend(prepared_update.prepared.activation_risks.clone());
                prepared.push(prepared_update.summary.clone());
                prepared_packages.push(prepared_update);
            }
            PrepareOutcome::Failed(failure) => failed.push(failure),
            PrepareOutcome::UpToDate => {}
        }
    }
    if let Err(error) = ensure_not_cancelled(cancellation) {
        for prepared_update in &prepared_packages {
            install::cleanup_staging(&prepared_update.prepared.staging_dir);
        }
        return Err(error);
    }

    let changes = prepared
        .iter()
        .flat_map(|package| {
            [
                PlannedChange {
                    package_id: package.package_id.clone(),
                    description: format!(
                        "prepare update from {} to {}",
                        package.current_version.as_str(),
                        package.target_version.as_str()
                    ),
                },
                PlannedChange {
                    package_id: package.package_id.clone(),
                    description: "replace activation artifacts".to_string(),
                },
                PlannedChange {
                    package_id: package.package_id.clone(),
                    description: "record updated version in manifest".to_string(),
                },
            ]
        })
        .collect();

    Ok(UpdatePlan {
        operation_id: new_operation_id()?,
        changes,
        risks,
        prepared,
        failed,
        prepared_packages,
    })
}

enum PrepareOutcome {
    Prepared(Box<PreparedUpdatePackage>),
    Failed(UpdatePlanFailure),
    UpToDate,
}

fn prepare_update_package(
    paths: &FontbrewPaths,
    record: ManifestPackageRecord,
    http_client: &dyn HttpClient,
    cancellation: &dyn CancellationToken,
) -> PrepareOutcome {
    match prepare_update_package_inner(paths, &record, http_client, cancellation) {
        Ok(Some(prepared)) => PrepareOutcome::Prepared(Box::new(prepared)),
        Ok(None) => PrepareOutcome::UpToDate,
        Err(error) => PrepareOutcome::Failed(UpdatePlanFailure {
            package_id: record.package_id,
            reason: prepare_failure_reason(&error),
        }),
    }
}

fn prepare_update_package_inner(
    paths: &FontbrewPaths,
    record: &ManifestPackageRecord,
    http_client: &dyn HttpClient,
    cancellation: &dyn CancellationToken,
) -> Result<Option<PreparedUpdatePackage>> {
    ensure_not_cancelled(cancellation)?;
    if let Some(provider_id) = fontsource_provider_id(record) {
        return prepare_fontsource_update_package_inner(
            paths,
            record,
            provider_id,
            http_client,
            cancellation,
        );
    }

    let Some(repo) = github_update_repo(record)? else {
        return Err(FontbrewError::NoUpdateSource {
            package_id: record.package_id.clone(),
        });
    };

    ensure_not_cancelled(cancellation)?;
    let asset = github::resolve_release_asset(http_client, &repo, None, &record.package_id)?;
    ensure_not_cancelled(cancellation)?;
    match compare_versions(&record.version, &asset.version) {
        VersionComparison::Equal | VersionComparison::CurrentIsNewer => return Ok(None),
        VersionComparison::Unknown => {
            return Err(FontbrewError::Manifest {
                message: format!(
                    "could not compare current version {} with latest version {}",
                    record.version.as_str(),
                    asset.version.as_str()
                ),
            });
        }
        VersionComparison::CandidateIsNewer => {}
    }

    let source = prepared_source_for_update(record)?;
    let mut options = install::RemoteInstallOptions::for_update(record.package_id.clone());
    options.family_boundary =
        install::InstallFamilyBoundary::from_selected_families(record.families.clone());
    let mut prepared = install::prepare_resolved_github_release_archive(
        paths,
        asset,
        source,
        options,
        http_client,
        cancellation,
    )?;
    if let Err(error) = ensure_not_cancelled(cancellation) {
        install::cleanup_staging(&prepared.staging_dir);
        return Err(error);
    }

    if let Err(error) = validate_update_identity(record, &prepared, &record.families) {
        install::cleanup_staging(&prepared.staging_dir);
        return Err(error);
    }
    prepared.activation_risks = update_activation_risks(record, &prepared.activation_risks);

    Ok(Some(PreparedUpdatePackage {
        summary: UpdatePlanPackage {
            package_id: record.package_id.clone(),
            current_version: record.version.clone(),
            target_version: prepared.version.clone(),
        },
        prepared,
    }))
}

fn prepare_fontsource_update_package_inner(
    paths: &FontbrewPaths,
    record: &ManifestPackageRecord,
    provider_id: &str,
    http_client: &dyn HttpClient,
    cancellation: &dyn CancellationToken,
) -> Result<Option<PreparedUpdatePackage>> {
    ensure_not_cancelled(cancellation)?;
    let resolved =
        FontsourceProvider::new(paths, http_client).resolve_install_package(provider_id)?;
    ensure_not_cancelled(cancellation)?;
    match compare_versions(&record.version, &resolved.version) {
        VersionComparison::Equal | VersionComparison::CurrentIsNewer => return Ok(None),
        VersionComparison::Unknown => {
            return Err(FontbrewError::Manifest {
                message: format!(
                    "could not compare current version {} with latest version {}",
                    record.version.as_str(),
                    resolved.version.as_str()
                ),
            });
        }
        VersionComparison::CandidateIsNewer => {}
    }

    let mut options = install::RemoteInstallOptions::for_update(record.package_id.clone());
    options.family_boundary =
        install::InstallFamilyBoundary::from_selected_families(record.families.clone());
    let mut prepared = install::prepare_resolved_provider_package(
        paths,
        resolved,
        options,
        http_client,
        cancellation,
    )?;
    if let Err(error) = ensure_not_cancelled(cancellation) {
        install::cleanup_staging(&prepared.staging_dir);
        return Err(error);
    }

    if let Err(error) = validate_update_identity(record, &prepared, &record.families) {
        install::cleanup_staging(&prepared.staging_dir);
        return Err(error);
    }
    prepared.activation_risks = update_activation_risks(record, &prepared.activation_risks);

    Ok(Some(PreparedUpdatePackage {
        summary: UpdatePlanPackage {
            package_id: record.package_id.clone(),
            current_version: record.version.clone(),
            target_version: prepared.version.clone(),
        },
        prepared,
    }))
}

fn update_activation_risks(record: &ManifestPackageRecord, risks: &[PlanRisk]) -> Vec<PlanRisk> {
    let managed_activation_paths = record
        .activation_artifacts
        .iter()
        .map(|artifact| artifact.path.display().to_string())
        .collect::<Vec<_>>();

    risks
        .iter()
        .filter(|risk| match risk {
            PlanRisk::Conflict {
                package_id,
                description,
            } if package_id == &record.package_id => !managed_activation_paths
                .iter()
                .any(|path| description.contains(path)),
            _ => true,
        })
        .cloned()
        .collect()
}

fn prepared_source_for_update(record: &ManifestPackageRecord) -> Result<PreparedInstallSource> {
    match &record.source {
        ManifestSource::GitHub { owner, repo } => Ok(PreparedInstallSource::GitHub {
            owner: owner.clone(),
            repo: repo.clone(),
        }),
        ManifestSource::Provider { .. } | ManifestSource::LocalArchive { .. } => {
            Err(FontbrewError::NoUpdateSource {
                package_id: record.package_id.clone(),
            })
        }
    }
}

fn validate_update_identity(
    record: &ManifestPackageRecord,
    prepared: &PreparedInstallPackage,
    expected_families: &[FamilyName],
) -> Result<()> {
    if prepared.package_id != record.package_id {
        return Err(FontbrewError::PackageIdentityMismatch {
            package_id: record.package_id.clone(),
            expected: first_family(expected_families),
            found: first_family(&prepared.families),
        });
    }

    for expected_family in expected_families {
        if !prepared
            .families
            .iter()
            .any(|family| family_names_match(family, expected_family))
        {
            return Err(FontbrewError::PackageIdentityMismatch {
                package_id: record.package_id.clone(),
                expected: expected_family.clone(),
                found: first_family(&prepared.families),
            });
        }
    }

    Ok(())
}

fn family_names_match(left: &FamilyName, right: &FamilyName) -> bool {
    install::normalize_family_boundary_name(left.as_str())
        == install::normalize_family_boundary_name(right.as_str())
}

fn first_family(families: &[FamilyName]) -> FamilyName {
    families
        .first()
        .cloned()
        .unwrap_or_else(|| FamilyName::new("<none>"))
}

pub fn apply_update(
    paths: &FontbrewPaths,
    plan: UpdatePlan,
    policy: ExecutionPolicy,
    progress: &mut dyn ProgressSink,
    cancellation: &dyn CancellationToken,
) -> Result<UpdateReport> {
    if let Err(error) = ensure_not_cancelled(cancellation) {
        cleanup_update_plan(&plan);
        return Err(error);
    }

    if matches!(policy, ExecutionPolicy::DryRun) {
        cleanup_update_plan(&plan);
        return Ok(UpdateReport {
            operation_id: plan.operation_id,
            planned: plan.prepared,
            updated: Vec::new(),
            skipped: plan.failed,
        });
    }

    if let Err(error) = require_policy_for_risks(&plan.risks, &policy) {
        cleanup_update_plan(&plan);
        return Err(error);
    }

    let _lock = match GlobalFileLock::try_exclusive(&install::write_lock_path(paths)) {
        Ok(lock) => lock,
        Err(error) => {
            cleanup_update_plan(&plan);
            return Err(error);
        }
    };
    let mut manifest = match ManifestStore::read_or_empty(&paths.manifest_path()) {
        Ok(manifest) => manifest,
        Err(error) => {
            cleanup_update_plan(&plan);
            return Err(error);
        }
    };
    if let Err(error) = ensure_not_cancelled(cancellation) {
        cleanup_update_plan(&plan);
        return Err(error);
    }
    let mut updated = Vec::new();
    let mut skipped = plan.failed.clone();

    for prepared_update in &plan.prepared_packages {
        if let Err(error) = ensure_not_cancelled(cancellation) {
            cleanup_update_plan(&plan);
            return Err(error);
        }

        progress.emit(ProgressEvent::ApplyingUpdate {
            package_id: prepared_update.summary.package_id.clone(),
        });
        let result = apply_prepared_update(
            paths,
            &mut manifest,
            prepared_update,
            policy.clone(),
            progress,
            cancellation,
        );
        install::cleanup_staging(&prepared_update.prepared.staging_dir);

        match result {
            Ok(package) => updated.push(package),
            Err(error) => {
                if matches!(error, FontbrewError::Cancelled) {
                    cleanup_update_plan(&plan);
                    return Err(error);
                }
                skipped.push(UpdatePlanFailure {
                    package_id: prepared_update.summary.package_id.clone(),
                    reason: error.to_string(),
                });
                manifest = match ManifestStore::read_or_empty(&paths.manifest_path()) {
                    Ok(manifest) => manifest,
                    Err(error) => {
                        cleanup_update_plan(&plan);
                        return Err(error);
                    }
                };
            }
        }
    }

    Ok(UpdateReport {
        operation_id: plan.operation_id,
        planned: Vec::new(),
        updated,
        skipped,
    })
}

pub fn discard_update_plan(plan: UpdatePlan) {
    cleanup_update_plan(&plan);
}

fn cleanup_update_plan(plan: &UpdatePlan) {
    for prepared_update in &plan.prepared_packages {
        install::cleanup_staging(&prepared_update.prepared.staging_dir);
    }
}

fn apply_prepared_update(
    paths: &FontbrewPaths,
    manifest: &mut ManifestV1,
    prepared_update: &PreparedUpdatePackage,
    policy: ExecutionPolicy,
    progress: &mut dyn ProgressSink,
    cancellation: &dyn CancellationToken,
) -> Result<UpdatedPackage> {
    ensure_not_cancelled(cancellation)?;
    let package_id = &prepared_update.summary.package_id;
    let current_record = manifest
        .get_package(package_id)
        .cloned()
        .ok_or_else(|| package_not_installed_error(package_id))?;

    if current_record.version != prepared_update.summary.current_version {
        return Err(FontbrewError::Manifest {
            message: format!(
                "package {} changed from {} to {} after update plan was prepared",
                package_id.as_str(),
                prepared_update.summary.current_version.as_str(),
                current_record.version.as_str()
            ),
        });
    }

    let prepared = &prepared_update.prepared;
    if prepared.package_store_dir.exists() {
        return Err(FontbrewError::Conflict {
            package_id: package_id.clone(),
            message: format!(
                "target package store already exists: {}",
                prepared.package_store_dir.display()
            ),
        });
    }

    ensure_not_cancelled(cancellation)?;
    if let Err(error) = install::copy_prepared_files(paths, prepared) {
        remove_package_store(paths, &prepared.package_store_dir);
        return Err(error);
    }

    let old_activation_artifacts = install::activation_artifacts_from_record(&current_record);
    let new_activation_plan = ActivationPlan {
        package_id: package_id.clone(),
        activation_dir: prepared.activation_dir.clone(),
        strategy: prepared.activation_strategy,
        artifacts: prepared.activation_artifacts.clone(),
        risks: Vec::new(),
    };
    if let Err(error) = ensure_not_cancelled(cancellation) {
        remove_package_store(paths, &prepared.package_store_dir);
        return Err(error);
    }
    let new_activation_artifacts = match replace_activation(
        paths,
        package_id,
        &old_activation_artifacts,
        &new_activation_plan,
        policy,
    ) {
        Ok(artifacts) => artifacts,
        Err(error) => {
            remove_package_store(paths, &prepared.package_store_dir);
            return Err(error);
        }
    };

    if let Err(error) = ensure_not_cancelled(cancellation) {
        let cleanup_error = deactivate(&paths.activation_dir(), &new_activation_artifacts).err();
        let restore_error = restore_activation(paths, package_id, &old_activation_artifacts).err();
        remove_package_store(paths, &prepared.package_store_dir);
        if cleanup_error.is_none() && restore_error.is_none() {
            return Err(error);
        }
        return Err(activation_transaction_error(
            package_id,
            "cancel after activate new activation",
            error,
            cleanup_error,
            restore_error,
        ));
    }
    let new_manifest_record =
        install::manifest_record_from_prepared(prepared, new_activation_artifacts.clone())?;
    manifest.insert_package(new_manifest_record)?;
    if let Err(error) = ManifestStore::write_with_commit_status(&paths.manifest_path(), manifest) {
        return match error.commit_status {
            AtomicWriteCommitStatus::NotCommitted => {
                let cleanup_error = deactivate(&paths.activation_dir(), &new_activation_artifacts)
                    .err();
                let restore_error = restore_activation(paths, package_id, &old_activation_artifacts)
                    .err();
                remove_package_store(paths, &prepared.package_store_dir);
                manifest.insert_package(current_record.clone())?;

                Err(manifest_write_not_committed_error(
                    package_id,
                    error.error,
                    cleanup_error,
                    restore_error,
                ))
            }
            AtomicWriteCommitStatus::Uncertain => Err(FontbrewError::Manifest {
                message: format!(
                    "manifest write failed after installing new files and activation; kept new files and activation because commit state is uncertain: {}",
                    error.error
                ),
            }),
        };
    }

    let old_package_store_dir =
        paths.package_store_dir(&current_record.package_id, &current_record.version);
    if old_package_store_dir != prepared.package_store_dir {
        remove_package_store(paths, &old_package_store_dir);
    }

    progress.emit(ProgressEvent::FinishedPackage {
        package_id: package_id.clone(),
    });

    Ok(UpdatedPackage {
        package_id: package_id.clone(),
        previous_version: current_record.version,
        installed_version: prepared_update.summary.target_version.clone(),
    })
}

fn replace_activation(
    paths: &FontbrewPaths,
    package_id: &PackageId,
    old_artifacts: &[ActivationArtifact],
    new_plan: &ActivationPlan,
    policy: ExecutionPolicy,
) -> Result<Vec<ActivationArtifact>> {
    let mut removed_old_artifacts = Vec::new();
    for old_artifact in old_artifacts {
        if let Err(error) = deactivate(&paths.activation_dir(), std::slice::from_ref(old_artifact))
        {
            let restore_error = restore_activation(paths, package_id, &removed_old_artifacts).err();
            return Err(activation_transaction_error(
                package_id,
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
                    deactivate(&paths.activation_dir(), &created_new_artifacts).err();
                let restore_error = restore_activation(paths, package_id, old_artifacts).err();
                return Err(activation_transaction_error(
                    package_id,
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

fn restore_activation(
    paths: &FontbrewPaths,
    package_id: &PackageId,
    artifacts: &[ActivationArtifact],
) -> Result<()> {
    if artifacts.is_empty() {
        return Ok(());
    }

    let plan = ActivationPlan {
        package_id: package_id.clone(),
        activation_dir: paths.activation_dir(),
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

fn manifest_write_not_committed_error(
    package_id: &PackageId,
    primary_error: FontbrewError,
    cleanup_error: Option<FontbrewError>,
    restore_error: Option<FontbrewError>,
) -> FontbrewError {
    let mut message = format!(
        "manifest write did not commit; restored old activation and removed new package files: {primary_error}"
    );

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

fn remove_package_store(paths: &FontbrewPaths, package_store_dir: &std::path::Path) {
    if ensure_existing_path_does_not_cross_symlink(&paths.managed_store_dir(), package_store_dir)
        .is_err()
    {
        return;
    }

    let _ = fs::remove_dir_all(package_store_dir);
}

fn prepare_failure_reason(error: &FontbrewError) -> String {
    match error {
        FontbrewError::NoUpdateSource { .. } => "no update source".to_string(),
        FontbrewError::ArchiveRejected { reason }
            if reason.contains("missing selected font families") =>
        {
            format!("identity mismatch: {reason}")
        }
        other => other.to_string(),
    }
}

fn require_policy_for_risks(risks: &[PlanRisk], policy: &ExecutionPolicy) -> Result<()> {
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

fn new_operation_id() -> Result<OperationId> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| FontbrewError::PathResolution {
            message: format!("system clock is before unix epoch: {error}"),
        })?
        .as_nanos();
    let counter = UPDATE_OPERATION_COUNTER.fetch_add(1, Ordering::Relaxed);

    Ok(OperationId::new(format!("update-{timestamp}-{counter}")))
}

fn fontsource_provider_id(record: &ManifestPackageRecord) -> Option<&str> {
    match &record.source {
        ManifestSource::Provider {
            provider: ProviderKind::Fontsource,
            id,
        } => Some(id),
        _ => None,
    }
}

fn github_update_repo(record: &ManifestPackageRecord) -> Result<Option<GitHubRepo>> {
    match record.update_source.as_ref().unwrap_or(&record.source) {
        ManifestSource::GitHub { owner, repo } => GitHubRepo::parse(format!("{owner}/{repo}"))
            .map(Some)
            .map_err(|error| FontbrewError::Manifest {
                message: format!(
                    "manifest package {:?} has invalid GitHub update source: {error}",
                    record.package_id
                ),
            }),
        ManifestSource::Provider { .. } | ManifestSource::LocalArchive { .. } => Ok(None),
    }
}

fn not_updatable_package(
    record: &ManifestPackageRecord,
    reason: impl Into<String>,
) -> NotUpdatablePackage {
    NotUpdatablePackage {
        package_id: record.package_id.clone(),
        reason: reason.into(),
    }
}

fn package_not_installed_error(package_id: &PackageId) -> FontbrewError {
    FontbrewError::Manifest {
        message: format!("package is not installed: {:?}", package_id),
    }
}
