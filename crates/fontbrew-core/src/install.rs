use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{
    activation::{
        deactivate, ActivationArtifact, ActivationPlan, ActivationPlanner, ActivationRequest,
    },
    archives::{ArchiveExtractionOptions, ExtractedFontFile, ZipArchiveExtractor},
    config::{dedupe_formats, font_format_label, FontbrewConfig},
    error::{FontbrewError, Result},
    fetch::NetworkClient,
    fonts::{FontFaceMetadata, FontFileFormat, FontMetadataReader, TtfParserMetadataReader},
    fs::{ensure_existing_path_does_not_cross_symlink, GlobalFileLock},
    github,
    manifest::{
        ManifestActivationArtifactRecord, ManifestFontFileFormat, ManifestFontFileRecord,
        ManifestPackageRecord, ManifestSource, ManifestStore, ManifestV1,
    },
    model::{
        ensure_not_cancelled, CancellationToken, ExecutionPolicy, FontFormat, InfoReport,
        InfoRequest, InstallPlan, InstallReport, InstallRequest, InstallSource, ListPackage,
        ListReport, ManagedActivationArtifact, ManagedFontFile, NoProgress, PackageInfo,
        PlannedChange, PreparedFontFace, PreparedFontFile, PreparedInstallPackage,
        PreparedInstallSource, ProgressEvent, ProgressSink, RemovePlan, RemoveReport,
        RemoveRequest,
    },
    platform::FontbrewPaths,
    providers::{self, FontsourceProvider, ResolvedProviderPackage},
    sources::GitHubRepo,
    FamilyName, PackageId, PackageVersion, PlanRisk, ProviderKind,
};

const LOCAL_ARCHIVE_VERSION: &str = "local";
const MAX_PROVIDER_FONT_DOWNLOAD_BYTES: u64 = 64 * 1024 * 1024;
const MAX_PROVIDER_TOTAL_DOWNLOAD_BYTES: u64 = 512 * 1024 * 1024;
const MAX_PROVIDER_FONT_FILES: usize = 256;
const ACTIVE_STAGING_MARKER: &str = ".fontbrew-active";
const ACTIVE_STAGING_LEASE_SECS: u64 = 7 * 24 * 60 * 60;
static OPERATION_COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn install_plan_with_progress(
    paths: &FontbrewPaths,
    request: InstallRequest,
    progress: &mut dyn ProgressSink,
    cancellation: &dyn CancellationToken,
) -> Result<InstallPlan> {
    ensure_not_cancelled(cancellation)?;
    ensure_package_id_override_allowed_for_source(&request)?;
    cleanup_stale_install_staging(paths)?;
    ensure_not_cancelled(cancellation)?;

    let InstallRequest {
        source,
        package_id_override,
        format_preference,
        selected_families,
        reinstall,
        ..
    } = request;

    match source {
        InstallSource::LocalPath(path) => {
            progress.emit(ProgressEvent::ResolvingSource {
                source: path.display().to_string(),
            });
            local_archive_install_plan(
                paths,
                path,
                package_id_override,
                reinstall,
                format_preference,
                selected_families,
                progress,
                cancellation,
            )
        }
        _ => Err(FontbrewError::NotImplemented {
            operation: "install_source",
        }),
    }
}

pub fn install_plans_with_progress(
    paths: &FontbrewPaths,
    request: InstallRequest,
    progress: &mut dyn ProgressSink,
    cancellation: &dyn CancellationToken,
) -> Result<Vec<InstallPlan>> {
    ensure_not_cancelled(cancellation)?;
    ensure_package_id_override_allowed_for_source(&request)?;
    cleanup_stale_install_staging(paths)?;
    ensure_not_cancelled(cancellation)?;

    let InstallRequest {
        source,
        package_id_override,
        format_preference,
        selected_families,
        reinstall,
        ..
    } = request;

    match source {
        InstallSource::LocalPath(path) => {
            progress.emit(ProgressEvent::ResolvingSource {
                source: path.display().to_string(),
            });
            local_archive_install_plans(
                paths,
                path,
                package_id_override,
                reinstall,
                format_preference,
                selected_families,
                progress,
                cancellation,
            )
        }
        _ => Err(FontbrewError::NotImplemented {
            operation: "install_source",
        }),
    }
}

pub(crate) enum InstallPlanCandidate {
    Plan(InstallPlan),
    FamilySelection {
        parsed_archive: ParsedFontArchive,
        package_id_override: Option<PackageId>,
    },
}

