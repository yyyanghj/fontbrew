use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

mod apply;
mod plan;
mod prepare;
mod staging;

pub(crate) use apply::{
    activation_artifacts_from_record, copy_prepared_files, manifest_record_from_prepared,
};
use apply::{
    apply_prepared_install, cleanup_install_plan_staging, conflict_error_from_risk,
    current_install_risks, current_install_risks_with_unmanaged_overlap_risks,
    dry_run_install_report, first_blocking_conflict_description, install_report_from_record,
    managed_activation_artifacts_from_record, managed_font_files_from_record,
    manifest_source_from_prepared, manifest_update_source_from_prepared, require_policy_for_risks,
    source_conflict_risk, source_label, unmanaged_same_family_overlap_risks,
};
use plan::install_plan_candidate_from_parsed_archive;
pub(crate) use plan::{
    family_install_plans_from_parsed_archive, install_candidate_from_prepared,
    install_candidates_from_parsed_archive, install_plans_from_parsed_archive_targets,
};
use prepare::{
    extract_and_parse_archive, extract_archive_to_parsed_archive, prepare_github_release_archive,
    prepare_github_release_parsed_archive,
};
pub(crate) use prepare::{
    prepare_provider_package, prepare_resolved_github_release_archive,
    prepare_resolved_github_release_parsed_archive, prepare_resolved_provider_package,
};
pub(crate) use staging::{cleanup_staging, cleanup_stale_install_staging};
use staging::{
    create_active_staging_dir, ensure_path_inside, operation_suffix, StagingCleanupGuard,
};

use crate::{
    activation::{
        deactivate, ActivationArtifact, ActivationPlan, ActivationPlanner, ActivationRequest,
    },
    archives::{ArchiveExtractionOptions, ExtractedFontFile, ZipArchiveExtractor},
    config::{dedupe_formats, font_format_label, FontbrewConfig, LoadedFontbrewConfig},
    error::{FontbrewError, Result},
    fetch::NetworkClient,
    fonts::{FontFaceMetadata, FontFileFormat, FontMetadataReader, TtfParserMetadataReader},
    fs::{ensure_existing_path_does_not_cross_symlink, GlobalFileLock},
    manifest::{
        ManifestActivationArtifactRecord, ManifestFontFileFormat, ManifestFontFileRecord,
        ManifestPackageRecord, ManifestSource, ManifestStore, ManifestV1,
    },
    model::{
        ensure_not_cancelled, CancellationToken, ExecutionPolicy, FontFormat, InfoReport,
        InfoRequest, InstallPlan, InstallReport, InstallRequest, InstallSource, ListPackage,
        ListReport, ManagedActivationArtifact, ManagedFontFile, NoProgress, PackageInfo,
        PlannedChange, PreparedFontFace, PreparedFontFile, PreparedInstallPackage,
        PreparedInstallSource, ProgressEvent, ProgressSink, ProgressSubject, RemovePlan,
        RemoveReport, RemoveRequest,
    },
    platform::FontbrewPaths,
    providers::{self, github, FontsourceProvider, ProviderFontAsset, ResolvedProviderPackage},
    sources::GitHubRepo,
    FamilyName, PackageId, PackageVersion, PlanRisk, ProviderKind,
};

const LOCAL_ARCHIVE_VERSION: &str = "local";
const MAX_PROVIDER_FONT_DOWNLOAD_BYTES: u64 = 64 * 1024 * 1024;
const MAX_PROVIDER_TOTAL_DOWNLOAD_BYTES: u64 = 512 * 1024 * 1024;
const MAX_PROVIDER_FONT_FILES: usize = 256;

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
    AssetSelection(PendingGitHubAssetSelection),
    FamilySelection {
        parsed_archive: ParsedFontArchive,
        package_id_override: Option<PackageId>,
    },
}

pub(crate) struct PendingGitHubAssetSelection {
    source_label: String,
    assets: Vec<String>,
    release: github::ResolvedGitHubRelease,
    source: PreparedInstallSource,
    options: RemoteInstallOptions,
    package_id_hint: Option<PackageId>,
    package_id_override: Option<PackageId>,
    family_boundary: Option<InstallFamilyBoundary>,
}

impl PendingGitHubAssetSelection {
    pub(crate) fn source_label(&self) -> &str {
        &self.source_label
    }