pub(crate) fn ensure_package_id_override_allowed_for_source(
    request: &InstallRequest,
) -> Result<()> {
    if request.package_id_override.is_some()
        && !matches!(
            request.source,
            InstallSource::LocalPath(_) | InstallSource::GitHubRepo { .. }
        )
    {
        return Err(package_id_override_unsupported_source_error());
    }

    if request.package_id_override.is_some() && !request.selected_families.is_empty() {
        return Err(FontbrewError::Config {
            message: "--id cannot be combined with --family".to_string(),
        });
    }

    if !request.selected_families.is_empty()
        && !matches!(
            request.source,
            InstallSource::LocalPath(_) | InstallSource::GitHubRepo { .. }
        )
    {
        return Err(FontbrewError::Config {
            message: "--family is only supported for direct GitHub and local archive sources"
                .to_string(),
        });
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn local_archive_install_plan_candidate(
    paths: &FontbrewPaths,
    archive_path: PathBuf,
    package_id_override: Option<PackageId>,
    reinstall: bool,
    format_preference: Vec<FontFormat>,
    selected_families: Vec<FamilyName>,
    progress: &mut dyn ProgressSink,
    cancellation: &dyn CancellationToken,
) -> Result<InstallPlanCandidate> {
    let archive_path = resolve_local_archive_path(&archive_path)?;
    progress.emit(ProgressEvent::ResolvingSource {
        source: archive_path.display().to_string(),
    });
    ensure_not_cancelled(cancellation)?;
    let family_boundary = InstallFamilyBoundary::from_selected_families(selected_families);
    let parsed_archive = prepare_local_archive_as_parsed_archive(
        paths,
        archive_path,
        package_id_override.clone(),
        reinstall,
        format_preference,
        progress,
        cancellation,
    )?;

    install_plan_candidate_from_parsed_archive(
        paths,
        parsed_archive,
        package_id_override.clone(),
        package_id_override,
        family_boundary.as_ref(),
        None,
        progress,
        cancellation,
    )
}

pub async fn github_repo_install_plan(
    paths: &FontbrewPaths,
    repo: GitHubRepo,
    request: InstallRequest,
    progress: &mut dyn ProgressSink,
    network_client: &NetworkClient,
    cancellation: Arc<dyn CancellationToken>,
) -> Result<InstallPlan> {
    ensure_not_cancelled(cancellation.as_ref())?;
    cleanup_stale_install_staging(paths)?;
    ensure_not_cancelled(cancellation.as_ref())?;

    let has_selected_families = !request.selected_families.is_empty();
    let options = RemoteInstallOptions::from_request(request)?;
    let package_id = github_install_package_id(&repo, &options)?;
    let requested_source = ManifestSource::GitHub {
        owner: repo.owner.clone(),
        repo: repo.repo.clone(),
    };
    let requested_update_source = Some(requested_source.clone());
    if !has_selected_families {
        if let Some(plan) = already_installed_plan(
            paths,
            &package_id,
            options.reinstall,
            &requested_source,
            requested_update_source.as_ref(),
        )? {
            return Ok(plan);
        }
    }

    progress.emit(ProgressEvent::ResolvingSource {
        source: repo.label(),
    });
    let options = if has_selected_families {
        options.with_progress_package_id(package_id.clone())
    } else {
        options.with_package_id(package_id.clone())
    };
    let prepared = prepare_github_release_archive(
        paths,
        &repo,
        package_id.clone(),
        PreparedInstallSource::GitHub {
            owner: repo.owner.clone(),
            repo: repo.repo.clone(),
        },
        options,
        progress,
        network_client,
        cancellation.clone(),
    )
    .await?;
    ensure_not_cancelled_after_prepare(cancellation.as_ref(), &prepared)?;

    install_plan_from_prepared(paths, prepared, progress)
}

pub async fn github_repo_install_plan_candidate(
    paths: &FontbrewPaths,
    repo: GitHubRepo,
    request: InstallRequest,
    progress: &mut dyn ProgressSink,
    network_client: &NetworkClient,
    cancellation: Arc<dyn CancellationToken>,
) -> Result<InstallPlanCandidate> {
    ensure_not_cancelled(cancellation.as_ref())?;
    cleanup_stale_install_staging(paths)?;
    ensure_not_cancelled(cancellation.as_ref())?;

    let has_selected_families = !request.selected_families.is_empty();
    let options = RemoteInstallOptions::from_request(request)?;
    let package_id_override = options.package_id.clone();
    let package_id = github_install_package_id(&repo, &options)?;
    let requested_source = ManifestSource::GitHub {
        owner: repo.owner.clone(),
        repo: repo.repo.clone(),
    };
    let requested_update_source = Some(requested_source.clone());
    if !has_selected_families {
        if let Some(plan) = already_installed_plan(
            paths,
            &package_id,
            options.reinstall,
            &requested_source,
            requested_update_source.as_ref(),
        )? {
            return Ok(InstallPlanCandidate::Plan(plan));
        }
    }

    progress.emit(ProgressEvent::ResolvingSource {
        source: repo.label(),
    });
    let options = if has_selected_families {
        options.with_progress_package_id(package_id.clone())
    } else {
        options.with_package_id(package_id.clone())
    };
    let family_boundary = options.family_boundary.clone();
    let package_id_hint = options.package_id.clone();
    let parsed_archive = prepare_github_release_parsed_archive(
        paths,
        &repo,
        package_id,
        PreparedInstallSource::GitHub {
            owner: repo.owner.clone(),
            repo: repo.repo.clone(),
        },
        options,
        progress,
        network_client,
        cancellation.clone(),
    )
    .await?;

    install_plan_candidate_from_parsed_archive(
        paths,
        parsed_archive,
        package_id_hint,
        package_id_override,
        family_boundary.as_ref(),
        None,
        progress,
        cancellation.as_ref(),
    )
}

pub async fn github_repo_install_plans(
    paths: &FontbrewPaths,
    repo: GitHubRepo,
    request: InstallRequest,
    progress: &mut dyn ProgressSink,
    network_client: &NetworkClient,
    cancellation: Arc<dyn CancellationToken>,
) -> Result<Vec<InstallPlan>> {
    ensure_not_cancelled(cancellation.as_ref())?;
    cleanup_stale_install_staging(paths)?;
    ensure_not_cancelled(cancellation.as_ref())?;

    let selected_families = request.selected_families.clone();
    let options = RemoteInstallOptions::from_request(request)?;
    let package_id = github_install_package_id(&repo, &options)?;
    progress.emit(ProgressEvent::ResolvingSource {
        source: repo.label(),
    });
    let options = options.with_progress_package_id(package_id.clone());
    let parsed_archive = prepare_github_release_parsed_archive(
        paths,
        &repo,
        package_id,
        PreparedInstallSource::GitHub {
            owner: repo.owner.clone(),
            repo: repo.repo.clone(),
        },
        options,
        progress,
        network_client,
        cancellation.clone(),
    )
    .await?;

    family_install_plans_from_parsed_archive(
        paths,
        parsed_archive,
        selected_families,
        progress,
        cancellation.as_ref(),
    )
}

pub async fn fontsource_install_plan(
    paths: &FontbrewPaths,
    provider_id: String,
    request: InstallRequest,
    progress: &mut dyn ProgressSink,
    network_client: &NetworkClient,
    cancellation: Arc<dyn CancellationToken>,
) -> Result<InstallPlan> {
    ensure_not_cancelled(cancellation.as_ref())?;
    cleanup_stale_install_staging(paths)?;
    ensure_not_cancelled(cancellation.as_ref())?;

    let options = RemoteInstallOptions::from_request(request)?;
    let package_id = PackageId::parse(&provider_id)?;
    let requested_source = ManifestSource::Provider {
        provider: ProviderKind::Fontsource,
        id: provider_id.clone(),
    };
    if let Some(plan) = already_installed_plan(
        paths,
        &package_id,
        options.reinstall,
        &requested_source,
        None,
    )? {
        return Ok(plan);
    }

    if options.asset_selector.is_some() {
        return Err(FontbrewError::Config {
            message: "--asset is not supported for Fontsource provider sources".to_string(),
        });
    }

    progress.emit(ProgressEvent::ResolvingSource {
        source: format!("fontsource:{provider_id}"),
    });
    let resolved = FontsourceProvider::new(paths, network_client)
        .resolve_install_package(&provider_id)
        .await?;
    let prepared = prepare_provider_package(
        paths,
        resolved,
        options,
        progress,
        network_client,
        cancellation.clone(),
    )
    .await?;
    ensure_not_cancelled_after_prepare(cancellation.as_ref(), &prepared)?;

    install_plan_from_prepared(paths, prepared, progress)
}

fn already_installed_plan(
    paths: &FontbrewPaths,
    package_id: &PackageId,
    reinstall: bool,
    requested_source: &ManifestSource,
    requested_update_source: Option<&ManifestSource>,
) -> Result<Option<InstallPlan>> {
    if reinstall {
        return Ok(None);
    }

    let manifest = ManifestStore::read_or_empty(&paths.manifest_path())?;
    let Some(record) = manifest.get_package(package_id) else {
        return Ok(None);
    };

    if let Some(risk) = source_conflict_risk(record, requested_source, requested_update_source) {
        return Ok(Some(source_conflict_plan(
            package_id.clone(),
            record.version.clone(),
            risk,
        )));
    }

    Ok(Some(InstallPlan {
        package_id: package_id.clone(),
        target_version: Some(record.version.clone()),
        changes: Vec::new(),
        risks: Vec::new(),
        already_installed: true,
        prepared: None,
    }))
}

pub fn apply_install(
    paths: &FontbrewPaths,
    plan: InstallPlan,
    policy: ExecutionPolicy,
    progress: &mut dyn ProgressSink,
    cancellation: &dyn CancellationToken,
) -> Result<InstallReport> {
    if let Err(error) = ensure_not_cancelled(cancellation) {
        cleanup_install_plan_staging(&plan);
        return Err(error);
    }

    if matches!(policy, ExecutionPolicy::DryRun) {
        return dry_run_install_report(plan);
    }

    let planned_risks = plan.risks.clone();
    let package_id = plan.package_id.clone();

    if let Err(error) = require_policy_for_risks(&plan.risks, &policy) {
        cleanup_install_plan_staging(&plan);
        return Err(error);
    }

    if let Some(description) = first_blocking_conflict_description(&plan.risks) {
        cleanup_install_plan_staging(&plan);
        return Err(FontbrewError::Conflict {
            package_id,
            message: description,
        });
    }

    let _lock = match GlobalFileLock::try_exclusive(&write_lock_path(paths)) {
        Ok(lock) => lock,
        Err(error) => {
            cleanup_install_plan_staging(&plan);
            return Err(error);
        }
    };
    let mut manifest = match ManifestStore::read_or_empty(&paths.manifest_path()) {
        Ok(manifest) => manifest,
        Err(error) => {
            cleanup_install_plan_staging(&plan);
            return Err(error);
        }
    };
    if let Err(error) = ensure_not_cancelled(cancellation) {
        cleanup_install_plan_staging(&plan);
        return Err(error);
    }

    if plan.already_installed {
        let record = manifest
            .get_package(&plan.package_id)
            .ok_or_else(|| package_not_installed_error(&plan.package_id))?;
        return Ok(install_report_from_record(record, false, true));
    }

    let Some(prepared) = plan.prepared else {
        return Err(FontbrewError::Manifest {
            message: format!(
                "install plan for {:?} has no prepared package",
                plan.package_id
            ),
        });
    };

    let mut current_risks = planned_risks;
    if let Err(error) = ensure_not_cancelled(cancellation) {
        cleanup_staging(&prepared.staging_dir);
        return Err(error);
    }
    progress.emit(ProgressEvent::CheckingInstallRisks {
        package_id: prepared_package_id(&prepared),
    });
    match current_install_risks(paths, &manifest, &prepared) {
        Ok(risks) => current_risks.extend(risks),
        Err(error) => {
            cleanup_staging(&prepared.staging_dir);
            return Err(error);
        }
    }
    if let Err(error) = require_policy_for_risks(&current_risks, &policy) {
        cleanup_staging(&prepared.staging_dir);
        return Err(error);
    }
    if let Some(description) = first_blocking_conflict_description(&current_risks) {
        cleanup_staging(&prepared.staging_dir);
        return Err(FontbrewError::Conflict {
            package_id: prepared_package_id(&prepared),
            message: description,
        });
    }

    let requested_source = manifest_source_from_prepared(&prepared.source);
    let requested_update_source = manifest_update_source_from_prepared(&prepared.source);
    if let Some(record) = manifest.get_package(&prepared_package_id(&prepared)) {
        if let Some(risk) =
            source_conflict_risk(record, &requested_source, requested_update_source.as_ref())
        {
            cleanup_staging(&prepared.staging_dir);
            return Err(conflict_error_from_risk(
                &prepared_package_id(&prepared),
                &risk,
            ));
        }

        if !prepared.reinstall {
            cleanup_staging(&prepared.staging_dir);
            return Ok(install_report_from_record(record, false, true));
        }
    }

    let result = apply_prepared_install(
        paths,
        &mut manifest,
        &prepared,
        policy,
        progress,
        cancellation,
    );
    cleanup_staging(&prepared.staging_dir);

    result
}

pub fn discard_install_plan(plan: InstallPlan) {
    cleanup_install_plan_staging(&plan);
}

pub fn list_packages(paths: &FontbrewPaths) -> Result<ListReport> {
    let manifest = ManifestStore::read_or_empty(&paths.manifest_path())?;
    let packages = manifest
        .packages
        .values()
        .map(|record| ListPackage {
            package_id: record.package_id.clone(),
            version: record.version.clone(),
            families: package_families_for_report(paths, record),
            activated: record.active_version.is_some(),
        })
        .collect();

    Ok(ListReport { packages })
}

pub fn package_info(paths: &FontbrewPaths, request: InfoRequest) -> Result<InfoReport> {
    let manifest = ManifestStore::read_or_empty(&paths.manifest_path())?;
    let record = manifest
        .get_package(&request.package_id)
        .ok_or_else(|| package_not_installed_error(&request.package_id))?;

    Ok(InfoReport {
        package: PackageInfo {
            package_id: record.package_id.clone(),
            version: record.version.clone(),
            families: package_families_for_report(paths, record),
            source: source_label(&record.source),
            activated: record.active_version.is_some(),
            update_source: record.update_source.as_ref().map(source_label),
            managed: true,
            update_available: None,
            font_files: managed_font_files_from_record(record),
            activation_artifacts: managed_activation_artifacts_from_record(record),
        },
    })
}

fn package_families_for_report(
    paths: &FontbrewPaths,
    record: &ManifestPackageRecord,
) -> Vec<FamilyName> {
    if let ManifestSource::Provider {
        provider: ProviderKind::Fontsource,
        id,
    } = &record.source
    {
        if let Some(family) = providers::cached_fontsource_family(paths, id) {
            return vec![family];
        }
    }

    record.families.clone()
}

pub fn remove_plan(paths: &FontbrewPaths, request: RemoveRequest) -> Result<RemovePlan> {
    remove_plan_with_cancellation(paths, request, &crate::model::NoCancellation)
}

pub fn remove_plan_with_cancellation(
    paths: &FontbrewPaths,
    request: RemoveRequest,
    cancellation: &dyn CancellationToken,
) -> Result<RemovePlan> {
    ensure_not_cancelled(cancellation)?;
    let manifest = ManifestStore::read_or_empty(&paths.manifest_path())?;
    ensure_not_cancelled(cancellation)?;
    let (changes, font_files, activation_artifacts) = manifest
        .get_package(&request.package_id)
        .map(|record| {
            (
                vec![
                    PlannedChange {
                        package_id: request.package_id.clone(),
                        description: "deactivate managed font artifacts".to_string(),
                    },
                    PlannedChange {
                        package_id: request.package_id.clone(),
                        description: format!(
                            "remove managed package files for version {}",
                            record.version.as_str()
                        ),
                    },
                    PlannedChange {
                        package_id: request.package_id.clone(),
                        description: "remove package from manifest".to_string(),
                    },
                ],
                managed_font_files_from_record(record),
                managed_activation_artifacts_from_record(record),
            )
        })
        .unwrap_or_default();

    Ok(RemovePlan {
        package_id: request.package_id,
        changes,
        risks: Vec::new(),
        font_files,
        activation_artifacts,
    })
}

pub fn apply_remove(
    paths: &FontbrewPaths,
    plan: RemovePlan,
    policy: ExecutionPolicy,
    progress: &mut dyn ProgressSink,
    cancellation: &dyn CancellationToken,
) -> Result<RemoveReport> {
    ensure_not_cancelled(cancellation)?;
    require_policy_for_risks(&plan.risks, &policy)?;

    if matches!(policy, ExecutionPolicy::DryRun) {
        let planned = !plan.changes.is_empty();
        return Ok(RemoveReport {
            package_id: plan.package_id,
            removed: false,
            planned,
            font_files: plan.font_files,
            activation_artifacts: plan.activation_artifacts,
        });
    }

    let _lock = GlobalFileLock::try_exclusive(&write_lock_path(paths))?;
    let mut manifest = ManifestStore::read_or_empty(&paths.manifest_path())?;
    ensure_not_cancelled(cancellation)?;
    let Some(record) = manifest.get_package(&plan.package_id).cloned() else {
        return Ok(RemoveReport {
            package_id: plan.package_id,
            removed: false,
            planned: false,
            font_files: Vec::new(),
            activation_artifacts: Vec::new(),
        });
    };
    let report_font_files = managed_font_files_from_record(&record);
    let report_activation_artifacts = managed_activation_artifacts_from_record(&record);

    let activation_artifacts = activation_artifacts_from_record(&record);
    ensure_not_cancelled(cancellation)?;
    deactivate(&paths.activation_dir(), &activation_artifacts)?;

    let package_store_dir = paths.package_store_dir(&record.package_id, &record.version);
    ensure_existing_path_does_not_cross_symlink(&paths.managed_store_dir(), &package_store_dir)?;
    if package_store_dir.exists() {
        fs::remove_dir_all(package_store_dir)?;
    }

    manifest.remove_package(&record.package_id);
    ManifestStore::write(&paths.manifest_path(), &manifest)?;
    progress.emit(ProgressEvent::FinishedPackage {
        package_id: record.package_id.clone(),
    });

    Ok(RemoveReport {
        package_id: record.package_id,
        removed: true,
        planned: false,
        font_files: report_font_files,
        activation_artifacts: report_activation_artifacts,
    })
}

#[allow(clippy::too_many_arguments)]
fn local_archive_install_plan(
    paths: &FontbrewPaths,
    archive_path: PathBuf,
    package_id_override: Option<PackageId>,
    reinstall: bool,
    format_preference: Vec<FontFormat>,
    selected_families: Vec<FamilyName>,
    progress: &mut dyn ProgressSink,
    cancellation: &dyn CancellationToken,
) -> Result<InstallPlan> {
    let archive_path = resolve_local_archive_path(&archive_path)?;
    ensure_not_cancelled(cancellation)?;
    let prepared = prepare_local_archive(
        paths,
        archive_path,
        package_id_override,
        reinstall,
        format_preference,
        selected_families,
        progress,
        cancellation,
    )?;
    ensure_not_cancelled_after_prepare(cancellation, &prepared)?;
    install_plan_from_prepared(paths, prepared, progress)
}

#[allow(clippy::too_many_arguments)]
fn local_archive_install_plans(
    paths: &FontbrewPaths,
    archive_path: PathBuf,
    package_id_override: Option<PackageId>,
    reinstall: bool,
    format_preference: Vec<FontFormat>,
    selected_families: Vec<FamilyName>,
    progress: &mut dyn ProgressSink,
    cancellation: &dyn CancellationToken,
) -> Result<Vec<InstallPlan>> {
    let archive_path = resolve_local_archive_path(&archive_path)?;
    ensure_not_cancelled(cancellation)?;
    let parsed_archive = prepare_local_archive_as_parsed_archive(
        paths,
        archive_path,
        package_id_override,
        reinstall,
        format_preference,
        progress,
        cancellation,
    )?;

    family_install_plans_from_parsed_archive(
        paths,
        parsed_archive,
        selected_families,
        progress,
        cancellation,
    )
}

fn install_plan_from_prepared(
    paths: &FontbrewPaths,
    prepared: PreparedInstallPackage,
    progress: &mut dyn ProgressSink,
) -> Result<InstallPlan> {
    let package_id = prepared_package_id(&prepared);
    let manifest = match ManifestStore::read_or_empty(&paths.manifest_path()) {
        Ok(manifest) => manifest,
        Err(error) => {
            cleanup_staging(&prepared.staging_dir);
            return Err(error);
        }
    };
    progress.emit(ProgressEvent::CheckingInstallRisks {
        package_id: package_id.clone(),
    });
    let unmanaged_overlap_risks =
        match unmanaged_same_family_overlap_risks(paths, &manifest, &prepared) {
            Ok(risks) => risks,
            Err(error) => {
                cleanup_staging(&prepared.staging_dir);
                return Err(error);
            }
        };

    install_plan_from_prepared_with_manifest(&manifest, prepared, unmanaged_overlap_risks)
}

fn install_plan_from_prepared_with_manifest(
    manifest: &ManifestV1,
    prepared: PreparedInstallPackage,
    unmanaged_overlap_risks: Vec<PlanRisk>,
) -> Result<InstallPlan> {
    let package_id = prepared_package_id(&prepared);
    let requested_source = manifest_source_from_prepared(&prepared.source);
    let requested_update_source = manifest_update_source_from_prepared(&prepared.source);

    if let Some(record) = manifest.get_package(&package_id) {
        if let Some(risk) =
            source_conflict_risk(record, &requested_source, requested_update_source.as_ref())
        {
            cleanup_staging(&prepared.staging_dir);
            return Ok(source_conflict_plan(
                package_id,
                record.version.clone(),
                risk,
            ));
        }

        if !prepared.reinstall {
            cleanup_staging(&prepared.staging_dir);
            return Ok(InstallPlan {
                package_id,
                target_version: Some(record.version.clone()),
                changes: Vec::new(),
                risks: Vec::new(),
                already_installed: true,
                prepared: None,
            });
        }
    }

    let risks = match current_install_risks_with_unmanaged_overlap_risks(
        manifest,
        &prepared,
        unmanaged_overlap_risks,
    ) {
        Ok(risks) => risks,
        Err(error) => {
            cleanup_staging(&prepared.staging_dir);
            return Err(error);
        }
    };

    Ok(InstallPlan {
        package_id: package_id.clone(),
        target_version: Some(prepared.version.clone()),
        changes: vec![
            PlannedChange {
                package_id: package_id.clone(),
                description: "stage fonts into managed package store".to_string(),
            },
            PlannedChange {
                package_id: package_id.clone(),
                description: "activate managed font files".to_string(),
            },
            PlannedChange {
                package_id,
                description: "record package in manifest".to_string(),
            },
        ],
        risks,
        already_installed: false,
        prepared: Some(prepared),
    })
}

fn ensure_not_cancelled_after_prepare(
    cancellation: &dyn CancellationToken,
    prepared: &PreparedInstallPackage,
) -> Result<()> {
    if let Err(error) = ensure_not_cancelled(cancellation) {
        cleanup_staging(&prepared.staging_dir);
        return Err(error);
    }

    Ok(())
}

fn source_conflict_plan(
    package_id: PackageId,
    target_version: PackageVersion,
    risk: PlanRisk,
) -> InstallPlan {
    InstallPlan {
        package_id: package_id.clone(),
        target_version: Some(target_version),
        changes: vec![PlannedChange {
            package_id,
            description:
                "refuse install because package is already managed from a different source"
                    .to_string(),
        }],
        risks: vec![risk],
        already_installed: true,
        prepared: None,
    }
}

#[allow(clippy::too_many_arguments)]
fn prepare_local_archive(
    paths: &FontbrewPaths,
    archive_path: PathBuf,
    package_id_override: Option<PackageId>,
    reinstall: bool,
    format_preference: Vec<FontFormat>,
    selected_families: Vec<FamilyName>,
    progress: &mut dyn ProgressSink,
    cancellation: &dyn CancellationToken,
) -> Result<PreparedInstallPackage> {
    ensure_not_cancelled(cancellation)?;
    let staging_dir = create_active_staging_dir(paths)?;
    let mut staging_cleanup = StagingCleanupGuard::new(staging_dir);
    ensure_not_cancelled(cancellation)?;
    let result = extract_and_parse_archive(
        paths,
        archive_path.clone(),
        staging_cleanup.path().to_path_buf(),
        PackageVersion::new(LOCAL_ARCHIVE_VERSION),
        PreparedInstallSource::LocalArchive { path: archive_path },
        package_id_override.clone(),
        package_id_override,
        reinstall,
        ArchiveFormatPreference {
            explicit_format_preference: format_preference,
        },
        InstallFamilyBoundary::from_selected_families(selected_families),
        progress,
        cancellation,
    );

    if result.is_ok() {
        staging_cleanup.disarm();
    }

    result
}

fn prepare_local_archive_as_parsed_archive(
    paths: &FontbrewPaths,
    archive_path: PathBuf,
    package_id_override: Option<PackageId>,
    reinstall: bool,
    format_preference: Vec<FontFormat>,
    progress: &mut dyn ProgressSink,
    cancellation: &dyn CancellationToken,
) -> Result<ParsedFontArchive> {
    ensure_not_cancelled(cancellation)?;
    let staging_dir = create_active_staging_dir(paths)?;
    let mut staging_cleanup = StagingCleanupGuard::new(staging_dir);
    ensure_not_cancelled(cancellation)?;
    let result = extract_archive_to_parsed_archive(
        paths,
        archive_path.clone(),
        staging_cleanup.path().to_path_buf(),
        PackageVersion::new(LOCAL_ARCHIVE_VERSION),
        PreparedInstallSource::LocalArchive { path: archive_path },
        package_id_override,
        reinstall,
        ArchiveFormatPreference {
            explicit_format_preference: format_preference,
        },
        progress,
        cancellation,
    );

    if result.is_ok() {
        staging_cleanup.disarm();
    }

    result
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteInstallOptions {
    pub(crate) asset_selector: Option<String>,
    pub(crate) package_id: Option<PackageId>,
    pub(crate) progress_package_id: Option<PackageId>,
    pub(crate) reinstall: bool,
    pub(crate) explicit_format_preference: Vec<FontFormat>,
    pub(crate) family_boundary: Option<InstallFamilyBoundary>,
}

impl RemoteInstallOptions {
    fn from_request(request: InstallRequest) -> Result<Self> {
        if request.package_id_override.is_some()
            && !matches!(request.source, InstallSource::GitHubRepo { .. })
        {
            return Err(package_id_override_unsupported_source_error());
        }

        Ok(Self {
            asset_selector: request.asset_selector,
            package_id: request.package_id_override,
            progress_package_id: None,
            reinstall: request.reinstall,
            explicit_format_preference: dedupe_formats(request.format_preference),
            family_boundary: InstallFamilyBoundary::from_selected_families(
                request.selected_families,
            ),
        })
    }

    fn with_package_id(mut self, package_id: PackageId) -> Self {
        if self.progress_package_id.is_none() {
            self.progress_package_id = Some(package_id.clone());
        }
        self.package_id = Some(package_id);
        self
    }

    fn with_progress_package_id(mut self, package_id: PackageId) -> Self {
        self.progress_package_id = Some(package_id);
        self
    }

    pub(crate) fn for_update(package_id: PackageId) -> Self {
        Self {
            asset_selector: None,
            progress_package_id: Some(package_id.clone()),
            package_id: Some(package_id),
            reinstall: false,
            explicit_format_preference: Vec::new(),
            family_boundary: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InstallFamilyBoundary {
    expected_families: Vec<FamilyName>,
    include_families: Vec<FamilyName>,
    exclude_families: Vec<FamilyName>,
    allows_extra_archive_families: bool,
    family_label: &'static str,
}

impl InstallFamilyBoundary {
    pub(crate) fn from_selected_families(families: Vec<FamilyName>) -> Option<Self> {
        let families = dedupe_family_names(families);
        if families.is_empty() {
            return None;
        }

        Some(Self {
            expected_families: families.clone(),
            include_families: families,
            exclude_families: Vec::new(),
            allows_extra_archive_families: true,
            family_label: "selected",
        })
    }

    fn expected_families(&self) -> &[FamilyName] {
        &self.expected_families
    }

    fn includes_family(&self, family: &FamilyName) -> bool {
        family_matches_any(&self.include_families, family)
    }

    fn excludes_family(&self, family: &FamilyName) -> bool {
        family_matches_any(&self.exclude_families, family)
    }

    fn allows_extra_archive_families(&self) -> bool {
        self.allows_extra_archive_families
    }

    fn family_label(&self) -> &'static str {
        self.family_label
    }
}

fn package_id_override_unsupported_source_error() -> FontbrewError {
    FontbrewError::Config {
        message: "--id is only supported for local archive and direct GitHub sources".to_string(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ArchiveFormatPreference {
    explicit_format_preference: Vec<FontFormat>,
}

struct StagingCleanupGuard {
    path: PathBuf,
    cleanup_on_drop: bool,
}

impl StagingCleanupGuard {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            cleanup_on_drop: true,
        }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn disarm(&mut self) {
        self.cleanup_on_drop = false;
    }
}

impl Drop for StagingCleanupGuard {
    fn drop(&mut self) {
        if self.cleanup_on_drop {
            cleanup_staging(&self.path);
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn prepare_github_release_archive(
    paths: &FontbrewPaths,
    repo: &GitHubRepo,
    fallback_package_id: PackageId,
    source: PreparedInstallSource,
    options: RemoteInstallOptions,
    progress: &mut dyn ProgressSink,
    network_client: &NetworkClient,
    cancellation: Arc<dyn CancellationToken>,
) -> Result<PreparedInstallPackage> {
    ensure_not_cancelled(cancellation.as_ref())?;
    let staging_dir = create_active_staging_dir(paths)?;
    let mut staging_cleanup = StagingCleanupGuard::new(staging_dir);
    ensure_not_cancelled(cancellation.as_ref())?;
    let result = download_and_parse_github_archive(
        paths,
        repo,
        fallback_package_id,
        source,
        options,
        progress,
        network_client,
        staging_cleanup.path().to_path_buf(),
        cancellation.clone(),
    )
    .await;

    if result.is_ok() {
        staging_cleanup.disarm();
    }

    result
}

#[allow(clippy::too_many_arguments)]
async fn prepare_github_release_parsed_archive(
    paths: &FontbrewPaths,
    repo: &GitHubRepo,
    fallback_package_id: PackageId,
    source: PreparedInstallSource,
    options: RemoteInstallOptions,
    progress: &mut dyn ProgressSink,
    network_client: &NetworkClient,
    cancellation: Arc<dyn CancellationToken>,
) -> Result<ParsedFontArchive> {
    ensure_not_cancelled(cancellation.as_ref())?;
    let staging_dir = create_active_staging_dir(paths)?;
    let mut staging_cleanup = StagingCleanupGuard::new(staging_dir);
    ensure_not_cancelled(cancellation.as_ref())?;
    let result = download_and_parse_github_archive_to_parsed_archive(
        paths,
        repo,
        fallback_package_id,
        source,
        options,
        progress,
        network_client,
        staging_cleanup.path().to_path_buf(),
        cancellation.clone(),
    )
    .await;

    if result.is_ok() {
        staging_cleanup.disarm();
    }

    result
}

async fn prepare_provider_package(
    paths: &FontbrewPaths,
    resolved: ResolvedProviderPackage,
    options: RemoteInstallOptions,
    progress: &mut dyn ProgressSink,
    network_client: &NetworkClient,
    cancellation: Arc<dyn CancellationToken>,
) -> Result<PreparedInstallPackage> {
    ensure_not_cancelled(cancellation.as_ref())?;
    let staging_dir = create_active_staging_dir(paths)?;
    let mut staging_cleanup = StagingCleanupGuard::new(staging_dir);
    ensure_not_cancelled(cancellation.as_ref())?;
    let result = download_and_parse_provider_fonts(
        paths,
        resolved,
        options,
        progress,
        network_client,
        staging_cleanup.path().to_path_buf(),
        cancellation.clone(),
    )
    .await;

    if result.is_ok() {
        staging_cleanup.disarm();
    }

    result
}

pub(crate) async fn prepare_resolved_provider_package(
    paths: &FontbrewPaths,
    resolved: ResolvedProviderPackage,
    options: RemoteInstallOptions,
    network_client: &NetworkClient,
    cancellation: Arc<dyn CancellationToken>,
) -> Result<PreparedInstallPackage> {
    let mut progress = NoProgress;
    prepare_provider_package(
        paths,
        resolved,
        options,
        &mut progress,
        network_client,
        cancellation,
    )
    .await
}

async fn download_and_parse_provider_fonts(
    paths: &FontbrewPaths,
    resolved: ResolvedProviderPackage,
    options: RemoteInstallOptions,
    progress: &mut dyn ProgressSink,
    network_client: &NetworkClient,
    staging_dir: PathBuf,
    cancellation: Arc<dyn CancellationToken>,
) -> Result<PreparedInstallPackage> {
    ensure_existing_path_does_not_cross_symlink(&paths.managed_store_dir(), &staging_dir)?;
    ensure_not_cancelled(cancellation.as_ref())?;
    fs::create_dir_all(&staging_dir)?;

    if resolved.assets.len() > MAX_PROVIDER_FONT_FILES {
        return Err(FontbrewError::ArchiveRejected {
            reason: format!(
                "provider package {} exceeds font file count limit",
                resolved.provider_id
            ),
        });
    }

    let mut total_downloaded = 0_u64;
    let mut staged_fonts = Vec::with_capacity(resolved.assets.len());
    progress.emit(ProgressEvent::DownloadStarted {
        package_id: resolved.package_id.clone(),
        bytes: None,
    });
    for asset in &resolved.assets {
        ensure_not_cancelled(cancellation.as_ref())?;
        let destination = staging_dir.join(&asset.file_name);
        ensure_path_inside(&staging_dir, &destination)?;
        let downloaded = network_client
            .download_to_file(
                providers::provider_asset_request(&asset.url),
                &destination,
                MAX_PROVIDER_FONT_DOWNLOAD_BYTES,
                cancellation.clone(),
            )
            .await?;
        total_downloaded = total_downloaded.checked_add(downloaded).ok_or_else(|| {
            FontbrewError::ArchiveRejected {
                reason: format!(
                    "provider package {} download size overflowed",
                    resolved.provider_id
                ),
            }
        })?;
        if total_downloaded > MAX_PROVIDER_TOTAL_DOWNLOAD_BYTES {
            return Err(FontbrewError::ArchiveRejected {
                reason: format!(
                    "provider package {} exceeds total download size limit",
                    resolved.provider_id
                ),
            });
        }

        progress.emit(ProgressEvent::DownloadProgress {
            package_id: resolved.package_id.clone(),
            downloaded: total_downloaded,
            total: None,
        });
        staged_fonts.push(StagedFontFile {
            path: destination,
            format: reader_format_from_font_format(asset.format),
            weight_override: asset.weight,
        });
    }

    ensure_not_cancelled(cancellation.as_ref())?;
    let (result, events) = parse_staged_provider_fonts_blocking(RemoteFontParseInput {
        paths: paths.clone(),
        staged_fonts,
        staging_dir,
        version: resolved.version,
        source: PreparedInstallSource::Provider {
            provider: resolved.provider,
            id: resolved.provider_id,
        },
        package_families: Some(resolved.families),
        package_id_hint: Some(resolved.package_id),
        reinstall: options.reinstall,
        archive_format_preference: ArchiveFormatPreference {
            explicit_format_preference: options.explicit_format_preference,
        },
        family_boundary: options.family_boundary,
        cancellation: cancellation.clone(),
    })
    .await?;
    replay_progress(progress, events);
    match result {
        Ok(prepared) => {
            ensure_not_cancelled_after_prepare(cancellation.as_ref(), &prepared)?;
            Ok(prepared)
        }
        Err(error) => Err(error),
    }
}

#[allow(clippy::too_many_arguments)]
async fn download_and_parse_github_archive(
    paths: &FontbrewPaths,
    repo: &GitHubRepo,
    fallback_package_id: PackageId,
    source: PreparedInstallSource,
    options: RemoteInstallOptions,
    progress: &mut dyn ProgressSink,
    network_client: &NetworkClient,
    staging_dir: PathBuf,
    cancellation: Arc<dyn CancellationToken>,
) -> Result<PreparedInstallPackage> {
    ensure_existing_path_does_not_cross_symlink(&paths.managed_store_dir(), &staging_dir)?;
    ensure_not_cancelled(cancellation.as_ref())?;

    let asset = github::resolve_release_asset(
        network_client,
        repo,
        options.asset_selector.as_deref(),
        &fallback_package_id,
    )
    .await?;
    ensure_not_cancelled(cancellation.as_ref())?;

    download_and_parse_resolved_github_archive(
        paths,
        asset,
        source,
        options,
        progress,
        network_client,
        staging_dir,
        cancellation,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn download_and_parse_github_archive_to_parsed_archive(
    paths: &FontbrewPaths,
    repo: &GitHubRepo,
    fallback_package_id: PackageId,
    source: PreparedInstallSource,
    options: RemoteInstallOptions,
    progress: &mut dyn ProgressSink,
    network_client: &NetworkClient,
    staging_dir: PathBuf,
    cancellation: Arc<dyn CancellationToken>,
) -> Result<ParsedFontArchive> {
    ensure_existing_path_does_not_cross_symlink(&paths.managed_store_dir(), &staging_dir)?;
    ensure_not_cancelled(cancellation.as_ref())?;

    let asset = github::resolve_release_asset(
        network_client,
        repo,
        options.asset_selector.as_deref(),
        &fallback_package_id,
    )
    .await?;
    ensure_not_cancelled(cancellation.as_ref())?;

    download_and_parse_resolved_github_archive_to_parsed_archive(
        paths,
        asset,
        source,
        options,
        progress,
        network_client,
        staging_dir,
        cancellation,
    )
    .await
}

pub(crate) async fn prepare_resolved_github_release_archive(
    paths: &FontbrewPaths,
    asset: github::ResolvedGitHubAsset,
    source: PreparedInstallSource,
    options: RemoteInstallOptions,
    network_client: &NetworkClient,
    cancellation: Arc<dyn CancellationToken>,
) -> Result<PreparedInstallPackage> {
    let mut progress = NoProgress;
    ensure_not_cancelled(cancellation.as_ref())?;
    let staging_dir = create_active_staging_dir(paths)?;
    let mut staging_cleanup = StagingCleanupGuard::new(staging_dir);
    ensure_not_cancelled(cancellation.as_ref())?;
    let result = download_and_parse_resolved_github_archive(
        paths,
        asset,
        source,
        options,
        &mut progress,
        network_client,
        staging_cleanup.path().to_path_buf(),
        cancellation.clone(),
    )
    .await;

    if result.is_ok() {
        staging_cleanup.disarm();
    }

    result
}

#[allow(clippy::too_many_arguments)]
async fn download_and_parse_resolved_github_archive(
    paths: &FontbrewPaths,
    asset: github::ResolvedGitHubAsset,
    source: PreparedInstallSource,
    options: RemoteInstallOptions,
    progress: &mut dyn ProgressSink,
    network_client: &NetworkClient,
    staging_dir: PathBuf,
    cancellation: Arc<dyn CancellationToken>,
) -> Result<PreparedInstallPackage> {
    ensure_not_cancelled(cancellation.as_ref())?;
    fs::create_dir_all(&staging_dir)?;
    let archive_path = staging_dir.join("download.zip");
    let progress_package_id = options
        .progress_package_id
        .clone()
        .or_else(|| options.package_id.clone());
    if let Some(package_id) = &progress_package_id {
        progress.emit(ProgressEvent::DownloadStarted {
            package_id: package_id.clone(),
            bytes: None,
        });
    }
    github::download_release_asset_to_file(
        network_client,
        &asset.download_url,
        &archive_path,
        cancellation.clone(),
    )
    .await?;
    ensure_not_cancelled(cancellation.as_ref())?;

    ensure_not_cancelled(cancellation.as_ref())?;
    let (result, events) = extract_and_parse_archive_blocking(RemoteArchiveParseInput {
        paths: paths.clone(),
        archive_path,
        staging_dir,
        version: asset.version,
        source,
        package_id_hint: options.package_id,
        progress_package_id,
        reinstall: options.reinstall,
        archive_format_preference: ArchiveFormatPreference {
            explicit_format_preference: options.explicit_format_preference,
        },
        family_boundary: options.family_boundary,
        cancellation: cancellation.clone(),
    })
    .await?;
    replay_progress(progress, events);
    match result {
        Ok(prepared) => {
            ensure_not_cancelled_after_prepare(cancellation.as_ref(), &prepared)?;
            Ok(prepared)
        }
        Err(error) => Err(error),
    }
}

#[allow(clippy::too_many_arguments)]
async fn download_and_parse_resolved_github_archive_to_parsed_archive(
    paths: &FontbrewPaths,
    asset: github::ResolvedGitHubAsset,
    source: PreparedInstallSource,
    options: RemoteInstallOptions,
    progress: &mut dyn ProgressSink,
    network_client: &NetworkClient,
    staging_dir: PathBuf,
    cancellation: Arc<dyn CancellationToken>,
) -> Result<ParsedFontArchive> {
    ensure_not_cancelled(cancellation.as_ref())?;
    fs::create_dir_all(&staging_dir)?;
    let archive_path = staging_dir.join("download.zip");
    let progress_package_id = options
        .progress_package_id
        .clone()
        .or_else(|| options.package_id.clone());
    if let Some(package_id) = &progress_package_id {
        progress.emit(ProgressEvent::DownloadStarted {
            package_id: package_id.clone(),
            bytes: None,
        });
    }
    github::download_release_asset_to_file(
        network_client,
        &asset.download_url,
        &archive_path,
        cancellation.clone(),
    )
    .await?;
    ensure_not_cancelled(cancellation.as_ref())?;

    let (result, events) = extract_archive_to_parsed_archive_blocking(RemoteParsedArchiveInput {
        paths: paths.clone(),
        archive_path,
        staging_dir,
        version: asset.version,
        source,
        progress_package_id,
        reinstall: options.reinstall,
        archive_format_preference: ArchiveFormatPreference {
            explicit_format_preference: options.explicit_format_preference,
        },
        cancellation: cancellation.clone(),
    })
    .await?;
    replay_progress(progress, events);
    result
}

struct RemoteArchiveParseInput {
    paths: FontbrewPaths,
    archive_path: PathBuf,
    staging_dir: PathBuf,
    version: PackageVersion,
    source: PreparedInstallSource,
    package_id_hint: Option<PackageId>,
    progress_package_id: Option<PackageId>,
    reinstall: bool,
    archive_format_preference: ArchiveFormatPreference,
    family_boundary: Option<InstallFamilyBoundary>,
    cancellation: Arc<dyn CancellationToken>,
}

struct RemoteParsedArchiveInput {
    paths: FontbrewPaths,
    archive_path: PathBuf,
    staging_dir: PathBuf,
    version: PackageVersion,
    source: PreparedInstallSource,
    progress_package_id: Option<PackageId>,
    reinstall: bool,
    archive_format_preference: ArchiveFormatPreference,
    cancellation: Arc<dyn CancellationToken>,
}

struct RemoteFontParseInput {
    paths: FontbrewPaths,
    staged_fonts: Vec<StagedFontFile>,
    staging_dir: PathBuf,
    version: PackageVersion,
    source: PreparedInstallSource,
    package_families: Option<Vec<FamilyName>>,
    package_id_hint: Option<PackageId>,
    reinstall: bool,
    archive_format_preference: ArchiveFormatPreference,
    family_boundary: Option<InstallFamilyBoundary>,
    cancellation: Arc<dyn CancellationToken>,
}

#[derive(Default)]
struct RecordingProgressSink {
    events: Vec<ProgressEvent>,
}

struct StagedFontFile {
    path: PathBuf,
    format: FontFileFormat,
    weight_override: Option<u16>,
}

impl ProgressSink for RecordingProgressSink {
    fn emit(&mut self, event: ProgressEvent) {
        self.events.push(event);
    }
}

fn replay_progress(progress: &mut dyn ProgressSink, events: Vec<ProgressEvent>) {
    for event in events {
        progress.emit(event);
    }
}

async fn extract_and_parse_archive_blocking(
    input: RemoteArchiveParseInput,
) -> Result<(Result<PreparedInstallPackage>, Vec<ProgressEvent>)> {
    tokio::task::spawn_blocking(move || {
        let mut progress = RecordingProgressSink::default();
        let result = extract_and_parse_archive(
            &input.paths,
            input.archive_path,
            input.staging_dir,
            input.version,
            input.source,
            input.package_id_hint,
            input.progress_package_id,
            input.reinstall,
            input.archive_format_preference,
            input.family_boundary,
            &mut progress,
            input.cancellation.as_ref(),
        );
        Ok((result, progress.events))
    })
    .await
    .map_err(blocking_join_error)?
}

async fn extract_archive_to_parsed_archive_blocking(
    input: RemoteParsedArchiveInput,
) -> Result<(Result<ParsedFontArchive>, Vec<ProgressEvent>)> {
    tokio::task::spawn_blocking(move || {
        let mut progress = RecordingProgressSink::default();
        let result = extract_archive_to_parsed_archive(
            &input.paths,
            input.archive_path,
            input.staging_dir,
            input.version,
            input.source,
            input.progress_package_id,
            input.reinstall,
            input.archive_format_preference,
            &mut progress,
            input.cancellation.as_ref(),
        );
        Ok((result, progress.events))
    })
    .await
    .map_err(blocking_join_error)?
}

async fn parse_staged_provider_fonts_blocking(
    input: RemoteFontParseInput,
) -> Result<(Result<PreparedInstallPackage>, Vec<ProgressEvent>)> {
    tokio::task::spawn_blocking(move || {
        let mut progress = RecordingProgressSink::default();
        if let Some(package_id) = &input.package_id_hint {
            progress.emit(ProgressEvent::ParsingFonts {
                package_id: package_id.clone(),
            });
        }
        let result = parse_staged_font_files(
            &input.paths,
            input.staged_fonts,
            input.staging_dir,
            input.version,
            input.source,
            input.package_id_hint,
            input.reinstall,
            input.archive_format_preference,
            input.family_boundary,
            input.package_families,
            input.cancellation.as_ref(),
        );
        Ok((result, progress.events))
    })
    .await
    .map_err(blocking_join_error)?
}

fn blocking_join_error(error: tokio::task::JoinError) -> FontbrewError {
    FontbrewError::Io(std::io::Error::other(error.to_string()))
}

#[allow(clippy::too_many_arguments)]
fn extract_and_parse_archive(
    paths: &FontbrewPaths,
    archive_path: PathBuf,
    staging_dir: PathBuf,
    version: PackageVersion,
    source: PreparedInstallSource,
    package_id_hint: Option<PackageId>,
    progress_package_id: Option<PackageId>,
    reinstall: bool,
    archive_format_preference: ArchiveFormatPreference,
    family_boundary: Option<InstallFamilyBoundary>,
    progress: &mut dyn ProgressSink,
    cancellation: &dyn CancellationToken,
) -> Result<PreparedInstallPackage> {
    ensure_existing_path_does_not_cross_symlink(&paths.managed_store_dir(), &staging_dir)?;
    ensure_not_cancelled(cancellation)?;

    if let Some(package_id) = &progress_package_id {
        progress.emit(ProgressEvent::ExtractingArchive {
            package_id: package_id.clone(),
        });
    }
    let extracted_fonts = ZipArchiveExtractor::new(ArchiveExtractionOptions::default())
        .extract(&archive_path, &staging_dir)?;
    ensure_not_cancelled(cancellation)?;

    if let Some(package_id) = &progress_package_id {
        progress.emit(ProgressEvent::ParsingFonts {
            package_id: package_id.clone(),
        });
    }
    let staged_fonts = extracted_fonts
        .into_iter()
        .map(StagedFontFile::from_extracted)
        .collect();
    parse_staged_font_files(
        paths,
        staged_fonts,
        staging_dir,
        version,
        source,
        package_id_hint,
        reinstall,
        archive_format_preference,
        family_boundary,
        None,
        cancellation,
    )
}

#[allow(clippy::too_many_arguments)]
fn extract_archive_to_parsed_archive(
    paths: &FontbrewPaths,
    archive_path: PathBuf,
    staging_dir: PathBuf,
    version: PackageVersion,
    source: PreparedInstallSource,
    progress_package_id: Option<PackageId>,
    reinstall: bool,
    archive_format_preference: ArchiveFormatPreference,
    progress: &mut dyn ProgressSink,
    cancellation: &dyn CancellationToken,
) -> Result<ParsedFontArchive> {
    ensure_existing_path_does_not_cross_symlink(&paths.managed_store_dir(), &staging_dir)?;
    ensure_not_cancelled(cancellation)?;

    if let Some(package_id) = &progress_package_id {
        progress.emit(ProgressEvent::ExtractingArchive {
            package_id: package_id.clone(),
        });
    }
    let extracted_fonts = ZipArchiveExtractor::new(ArchiveExtractionOptions::default())
        .extract(&archive_path, &staging_dir)?;
    ensure_not_cancelled(cancellation)?;

    if let Some(package_id) = &progress_package_id {
        progress.emit(ProgressEvent::ParsingFonts {
            package_id: package_id.clone(),
        });
    }
    let staged_fonts = extracted_fonts
        .into_iter()
        .map(StagedFontFile::from_extracted)
        .collect();
    parse_staged_font_archive(
        staged_fonts,
        staging_dir,
        version,
        source,
        reinstall,
        archive_format_preference,
        cancellation,
    )
}

#[allow(clippy::too_many_arguments)]
fn parse_staged_font_files(
    paths: &FontbrewPaths,
    staged_fonts: Vec<StagedFontFile>,
    staging_dir: PathBuf,
    version: PackageVersion,
    source: PreparedInstallSource,
    package_id_hint: Option<PackageId>,
    reinstall: bool,
    archive_format_preference: ArchiveFormatPreference,
    family_boundary: Option<InstallFamilyBoundary>,
    package_families: Option<Vec<FamilyName>>,
    cancellation: &dyn CancellationToken,
) -> Result<PreparedInstallPackage> {
    let parsed_archive = parse_staged_font_archive(
        staged_fonts,
        staging_dir,
        version,
        source,
        reinstall,
        archive_format_preference,
        cancellation,
    )?;

    prepare_package_from_parsed_archive(
        paths,
        parsed_archive,
        package_id_hint,
        family_boundary.as_ref(),
        package_families,
        cancellation,
    )
}

fn parse_staged_font_archive(
    staged_fonts: Vec<StagedFontFile>,
    staging_dir: PathBuf,
    version: PackageVersion,
    source: PreparedInstallSource,
    reinstall: bool,
    archive_format_preference: ArchiveFormatPreference,
    cancellation: &dyn CancellationToken,
) -> Result<ParsedFontArchive> {
    if staged_fonts.is_empty() {
        cleanup_staging(&staging_dir);
        return Err(FontbrewError::ArchiveRejected {
            reason: "source contains no desktop font files".to_string(),
        });
    }

    let mut family_names = BTreeSet::new();
    let reader = TtfParserMetadataReader;
    let mut parsed_files = Vec::with_capacity(staged_fonts.len());

    for staged_font in staged_fonts {
        ensure_not_cancelled(cancellation)?;
        let faces = match reader.read_file(&staged_font.path) {
            Ok(faces) => faces,
            Err(error) => {
                cleanup_staging(&staging_dir);
                return Err(error);
            }
        };
        let faces = faces_with_weight_override(faces, staged_font.weight_override);

        if faces.is_empty() {
            cleanup_staging(&staging_dir);
            return Err(FontbrewError::FontParse {
                message: format!(
                    "font file has no readable faces: {}",
                    staged_font.path.display()
                ),
            });
        }

        for face in &faces {
            family_names.insert(face.family_name.as_str().to_string());
        }

        parsed_files.push(ParsedFontFile {
            staging_path: staged_font.path,
            faces,
            format: font_format_from_reader_format(staged_font.format),
        });
    }

    let archive_families = family_names
        .iter()
        .map(|family| FamilyName::new(family.clone()))
        .collect::<Vec<_>>();
    if archive_families.is_empty() {
        cleanup_staging(&staging_dir);
        return Err(FontbrewError::FontParse {
            message: "archive contained no readable font families".to_string(),
        });
    };

    Ok(ParsedFontArchive {
        staging_dir,
        version,
        source,
        reinstall,
        archive_format_preference,
        archive_families,
        parsed_files,
    })
}

fn prepare_package_from_parsed_archive(
    paths: &FontbrewPaths,
    parsed_archive: ParsedFontArchive,
    package_id_hint: Option<PackageId>,
    family_boundary: Option<&InstallFamilyBoundary>,
    package_families: Option<Vec<FamilyName>>,
    cancellation: &dyn CancellationToken,
) -> Result<PreparedInstallPackage> {
    let ParsedFontArchive {
        staging_dir,
        version,
        source,
        reinstall,
        archive_format_preference,
        archive_families,
        mut parsed_files,
    } = parsed_archive;

    if let Some(boundary) = family_boundary {
        if let Err(error) = validate_archive_family_boundary(boundary, &archive_families) {
            cleanup_staging(&staging_dir);
            return Err(error);
        }
        parsed_files = match filter_parsed_files_by_family_boundary(parsed_files, boundary) {
            Ok(parsed_files) => parsed_files,
            Err(error) => {
                cleanup_staging(&staging_dir);
                return Err(error);
            }
        };
    } else if should_reject_unbounded_multiple_families(&source) && archive_families.len() > 1 {
        cleanup_staging(&staging_dir);
        return Err(FontbrewError::FamilySelectionRequired {
            families: archive_families,
        });
    }

    let boundary_families = selected_family_names(&parsed_files);
    let Some(package_family) = boundary_families.first() else {
        cleanup_staging(&staging_dir);
        return Err(FontbrewError::ArchiveRejected {
            reason: "selected family boundary matched no font files".to_string(),
        });
    };

    let package_id = match package_id_hint {
        Some(package_id) => package_id,
        None => match PackageId::normalize(package_family.as_str()) {
            Ok(package_id) => package_id,
            Err(error) => {
                cleanup_staging(&staging_dir);
                return Err(error);
            }
        },
    };
    ensure_not_cancelled(cancellation)?;
    let loaded_config = match FontbrewConfig::load_with_sources(&paths.config_path()) {
        Ok(config) => config,
        Err(error) => {
            cleanup_staging(&staging_dir);
            return Err(error);
        }
    };
    let format_selection = format_selection(
        &archive_format_preference,
        &loaded_config.config.format_preference,
        loaded_config.has_format_preference,
    );
    let parsed_files =
        match select_preferred_format_files(&package_id, parsed_files, &format_selection) {
            Ok(parsed_files) => parsed_files,
            Err(error) => {
                cleanup_staging(&staging_dir);
                return Err(error);
            }
        };
    if let Some(boundary) = family_boundary {
        let selected_families = selected_family_names(&parsed_files);
        if let Err(error) =
            validate_expected_family_boundary(boundary, &selected_families, "selected font files")
        {
            cleanup_staging(&staging_dir);
            return Err(error);
        }
    }

    let package_store_dir = paths.package_store_dir(&package_id, &version);
    let files_dir = package_store_dir.join("files");
    let families = package_families.unwrap_or_else(|| selected_family_names(&parsed_files));
    let mut font_files = Vec::with_capacity(parsed_files.len());
    let mut activation_sources = Vec::with_capacity(parsed_files.len());

    for parsed_file in parsed_files {
        ensure_not_cancelled(cancellation)?;
        let relative_path = parsed_file
            .staging_path
            .strip_prefix(&staging_dir)
            .map_err(|_| FontbrewError::PathResolution {
                message: format!(
                    "staged font path is outside staging directory: {}",
                    parsed_file.staging_path.display()
                ),
            })?;
        let stored_path = files_dir.join(relative_path);
        let prepared_faces = parsed_file
            .faces
            .iter()
            .map(prepared_face_from_metadata)
            .collect();

        activation_sources.push(stored_path.clone());
        font_files.push(PreparedFontFile {
            staging_path: parsed_file.staging_path,
            stored_path,
            faces: prepared_faces,
        });
    }

    let activation_plan = ActivationPlanner::plan(ActivationRequest {
        package_id: package_id.clone(),
        font_files: activation_sources,
        activation_dir: paths.activation_dir(),
        strategy: loaded_config.config.activation_strategy,
    })?;

    Ok(PreparedInstallPackage {
        package_id,
        version,
        source,
        families,
        font_files,
        activation_dir: activation_plan.activation_dir,
        activation_strategy: activation_plan.strategy,
        activation_artifacts: activation_plan.artifacts,
        activation_risks: activation_plan.risks,
        staging_dir,
        files_dir,
        package_store_dir,
        reinstall,
    })
}

pub(crate) fn family_install_plans_from_parsed_archive(
    paths: &FontbrewPaths,
    parsed_archive: ParsedFontArchive,
    selected_families: Vec<FamilyName>,
    progress: &mut dyn ProgressSink,
    cancellation: &dyn CancellationToken,
) -> Result<Vec<InstallPlan>> {
    let selected_families = dedupe_family_names(selected_families);
    let Some(all_selected_boundary) =
        InstallFamilyBoundary::from_selected_families(selected_families.clone())
    else {
        cleanup_staging(&parsed_archive.staging_dir);
        return Err(FontbrewError::ArchiveRejected {
            reason: "selected family boundary matched no font files".to_string(),
        });
    };
    if let Err(error) =
        validate_archive_family_boundary(&all_selected_boundary, &parsed_archive.archive_families)
    {
        cleanup_staging(&parsed_archive.staging_dir);
        return Err(error);
    }

    let mut prepared_packages = Vec::new();
    for family in selected_families {
        let prepared_result = prepare_family_package_from_parsed_archive(
            paths,
            &parsed_archive,
            family,
            cancellation,
        );
        match prepared_result {
            Ok(prepared) => prepared_packages.push(prepared),
            Err(error) => {
                cleanup_staging(&parsed_archive.staging_dir);
                cleanup_prepared_packages(&prepared_packages);
                return Err(error);
            }
        }
    }
    cleanup_staging(&parsed_archive.staging_dir);

    install_plans_from_prepared_packages(paths, prepared_packages, progress)
}

#[allow(clippy::too_many_arguments)]
fn install_plan_candidate_from_parsed_archive(
    paths: &FontbrewPaths,
    parsed_archive: ParsedFontArchive,
    package_id_hint: Option<PackageId>,
    package_id_override: Option<PackageId>,
    family_boundary: Option<&InstallFamilyBoundary>,
    package_families: Option<Vec<FamilyName>>,
    progress: &mut dyn ProgressSink,
    cancellation: &dyn CancellationToken,
) -> Result<InstallPlanCandidate> {
    if family_boundary.is_none() && parsed_archive_requires_family_selection(&parsed_archive) {
        return Ok(InstallPlanCandidate::FamilySelection {
            parsed_archive,
            package_id_override,
        });
    }

    let prepared = prepare_package_from_parsed_archive(
        paths,
        parsed_archive,
        package_id_hint,
        family_boundary,
        package_families,
        cancellation,
    )?;
    ensure_not_cancelled_after_prepare(cancellation, &prepared)?;
    install_plan_from_prepared(paths, prepared, progress).map(InstallPlanCandidate::Plan)
}

fn parsed_archive_requires_family_selection(parsed_archive: &ParsedFontArchive) -> bool {
    should_reject_unbounded_multiple_families(&parsed_archive.source)
        && parsed_archive.archive_families.len() > 1
}

fn prepare_family_package_from_parsed_archive(
    paths: &FontbrewPaths,
    parsed_archive: &ParsedFontArchive,
    family: FamilyName,
    cancellation: &dyn CancellationToken,
) -> Result<PreparedInstallPackage> {
    ensure_not_cancelled(cancellation)?;
    let staging_dir = create_active_staging_dir(paths)?;
    let mut staging_cleanup = StagingCleanupGuard::new(staging_dir);
    let copied_archive = copy_parsed_archive_to_staging(
        paths,
        parsed_archive,
        staging_cleanup.path(),
        cancellation,
    )?;
    let boundary =
        InstallFamilyBoundary::from_selected_families(vec![family]).ok_or_else(|| {
            FontbrewError::ArchiveRejected {
                reason: "selected family boundary matched no font files".to_string(),
            }
        })?;
    let prepared = prepare_package_from_parsed_archive(
        paths,
        copied_archive,
        None,
        Some(&boundary),
        None,
        cancellation,
    );

    if prepared.is_ok() {
        staging_cleanup.disarm();
    }

    prepared
}

fn copy_parsed_archive_to_staging(
    paths: &FontbrewPaths,
    parsed_archive: &ParsedFontArchive,
    staging_dir: &Path,
    cancellation: &dyn CancellationToken,
) -> Result<ParsedFontArchive> {
    ensure_existing_path_does_not_cross_symlink(&paths.managed_store_dir(), staging_dir)?;
    fs::create_dir_all(staging_dir)?;

    let mut parsed_files = Vec::with_capacity(parsed_archive.parsed_files.len());
    for parsed_file in &parsed_archive.parsed_files {
        ensure_not_cancelled(cancellation)?;
        let relative_path = parsed_file
            .staging_path
            .strip_prefix(&parsed_archive.staging_dir)
            .map_err(|_| FontbrewError::PathResolution {
                message: format!(
                    "staged font path is outside staging directory: {}",
                    parsed_file.staging_path.display()
                ),
            })?;
        let copied_path = staging_dir.join(relative_path);
        ensure_path_inside(staging_dir, &copied_path)?;
        if let Some(parent) = copied_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(&parsed_file.staging_path, &copied_path)?;
        parsed_files.push(ParsedFontFile {
            staging_path: copied_path,
            faces: parsed_file.faces.clone(),
            format: parsed_file.format,
        });
    }

    Ok(ParsedFontArchive {
        staging_dir: staging_dir.to_path_buf(),
        version: parsed_archive.version.clone(),
        source: parsed_archive.source.clone(),
        reinstall: parsed_archive.reinstall,
        archive_format_preference: parsed_archive.archive_format_preference.clone(),
        archive_families: parsed_archive.archive_families.clone(),
        parsed_files,
    })
}

fn install_plans_from_prepared_packages(
    paths: &FontbrewPaths,
    prepared_packages: Vec<PreparedInstallPackage>,
    progress: &mut dyn ProgressSink,
) -> Result<Vec<InstallPlan>> {
    let manifest = match ManifestStore::read_or_empty(&paths.manifest_path()) {
        Ok(manifest) => manifest,
        Err(error) => {
            cleanup_prepared_packages(&prepared_packages);
            return Err(error);
        }
    };
    let unmanaged_overlap_risks = match unmanaged_same_family_overlap_risks_for_prepared_packages(
        paths,
        &manifest,
        &prepared_packages,
    ) {
        Ok(risks) => risks,
        Err(error) => {
            cleanup_prepared_packages(&prepared_packages);
            return Err(error);
        }
    };

    let mut plans = Vec::new();
    for prepared in prepared_packages {
        progress.emit(ProgressEvent::CheckingInstallRisks {
            package_id: prepared_package_id(&prepared),
        });
        let package_overlap_risks =
            unmanaged_overlap_risks_for_families(&unmanaged_overlap_risks, &prepared.families);
        match install_plan_from_prepared_with_manifest(&manifest, prepared, package_overlap_risks) {
            Ok(plan) => plans.push(plan),
            Err(error) => {
                cleanup_install_plans(&plans);
                return Err(error);
            }
        }
    }

    Ok(plans)
}

fn cleanup_prepared_packages(prepared_packages: &[PreparedInstallPackage]) {
    for prepared in prepared_packages {
        cleanup_staging(&prepared.staging_dir);
    }
}

fn cleanup_install_plans(plans: &[InstallPlan]) {
    for plan in plans {
        cleanup_install_plan_staging(plan);
    }
}

#[derive(Debug, Clone)]
struct ParsedFontFile {
    staging_path: PathBuf,
    faces: Vec<FontFaceMetadata>,
    format: FontFormat,
}

#[derive(Debug, Clone)]
pub(crate) struct ParsedFontArchive {
    pub(crate) staging_dir: PathBuf,
    version: PackageVersion,
    pub(crate) source: PreparedInstallSource,
    reinstall: bool,
    archive_format_preference: ArchiveFormatPreference,
    pub(crate) archive_families: Vec<FamilyName>,
    parsed_files: Vec<ParsedFontFile>,
}

fn validate_archive_family_boundary(
    boundary: &InstallFamilyBoundary,
    archive_families: &[FamilyName],
) -> Result<()> {
    validate_expected_family_boundary(boundary, archive_families, "archive")?;

    if boundary.allows_extra_archive_families() {
        return Ok(());
    }

    let unexpected = archive_families
        .iter()
        .filter(|family| !boundary.includes_family(family) && !boundary.excludes_family(family))
        .cloned()
        .collect::<Vec<_>>();
    if unexpected.is_empty() {
        return Ok(());
    }

    Err(FontbrewError::ArchiveRejected {
        reason: format!(
            "archive contains unexpected font families: {}",
            family_list_label(&unexpected)
        ),
    })
}

fn validate_expected_family_boundary(
    boundary: &InstallFamilyBoundary,
    families: &[FamilyName],
    source_label: &str,
) -> Result<()> {
    let missing = boundary
        .expected_families()
        .iter()
        .filter(|expected| !family_matches_any(families, expected))
        .cloned()
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(());
    }

    Err(FontbrewError::ArchiveRejected {
        reason: format!(
            "{source_label} is missing {} font families: {}",
            boundary.family_label(),
            family_list_label(&missing)
        ),
    })
}

fn filter_parsed_files_by_family_boundary(
    parsed_files: Vec<ParsedFontFile>,
    boundary: &InstallFamilyBoundary,
) -> Result<Vec<ParsedFontFile>> {
    let mut filtered_files = Vec::new();

    for parsed_file in parsed_files {
        let included_face_count = parsed_file
            .faces
            .iter()
            .filter(|face| boundary.includes_family(&face.family_name))
            .count();

        if included_face_count == 0 {
            continue;
        }

        if included_face_count != parsed_file.faces.len() {
            return Err(FontbrewError::ArchiveRejected {
                reason: format!(
                    "font file contains both included and excluded {} font families; cannot install a family subset from one font binary: {} (included: {}; excluded: {})",
                    boundary.family_label(),
                    parsed_file.staging_path.display(),
                    face_family_list(&parsed_file.faces, boundary, true),
                    face_family_list(&parsed_file.faces, boundary, false)
                ),
            });
        }

        filtered_files.push(parsed_file);
    }

    Ok(filtered_files)
}

fn should_reject_unbounded_multiple_families(source: &PreparedInstallSource) -> bool {
    matches!(
        source,
        PreparedInstallSource::LocalArchive { .. } | PreparedInstallSource::GitHub { .. }
    )
}

fn family_matches_any(families: &[FamilyName], family: &FamilyName) -> bool {
    let normalized = normalize_family_boundary_name(family.as_str());

    families
        .iter()
        .any(|candidate| normalize_family_boundary_name(candidate.as_str()) == normalized)
}

fn dedupe_family_names(families: Vec<FamilyName>) -> Vec<FamilyName> {
    let mut seen = BTreeSet::new();
    let mut deduped = Vec::new();

    for family in families {
        let normalized = normalize_family_boundary_name(family.as_str());
        if seen.insert(normalized) {
            deduped.push(family);
        }
    }

    deduped
}

pub(crate) fn normalize_family_boundary_name(name: &str) -> String {
    name.chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn family_list_label(families: &[FamilyName]) -> String {
    families
        .iter()
        .map(|family| family.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn face_family_list(
    faces: &[FontFaceMetadata],
    boundary: &InstallFamilyBoundary,
    included: bool,
) -> String {
    let mut families = BTreeSet::new();

    for face in faces {
        if boundary.includes_family(&face.family_name) == included {
            families.insert(face.family_name.as_str().to_string());
        }
    }

    families.into_iter().collect::<Vec<_>>().join(", ")
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct FaceCoverage {
    family: String,
    style: String,
    weight: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FormatSelection {
    preference: Vec<FontFormat>,
    explicit: bool,
}

fn format_selection(
    archive_format_preference: &ArchiveFormatPreference,
    config_format_preference: &[FontFormat],
    has_config_format_preference: bool,
) -> FormatSelection {
    if !archive_format_preference
        .explicit_format_preference
        .is_empty()
    {
        return FormatSelection {
            preference: dedupe_formats(
                archive_format_preference
                    .explicit_format_preference
                    .iter()
                    .copied(),
            ),
            explicit: true,
        };
    }

    if has_config_format_preference {
        return FormatSelection {
            preference: preference_with_builtin_fallback(config_format_preference),
            explicit: false,
        };
    }

    FormatSelection {
        preference: desktop_format_fallback_order(),
        explicit: false,
    }
}

impl StagedFontFile {
    fn from_extracted(font_file: ExtractedFontFile) -> Self {
        Self {
            path: font_file.path,
            format: font_file.format,
            weight_override: None,
        }
    }
}

fn faces_with_weight_override(
    faces: Vec<FontFaceMetadata>,
    weight_override: Option<u16>,
) -> Vec<FontFaceMetadata> {
    let Some(weight) = weight_override else {
        return faces;
    };

    faces
        .into_iter()
        .map(|mut face| {
            face.weight = Some(weight);
            face
        })
        .collect()
}

fn preference_with_builtin_fallback(format_preference: &[FontFormat]) -> Vec<FontFormat> {
    let mut preference = dedupe_formats(format_preference.iter().copied());

    for fallback in desktop_format_fallback_order() {
        if !preference.contains(&fallback) {
            preference.push(fallback);
        }
    }

    preference
}

fn desktop_format_fallback_order() -> Vec<FontFormat> {
    vec![
        FontFormat::Otf,
        FontFormat::Ttf,
        FontFormat::Ttc,
        FontFormat::Otc,
    ]
}

fn select_preferred_format_files(
    package_id: &PackageId,
    parsed_files: Vec<ParsedFontFile>,
    format_selection: &FormatSelection,
) -> Result<Vec<ParsedFontFile>> {
    let coverage_by_format = font_coverage_by_format(&parsed_files);
    if format_selection.explicit {
        let selected_format =
            requested_available_format(package_id, format_selection, &coverage_by_format)?;

        return Ok(parsed_files
            .into_iter()
            .filter(|file| file.format == selected_format)
            .collect());
    }

    if coverage_by_format.len() <= 1 {
        return Ok(parsed_files);
    }

    let Some(selected_format) = format_selection
        .preference
        .iter()
        .find(|format| coverage_by_format.contains_key(format))
        .copied()
        .or_else(|| coverage_by_format.keys().next().copied())
    else {
        return Ok(parsed_files);
    };

    Ok(parsed_files
        .into_iter()
        .filter(|file| file.format == selected_format)
        .collect())
}

fn requested_available_format(
    package_id: &PackageId,
    format_selection: &FormatSelection,
    coverage_by_format: &BTreeMap<FontFormat, BTreeSet<FaceCoverage>>,
) -> Result<FontFormat> {
    format_selection
        .preference
        .iter()
        .find(|format| coverage_by_format.contains_key(format))
        .copied()
        .ok_or_else(|| FontbrewError::Conflict {
            package_id: package_id.clone(),
            message: format!(
                "requested font formats are not available for {}; requested: {}; available: {}",
                package_id.as_str(),
                format_list_label(&format_selection.preference),
                format_list_label(coverage_by_format.keys())
            ),
        })
}

fn font_coverage_by_format(
    parsed_files: &[ParsedFontFile],
) -> BTreeMap<FontFormat, BTreeSet<FaceCoverage>> {
    let mut coverage_by_format = BTreeMap::new();

    for parsed_file in parsed_files {
        let coverage = coverage_by_format
            .entry(parsed_file.format)
            .or_insert_with(BTreeSet::new);
        for face in &parsed_file.faces {
            coverage.insert(face_coverage(face));
        }
    }

    coverage_by_format
}

fn face_coverage(face: &FontFaceMetadata) -> FaceCoverage {
    FaceCoverage {
        family: face.family_name.as_str().to_string(),
        style: face_style(face),
        weight: face.weight.unwrap_or(400),
    }
}

fn format_list_label<'a>(formats: impl IntoIterator<Item = &'a FontFormat>) -> String {
    formats
        .into_iter()
        .map(font_format_label)
        .collect::<Vec<_>>()
        .join(", ")
}

fn selected_family_names(parsed_files: &[ParsedFontFile]) -> Vec<FamilyName> {
    let mut families = BTreeSet::new();

    for parsed_file in parsed_files {
        for face in &parsed_file.faces {
            families.insert(face.family_name.as_str().to_string());
        }
    }

    families.into_iter().map(FamilyName::new).collect()
}

fn github_install_package_id(
    repo: &GitHubRepo,
    options: &RemoteInstallOptions,
) -> Result<PackageId> {
    match &options.package_id {
        Some(package_id) => Ok(package_id.clone()),
        None => package_id_from_repo_name(&repo.repo),
    }
}

fn package_id_from_repo_name(repo: &str) -> Result<PackageId> {
    let mut slug = String::new();
    let mut previous_was_separator = false;

    for character in repo.chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
            previous_was_separator = false;
            continue;
        }

        if matches!(character, '-' | '_' | '.') {
            if !slug.is_empty() && !previous_was_separator {
                slug.push('-');
                previous_was_separator = true;
            }
            continue;
        }

        return Err(FontbrewError::InvalidPackageId {
            input: repo.to_string(),
            reason: "GitHub repo name contains an unsafe character".to_string(),
        });
    }

    while slug.ends_with('-') {
        slug.pop();
    }

    PackageId::parse(slug)
}

fn apply_prepared_install(
    paths: &FontbrewPaths,
    manifest: &mut crate::manifest::ManifestV1,
    prepared: &PreparedInstallPackage,
    policy: ExecutionPolicy,
    progress: &mut dyn ProgressSink,
    cancellation: &dyn CancellationToken,
) -> Result<InstallReport> {
    ensure_not_cancelled(cancellation)?;
    reject_unmanaged_package_store(paths, prepared)?;
    ensure_not_cancelled(cancellation)?;

    let previous_activation_artifacts = if prepared.reinstall {
        manifest
            .get_package(&prepared_package_id(prepared))
            .map(activation_artifacts_from_record)
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    deactivate(&paths.activation_dir(), &previous_activation_artifacts)?;

    if let Err(error) = ensure_not_cancelled(cancellation) {
        let _ = restore_activation_artifacts(paths, &previous_activation_artifacts);
        return Err(error);
    }

    let backup_dir = match backup_existing_package_store_for_reinstall(paths, prepared) {
        Ok(backup_dir) => backup_dir,
        Err(error) => {
            let _ = restore_activation_artifacts(paths, &previous_activation_artifacts);
            return Err(error);
        }
    };

    if let Err(error) = ensure_not_cancelled(cancellation) {
        rollback_install(
            paths,
            &[],
            &prepared.package_store_dir,
            backup_dir.as_deref(),
            &previous_activation_artifacts,
        );
        return Err(error);
    }
    if let Err(error) = copy_prepared_files(paths, prepared) {
        rollback_install(
            paths,
            &[],
            &prepared.package_store_dir,
            backup_dir.as_deref(),
            &previous_activation_artifacts,
        );
        return Err(error);
    }

    let activation_plan = ActivationPlan {
        package_id: prepared_package_id(prepared),
        activation_dir: prepared.activation_dir.clone(),
        strategy: prepared.activation_strategy,
        artifacts: prepared.activation_artifacts.clone(),
        risks: Vec::new(),
    };
    let preexisting_activation_paths =
        match preexisting_activation_artifact_paths(&activation_plan.artifacts) {
            Ok(paths) => paths,
            Err(error) => {
                rollback_install(
                    paths,
                    &[],
                    &prepared.package_store_dir,
                    backup_dir.as_deref(),
                    &previous_activation_artifacts,
                );
                return Err(error);
            }
        };
    if let Err(error) = ensure_not_cancelled(cancellation) {
        rollback_install(
            paths,
            &[],
            &prepared.package_store_dir,
            backup_dir.as_deref(),
            &previous_activation_artifacts,
        );
        return Err(error);
    }
    let activation_artifacts = match activation_plan.apply(policy) {
        Ok(artifacts) => artifacts,
        Err(error) => {
            let rollback_artifacts = rollback_activation_artifacts(
                &activation_plan.artifacts,
                &preexisting_activation_paths,
            );
            rollback_install(
                paths,
                &rollback_artifacts,
                &prepared.package_store_dir,
                backup_dir.as_deref(),
                &previous_activation_artifacts,
            );
            return Err(error);
        }
    };
    let manifest_record = manifest_record_from_prepared(prepared, activation_artifacts.clone())?;

    if let Err(error) = ensure_not_cancelled(cancellation) {
        let rollback_artifacts =
            rollback_activation_artifacts(&activation_artifacts, &preexisting_activation_paths);
        rollback_install(
            paths,
            &rollback_artifacts,
            &prepared.package_store_dir,
            backup_dir.as_deref(),
            &previous_activation_artifacts,
        );
        return Err(error);
    }
    manifest.insert_package(manifest_record.clone())?;
    if let Err(error) = ManifestStore::write(&paths.manifest_path(), manifest) {
        let rollback_artifacts =
            rollback_activation_artifacts(&activation_artifacts, &preexisting_activation_paths);
        rollback_install(
            paths,
            &rollback_artifacts,
            &prepared.package_store_dir,
            backup_dir.as_deref(),
            &previous_activation_artifacts,
        );
        return Err(error);
    }

    if let Some(backup_dir) = backup_dir {
        let _ = fs::remove_dir_all(backup_dir);
    }

    progress.emit(ProgressEvent::FinishedPackage {
        package_id: manifest_record.package_id.clone(),
    });

    Ok(install_report_from_record(&manifest_record, true, false))
}

fn prepared_face_from_metadata(face: &FontFaceMetadata) -> PreparedFontFace {
    PreparedFontFace {
        family: face.family_name.clone(),
        style: face_style(face),
        weight: face.weight.unwrap_or(400),
        format: font_format_from_reader_format(face.format),
    }
}

fn face_style(face: &FontFaceMetadata) -> String {
    if let Some(subfamily_name) = &face.subfamily_name {
        return subfamily_name.clone();
    }

    if face.is_italic {
        "Italic".to_string()
    } else if face.is_oblique {
        "Oblique".to_string()
    } else {
        "Regular".to_string()
    }
}

fn font_format_from_reader_format(format: FontFileFormat) -> FontFormat {
    match format {
        FontFileFormat::Ttf => FontFormat::Ttf,
        FontFileFormat::Otf => FontFormat::Otf,
        FontFileFormat::Ttc => FontFormat::Ttc,
        FontFileFormat::Otc => FontFormat::Otc,
    }
}

fn reader_format_from_font_format(format: FontFormat) -> FontFileFormat {
    match format {
        FontFormat::Ttf => FontFileFormat::Ttf,
        FontFormat::Otf => FontFileFormat::Otf,
        FontFormat::Ttc => FontFileFormat::Ttc,
        FontFormat::Otc => FontFileFormat::Otc,
    }
}

fn manifest_font_format(format: &FontFormat) -> ManifestFontFileFormat {
    match format {
        FontFormat::Ttf => ManifestFontFileFormat::Ttf,
        FontFormat::Otf => ManifestFontFileFormat::Otf,
        FontFormat::Ttc => ManifestFontFileFormat::Ttc,
        FontFormat::Otc => ManifestFontFileFormat::Otc,
    }
}

fn font_format_from_manifest_format(format: ManifestFontFileFormat) -> FontFormat {
    match format {
        ManifestFontFileFormat::Ttf => FontFormat::Ttf,
        ManifestFontFileFormat::Otf => FontFormat::Otf,
        ManifestFontFileFormat::Ttc => FontFormat::Ttc,
        ManifestFontFileFormat::Otc => FontFormat::Otc,
    }
}

fn prepared_package_id(prepared: &PreparedInstallPackage) -> PackageId {
    prepared.package_id.clone()
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
    prepared: &PreparedInstallPackage,
) -> Result<()> {
    ensure_existing_path_does_not_cross_symlink(
        &paths.managed_store_dir(),
        &prepared.package_store_dir,
    )?;

    let manifest = ManifestStore::read_or_empty(&paths.manifest_path())?;
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

fn backup_existing_package_store_for_reinstall(
    paths: &FontbrewPaths,
    prepared: &PreparedInstallPackage,
) -> Result<Option<PathBuf>> {
    if prepared.reinstall && prepared.package_store_dir.exists() {
        return backup_existing_package_store(paths, prepared).map(Some);
    }

    Ok(None)
}

fn backup_existing_package_store(
    paths: &FontbrewPaths,
    prepared: &PreparedInstallPackage,
) -> Result<PathBuf> {
    ensure_existing_path_does_not_cross_symlink(
        &paths.managed_store_dir(),
        &prepared.package_store_dir,
    )?;

    let backup_dir = prepared
        .package_store_dir
        .parent()
        .ok_or_else(|| FontbrewError::PathResolution {
            message: format!(
                "package store path has no parent: {}",
                prepared.package_store_dir.display()
            ),
        })?
        .join(format!(
            ".{}-{}-backup-{}",
            prepared_package_id(prepared).as_str(),
            prepared.version.as_str(),
            operation_suffix()?
        ));
    ensure_existing_path_does_not_cross_symlink(&paths.managed_store_dir(), &backup_dir)?;
    fs::rename(&prepared.package_store_dir, &backup_dir)?;

    Ok(backup_dir)
}

fn rollback_install(
    paths: &FontbrewPaths,
    activation_artifacts: &[ActivationArtifact],
    package_store_dir: &Path,
    backup_dir: Option<&Path>,
    previous_activation_artifacts: &[ActivationArtifact],
) {
    let _ = deactivate(&paths.activation_dir(), activation_artifacts);
    rollback_package_store(package_store_dir, backup_dir);
    let _ = restore_activation_artifacts(paths, previous_activation_artifacts);
}

fn restore_activation_artifacts(
    paths: &FontbrewPaths,
    artifacts: &[ActivationArtifact],
) -> Result<()> {
    for artifact in artifacts {
        let plan = ActivationPlan {
            package_id: artifact.package_id.clone(),
            activation_dir: paths.activation_dir(),
            strategy: artifact.strategy,
            artifacts: vec![artifact.clone()],
            risks: Vec::new(),
        };
        plan.apply(ExecutionPolicy::AssumeYes)?;
    }

    Ok(())
}

fn preexisting_activation_artifact_paths(artifacts: &[ActivationArtifact]) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();

    for artifact in artifacts {
        match fs::read_link(&artifact.path) {
            Ok(target) if target == artifact.source_path => paths.push(artifact.path.clone()),
            Ok(_) => {}
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound
                    || error.kind() == std::io::ErrorKind::InvalidInput => {}
            Err(error) => return Err(error.into()),
        }
    }

    Ok(paths)
}

fn rollback_activation_artifacts(
    artifacts: &[ActivationArtifact],
    preexisting_paths: &[PathBuf],
) -> Vec<ActivationArtifact> {
    artifacts
        .iter()
        .filter(|artifact| !preexisting_paths.iter().any(|path| path == &artifact.path))
        .cloned()
        .collect()
}

fn rollback_package_store(package_store_dir: &Path, backup_dir: Option<&Path>) {
    let _ = fs::remove_dir_all(package_store_dir);

    if let Some(backup_dir) = backup_dir {
        let _ = fs::rename(backup_dir, package_store_dir);
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

fn manifest_source_from_prepared(source: &PreparedInstallSource) -> ManifestSource {
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

fn manifest_update_source_from_prepared(source: &PreparedInstallSource) -> Option<ManifestSource> {
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

fn managed_font_files_from_record(record: &ManifestPackageRecord) -> Vec<ManagedFontFile> {
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

fn managed_activation_artifacts_from_record(
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

fn install_report_from_record(
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

fn dry_run_install_report(plan: InstallPlan) -> Result<InstallReport> {
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

fn cleanup_install_plan_staging(plan: &InstallPlan) {
    if let Some(prepared) = &plan.prepared {
        cleanup_staging(&prepared.staging_dir);
    }
}

fn first_blocking_conflict_description(risks: &[PlanRisk]) -> Option<String> {
    risks.iter().find_map(|risk| match risk {
        PlanRisk::Conflict { description, .. } => Some(description.clone()),
        PlanRisk::AmbiguousAsset { .. } | PlanRisk::UnmanagedFontOverlap { .. } => None,
    })
}

fn conflict_error_from_risk(default_package_id: &PackageId, risk: &PlanRisk) -> FontbrewError {
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

fn source_label(source: &ManifestSource) -> String {
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

fn source_conflict_risk(
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

fn current_install_risks(
    paths: &FontbrewPaths,
    manifest: &ManifestV1,
    prepared: &PreparedInstallPackage,
) -> Result<Vec<PlanRisk>> {
    let unmanaged_overlap_risks = unmanaged_same_family_overlap_risks(paths, manifest, prepared)?;
    current_install_risks_with_unmanaged_overlap_risks(manifest, prepared, unmanaged_overlap_risks)
}

fn current_install_risks_with_unmanaged_overlap_risks(
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
        strategy: prepared.activation_strategy,
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

fn unmanaged_same_family_overlap_risks(
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

fn unmanaged_same_family_overlap_risks_for_prepared_packages(
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

fn unmanaged_overlap_risks_for_families(
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

fn resolve_local_archive_path(path: &Path) -> Result<PathBuf> {
    let absolute_path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };

    let resolved = fs::canonicalize(&absolute_path)?;
    if !resolved.is_file() {
        return Err(FontbrewError::PathResolution {
            message: format!("local archive path is not a file: {}", resolved.display()),
        });
    }

    Ok(resolved)
}

fn new_staging_dir(paths: &FontbrewPaths) -> Result<PathBuf> {
    Ok(paths
        .staging_dir()
        .join(format!("install-{}", operation_suffix()?)))
}

fn create_active_staging_dir(paths: &FontbrewPaths) -> Result<PathBuf> {
    let staging_dir = new_staging_dir(paths)?;
    ensure_existing_path_does_not_cross_symlink(&paths.managed_store_dir(), &staging_dir)?;
    fs::create_dir_all(&staging_dir)?;
    fs::write(
        staging_dir.join(ACTIVE_STAGING_MARKER),
        format!("created_unix_seconds={}\n", current_unix_seconds()?),
    )?;
    Ok(staging_dir)
}

pub(crate) fn cleanup_stale_install_staging(paths: &FontbrewPaths) -> Result<()> {
    let staging_root = paths.staging_dir();
    if !staging_root.exists() {
        return Ok(());
    }

    ensure_existing_path_does_not_cross_symlink(&paths.managed_store_dir(), &staging_root)?;
    let now_seconds = current_unix_seconds()?;
    for entry in fs::read_dir(&staging_root)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("install-") {
            continue;
        }

        let path = entry.path();
        ensure_path_inside(&staging_root, &path)?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            fs::remove_file(path)?;
        } else if file_type.is_dir() {
            if has_live_active_staging_marker(&path, now_seconds)? {
                continue;
            }
            ensure_existing_path_does_not_cross_symlink(&staging_root, &path)?;
            fs::remove_dir_all(path)?;
        }
    }

    Ok(())
}

fn has_live_active_staging_marker(path: &Path, now_seconds: u64) -> Result<bool> {
    let marker_path = path.join(ACTIVE_STAGING_MARKER);
    match fs::symlink_metadata(&marker_path) {
        Ok(metadata) => {
            if !metadata.is_file() {
                return Ok(false);
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    }

    let content = fs::read_to_string(marker_path)?;
    let Some(created_seconds) = content
        .trim()
        .strip_prefix("created_unix_seconds=")
        .and_then(|value| value.parse::<u64>().ok())
    else {
        return Ok(false);
    };

    Ok(now_seconds.saturating_sub(created_seconds) <= ACTIVE_STAGING_LEASE_SECS)
}

fn current_unix_seconds() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| FontbrewError::PathResolution {
            message: format!("system clock is before unix epoch: {error}"),
        })?
        .as_secs())
}

fn operation_suffix() -> Result<String> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| FontbrewError::PathResolution {
            message: format!("system clock is before unix epoch: {error}"),
        })?
        .as_nanos();

    let counter = OPERATION_COUNTER.fetch_add(1, Ordering::Relaxed);

    Ok(format!("{timestamp}-{counter}"))
}

pub(crate) fn cleanup_staging(path: &Path) {
    let _ = fs::remove_dir_all(path);
}

fn ensure_path_inside(parent: &Path, child: &Path) -> Result<()> {
    let relative_path = child
        .strip_prefix(parent)
        .map_err(|_| FontbrewError::PathResolution {
            message: format!(
                "managed path must stay under {}: {}",
                parent.display(),
                child.display()
            ),
        })?;

    if relative_path
        .components()
        .all(|component| matches!(component, Component::Normal(_)))
    {
        Ok(())
    } else {
        Err(FontbrewError::PathResolution {
            message: format!(
                "managed path contains an unsafe component: {}",
                child.display()
            ),
        })
    }
}

fn installed_at_now() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default();
    format!("unix:{seconds}")
}

pub(crate) fn write_lock_path(paths: &FontbrewPaths) -> PathBuf {
    paths.managed_store_dir().join(".fontbrew.lock")
}

fn package_not_installed_error(package_id: &PackageId) -> FontbrewError {
    FontbrewError::Manifest {
        message: format!("package is not installed: {:?}", package_id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn selected_family_boundary() -> InstallFamilyBoundary {
        InstallFamilyBoundary::from_selected_families(vec![FamilyName::new("Source Code Pro")])
            .expect("selected family should create a boundary")
    }

    fn face(family: &str) -> FontFaceMetadata {
        FontFaceMetadata {
            family_name: FamilyName::new(family),
            subfamily_name: None,
            full_name: None,
            postscript_name: None,
            weight: Some(400),
            is_italic: false,
            is_oblique: false,
            format: FontFileFormat::Ttc,
            face_index: 0,
        }
    }

    #[test]
    fn family_boundary_filter_rejects_mixed_family_font_file() {
        let boundary = selected_family_boundary();
        let files = vec![ParsedFontFile {
            staging_path: PathBuf::from("Mixed.ttc"),
            faces: vec![face("Source Code Pro"), face("Inter")],
            format: FontFormat::Ttc,
        }];

        let error = filter_parsed_files_by_family_boundary(files, &boundary)
            .expect_err("mixed-family binary should not be partially filtered");

        assert!(matches!(error, FontbrewError::ArchiveRejected { .. }));
        let message = error.to_string();
        assert!(message.contains("one font binary"));
        assert!(message.contains("Mixed.ttc"));
        assert!(message.contains("Source Code Pro"));
        assert!(message.contains("Inter"));
    }

    #[test]
    fn family_boundary_filter_discards_whole_nonincluded_files() {
        let boundary = selected_family_boundary();
        let files = vec![
            ParsedFontFile {
                staging_path: PathBuf::from("SourceCodePro-Collection.ttc"),
                faces: vec![face("Source Code Pro"), face("Source Code Pro")],
                format: FontFormat::Ttc,
            },
            ParsedFontFile {
                staging_path: PathBuf::from("Inter-Variable.ttf"),
                faces: vec![face("Inter")],
                format: FontFormat::Ttf,
            },
        ];

        let filtered = filter_parsed_files_by_family_boundary(files, &boundary)
            .expect("whole-file filtering should succeed");

        assert_eq!(filtered.len(), 1);
        assert_eq!(
            filtered[0].staging_path,
            PathBuf::from("SourceCodePro-Collection.ttc")
        );
        assert_eq!(filtered[0].faces.len(), 2);
        assert!(filtered[0]
            .faces
            .iter()
            .all(|face| face.family_name.as_str() == "Source Code Pro"));
    }
}