    pub(crate) fn assets(&self) -> &[String] {
        &self.assets
    }
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

pub(crate) fn prepare_local_archive_install_source(
    paths: &FontbrewPaths,
    archive_path: PathBuf,
    format_preference: Vec<FontFormat>,
    progress: &mut dyn ProgressSink,
    cancellation: &dyn CancellationToken,
) -> Result<ParsedFontArchive> {
    ensure_not_cancelled(cancellation)?;
    cleanup_stale_install_staging(paths)?;
    let archive_path = resolve_local_archive_path(&archive_path)?;
    prepare_local_archive_as_parsed_archive(
        paths,
        archive_path,
        None,
        false,
        format_preference,
        progress,
        cancellation,
    )
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
    if family_boundary.is_none() {
        if let Some(package_id) = &package_id_override {
            let requested_source = ManifestSource::LocalArchive {
                path: archive_path.clone(),
            };
            if let Some(plan) =
                already_installed_plan(paths, package_id, reinstall, &requested_source, None)?
            {
                return Ok(InstallPlanCandidate::Plan(plan));
            }
        }
    }
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

    let options = RemoteInstallOptions::from_request(request)?;
    let known_package_id = known_github_package_id(&options);
    let requested_source = ManifestSource::GitHub {
        owner: repo.owner.clone(),
        repo: repo.repo.clone(),
    };
    let requested_update_source = Some(requested_source.clone());
    if let Some(package_id) = &known_package_id {
        if let Some(plan) = already_installed_plan(
            paths,
            package_id,
            options.reinstall,
            &requested_source,
            requested_update_source.as_ref(),
        )? {
            return Ok(plan);
        }
    }

    let source_label = repo.label();
    progress.emit(ProgressEvent::ResolvingSource {
        source: source_label.clone(),
    });
    let options = options.with_progress_subject(ProgressSubject::source(source_label.clone()));
    let prepared = prepare_github_release_archive(
        paths,
        &repo,
        source_label,
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

pub(crate) async fn prepare_github_repo_install_source(
    paths: &FontbrewPaths,
    repo: GitHubRepo,
    asset_selector: Option<String>,
    format_preference: Vec<FontFormat>,
    progress: &mut dyn ProgressSink,
    network_client: &NetworkClient,
    cancellation: Arc<dyn CancellationToken>,
) -> Result<ParsedFontArchive> {
    ensure_not_cancelled(cancellation.as_ref())?;
    cleanup_stale_install_staging(paths)?;
    ensure_not_cancelled(cancellation.as_ref())?;

    let source_label = repo.label();
    progress.emit(ProgressEvent::ResolvingSource {
        source: source_label.clone(),
    });
    let options = RemoteInstallOptions {
        asset_selector,
        package_id: None,
        progress_subject: Some(ProgressSubject::source(source_label.clone())),
        reinstall: false,
        explicit_format_preference: dedupe_formats(format_preference),
        family_boundary: None,
    };
    let parsed_archive = prepare_github_release_parsed_archive(
        paths,
        &repo,
        source_label,
        PreparedInstallSource::GitHub {
            owner: repo.owner.clone(),
            repo: repo.repo.clone(),
        },
        options,
        progress,
        network_client,
        cancellation,
    )
    .await?;

    Ok(parsed_archive)
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

    let options = RemoteInstallOptions::from_request(request)?;
    let package_id_override = options.package_id.clone();
    let known_package_id = known_github_package_id(&options);
    let requested_source = ManifestSource::GitHub {
        owner: repo.owner.clone(),
        repo: repo.repo.clone(),
    };
    let requested_update_source = Some(requested_source.clone());
    if let Some(package_id) = &known_package_id {
        if let Some(plan) = already_installed_plan(
            paths,
            package_id,
            options.reinstall,
            &requested_source,
            requested_update_source.as_ref(),
        )? {
            return Ok(InstallPlanCandidate::Plan(plan));
        }
    }

    let source_label = repo.label();
    progress.emit(ProgressEvent::ResolvingSource {
        source: source_label.clone(),
    });
    let options = options.with_progress_subject(ProgressSubject::source(source_label.clone()));
    let family_boundary = options.family_boundary.clone();
    let package_id_hint = options.package_id.clone();
    let source = PreparedInstallSource::GitHub {
        owner: repo.owner.clone(),
        repo: repo.repo.clone(),
    };
    let release = github::resolve_latest_stable_release(network_client, &repo).await?;
    let asset = match github::select_resolved_release_asset(
        &release,
        options.asset_selector.as_deref(),
        &source_label,
    ) {
        Ok(asset) => asset,
        Err(FontbrewError::AmbiguousAssets { assets, .. }) if options.asset_selector.is_none() => {
            return Ok(InstallPlanCandidate::AssetSelection(
                PendingGitHubAssetSelection {
                    source_label,
                    assets,
                    release,
                    source,
                    options,
                    package_id_hint,
                    package_id_override,
                    family_boundary,
                },
            ));
        }
        Err(error) => return Err(error),
    };
    let parsed_archive = prepare_resolved_github_release_parsed_archive(
        paths,
        asset,
        source,
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

pub(crate) async fn prepare_selected_github_asset(
    paths: &FontbrewPaths,
    pending: PendingGitHubAssetSelection,
    asset_selector: String,
    progress: &mut dyn ProgressSink,
    network_client: &NetworkClient,
    cancellation: Arc<dyn CancellationToken>,
) -> Result<InstallPlanCandidate> {
    ensure_not_cancelled(cancellation.as_ref())?;
    let PendingGitHubAssetSelection {
        source_label,
        release,
        source,
        options,
        package_id_hint,
        package_id_override,
        family_boundary,
        ..
    } = pending;
    let asset =
        github::select_resolved_release_asset(&release, Some(&asset_selector), &source_label)?;
    let parsed_archive = prepare_resolved_github_release_parsed_archive(
        paths,
        asset,
        source,
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
    let source_label = repo.label();
    progress.emit(ProgressEvent::ResolvingSource {
        source: source_label.clone(),
    });
    let options = options.with_progress_subject(ProgressSubject::source(source_label.clone()));
    let parsed_archive = prepare_github_release_parsed_archive(
        paths,
        &repo,
        source_label,
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

pub(crate) async fn prepare_fontsource_install_source(
    paths: &FontbrewPaths,
    provider_id: String,
    format_preference: Vec<FontFormat>,
    progress: &mut dyn ProgressSink,
    network_client: &NetworkClient,
    cancellation: Arc<dyn CancellationToken>,
) -> Result<PreparedInstallPackage> {
    ensure_not_cancelled(cancellation.as_ref())?;
    cleanup_stale_install_staging(paths)?;
    ensure_not_cancelled(cancellation.as_ref())?;

    progress.emit(ProgressEvent::ResolvingSource {
        source: format!("fontsource:{provider_id}"),
    });
    let resolved = FontsourceProvider::new(paths, network_client)
        .resolve_install_package(&provider_id)
        .await?;
    let options = RemoteInstallOptions {
        asset_selector: None,
        package_id: Some(resolved.package_id.clone()),
        progress_subject: Some(ProgressSubject::package(&resolved.package_id)),
        reinstall: false,
        explicit_format_preference: dedupe_formats(format_preference),
        family_boundary: None,
    };

    prepare_provider_package(
        paths,
        resolved,
        options,
        progress,
        network_client,
        cancellation,
    )
    .await
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
    if selected_families.is_empty() {
        if let Some(package_id) = &package_id_override {
            let requested_source = ManifestSource::LocalArchive {
                path: archive_path.clone(),
            };
            if let Some(plan) =
                already_installed_plan(paths, package_id, reinstall, &requested_source, None)?
            {
                return Ok(plan);
            }
        }
    }
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

pub(crate) fn install_plan_from_prepared(
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
    let progress_subject = package_id_override.as_ref().map(ProgressSubject::package);
    let result = extract_and_parse_archive(
        paths,
        archive_path.clone(),
        staging_cleanup.path().to_path_buf(),
        PackageVersion::new(LOCAL_ARCHIVE_VERSION),
        PreparedInstallSource::LocalArchive { path: archive_path },
        package_id_override.clone(),
        progress_subject,
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
    let progress_subject = package_id_override.as_ref().map(ProgressSubject::package);
    let result = extract_archive_to_parsed_archive(
        paths,
        archive_path.clone(),
        staging_cleanup.path().to_path_buf(),
        PackageVersion::new(LOCAL_ARCHIVE_VERSION),
        PreparedInstallSource::LocalArchive { path: archive_path },
        progress_subject,
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
    pub(crate) progress_subject: Option<ProgressSubject>,
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
            progress_subject: None,
            reinstall: request.reinstall,
            explicit_format_preference: dedupe_formats(request.format_preference),
            family_boundary: InstallFamilyBoundary::from_selected_families(
                request.selected_families,
            ),
        })
    }

    fn with_progress_subject(mut self, subject: ProgressSubject) -> Self {
        self.progress_subject = Some(subject);
        self
    }

    pub(crate) fn for_update(package_id: PackageId) -> Self {
        Self {
            asset_selector: None,
            progress_subject: Some(ProgressSubject::package(&package_id)),
            package_id: Some(package_id),
            reinstall: false,
            explicit_format_preference: Vec::new(),
            family_boundary: None,
        }
    }
}

fn known_github_package_id(options: &RemoteInstallOptions) -> Option<PackageId> {
    if let Some(package_id) = &options.package_id {
        return Some(package_id.clone());
    }

    let boundary = options.family_boundary.as_ref()?;
    if boundary.expected_families().len() != 1 {
        return None;
    }

    PackageId::from_family_name(&boundary.expected_families()[0]).ok()
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

    fn selected_family_count(&self) -> usize {
        self.expected_families.len()
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParsedArchiveInstallTarget {
    pub(crate) family: FamilyName,
    pub(crate) package_id: Option<PackageId>,
    pub(crate) package_id_override: Option<PackageId>,
    pub(crate) reinstall: bool,
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
