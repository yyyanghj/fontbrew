use std::{
    collections::BTreeSet,
    fs,
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{
    activation::{
        deactivate, ActivationArtifact, ActivationPlan, ActivationPlanner, ActivationRequest,
    },
    archives::{ArchiveExtractionOptions, ZipArchiveExtractor},
    config::FontbrewConfig,
    error::{FontbrewError, Result},
    fetch::HttpClient,
    fonts::{FontFaceMetadata, FontFileFormat, FontMetadataReader, TtfParserMetadataReader},
    fs::{ensure_existing_path_does_not_cross_symlink, GlobalFileLock},
    github,
    manifest::{
        ManifestActivationArtifactRecord, ManifestFontFileFormat, ManifestFontFileRecord,
        ManifestPackageRecord, ManifestSource, ManifestStore,
    },
    model::{
        CancellationToken, ExecutionPolicy, FontFormat, InfoReport, InfoRequest, InstallPlan,
        InstallReport, InstallRequest, InstallSource, ListPackage, ListReport, PackageInfo,
        PlannedChange, PreparedFontFace, PreparedFontFile, PreparedInstallPackage,
        PreparedInstallSource, ProgressEvent, ProgressSink, RemovePlan, RemoveReport,
        RemoveRequest,
    },
    platform::FontbrewPaths,
    registry::{RegistryAssetSelection, RegistryPackageRecipe},
    sources::GitHubRepo,
    FamilyName, PackageId, PackageVersion, PlanRisk,
};

const LOCAL_ARCHIVE_VERSION: &str = "local";
static OPERATION_COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn install_plan(paths: &FontbrewPaths, request: InstallRequest) -> Result<InstallPlan> {
    let InstallRequest {
        source, reinstall, ..
    } = request;

    match source {
        InstallSource::LocalPath(path) => local_archive_install_plan(paths, path, reinstall),
        _ => Err(FontbrewError::NotImplemented {
            operation: "install_source",
        }),
    }
}

pub fn github_repo_install_plan(
    paths: &FontbrewPaths,
    repo: GitHubRepo,
    request: InstallRequest,
    http_client: &dyn HttpClient,
) -> Result<InstallPlan> {
    let options = RemoteInstallOptions::from_request(request);
    let package_id = package_id_from_repo_name(&repo.repo)?;
    if let Some(plan) = already_installed_plan(paths, &package_id, options.reinstall)? {
        return Ok(plan);
    }

    let prepared = prepare_github_release_archive(
        paths,
        &repo,
        None,
        package_id.clone(),
        PreparedInstallSource::GitHub {
            owner: repo.owner.clone(),
            repo: repo.repo.clone(),
        },
        options.with_package_id(package_id),
        http_client,
    )?;

    install_plan_from_prepared(paths, prepared)
}

pub fn registry_recipe_install_plan(
    paths: &FontbrewPaths,
    recipe: RegistryPackageRecipe,
    request: InstallRequest,
    http_client: &dyn HttpClient,
) -> Result<InstallPlan> {
    let options = RemoteInstallOptions::from_request(request);
    let repo = recipe.github_repo.clone();
    let package_id = recipe.package_id.clone();
    if let Some(plan) = already_installed_plan(paths, &package_id, options.reinstall)? {
        return Ok(plan);
    }

    let prepared = prepare_github_release_archive(
        paths,
        &repo,
        recipe.asset.as_ref(),
        package_id.clone(),
        PreparedInstallSource::Registry {
            id: package_id.as_str().to_string(),
            github_owner: repo.owner.clone(),
            github_repo: repo.repo.clone(),
        },
        options.with_package_id(package_id),
        http_client,
    )?;

    install_plan_from_prepared(paths, prepared)
}

fn already_installed_plan(
    paths: &FontbrewPaths,
    package_id: &PackageId,
    reinstall: bool,
) -> Result<Option<InstallPlan>> {
    if reinstall {
        return Ok(None);
    }

    let manifest = ManifestStore::read_or_empty(&paths.manifest_path())?;
    let Some(record) = manifest.get_package(package_id) else {
        return Ok(None);
    };

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
    _cancellation: &dyn CancellationToken,
) -> Result<InstallReport> {
    if matches!(policy, ExecutionPolicy::DryRun) {
        return dry_run_install_report(plan);
    }

    require_policy_for_risks(&plan.risks, &policy)?;

    if !plan.risks.is_empty() {
        return Err(FontbrewError::Conflict {
            package_id: plan.package_id,
            message: "install plan contains unresolved conflicts".to_string(),
        });
    }

    let _lock = GlobalFileLock::try_exclusive(&write_lock_path(paths))?;
    let mut manifest = ManifestStore::read_or_empty(&paths.manifest_path())?;

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

    if let Some(record) = manifest.get_package(&prepared_package_id(&prepared)) {
        if !prepared.reinstall {
            cleanup_staging(&prepared.staging_dir);
            return Ok(install_report_from_record(record, false, true));
        }
    }

    let result = apply_prepared_install(paths, &mut manifest, &prepared, policy, progress);
    cleanup_staging(&prepared.staging_dir);

    result
}

pub fn list_packages(paths: &FontbrewPaths) -> Result<ListReport> {
    let manifest = ManifestStore::read_or_empty(&paths.manifest_path())?;
    let packages = manifest
        .packages
        .values()
        .map(|record| ListPackage {
            package_id: record.package_id.clone(),
            version: record.version.clone(),
            families: record.families.clone(),
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
            families: record.families.clone(),
            source: source_label(&record.source),
            activated: record.active_version.is_some(),
            update_source: record.update_source.as_ref().map(source_label),
        },
    })
}

pub fn remove_plan(paths: &FontbrewPaths, request: RemoveRequest) -> Result<RemovePlan> {
    let manifest = ManifestStore::read_or_empty(&paths.manifest_path())?;
    let changes = manifest
        .get_package(&request.package_id)
        .map(|record| {
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
            ]
        })
        .unwrap_or_default();

    Ok(RemovePlan {
        package_id: request.package_id,
        changes,
        risks: Vec::new(),
    })
}

pub fn apply_remove(
    paths: &FontbrewPaths,
    plan: RemovePlan,
    policy: ExecutionPolicy,
    progress: &mut dyn ProgressSink,
    _cancellation: &dyn CancellationToken,
) -> Result<RemoveReport> {
    require_policy_for_risks(&plan.risks, &policy)?;

    if matches!(policy, ExecutionPolicy::DryRun) {
        let planned = !plan.changes.is_empty();
        return Ok(RemoveReport {
            package_id: plan.package_id,
            removed: false,
            planned,
        });
    }

    let _lock = GlobalFileLock::try_exclusive(&write_lock_path(paths))?;
    let mut manifest = ManifestStore::read_or_empty(&paths.manifest_path())?;
    let Some(record) = manifest.get_package(&plan.package_id).cloned() else {
        return Ok(RemoveReport {
            package_id: plan.package_id,
            removed: false,
            planned: false,
        });
    };

    let activation_artifacts = activation_artifacts_from_record(&record);
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
    })
}

fn local_archive_install_plan(
    paths: &FontbrewPaths,
    archive_path: PathBuf,
    reinstall: bool,
) -> Result<InstallPlan> {
    let archive_path = resolve_local_archive_path(&archive_path)?;
    let prepared = prepare_local_archive(paths, archive_path, reinstall)?;
    install_plan_from_prepared(paths, prepared)
}

fn install_plan_from_prepared(
    paths: &FontbrewPaths,
    prepared: PreparedInstallPackage,
) -> Result<InstallPlan> {
    let package_id = prepared_package_id(&prepared);
    let manifest = match ManifestStore::read_or_empty(&paths.manifest_path()) {
        Ok(manifest) => manifest,
        Err(error) => {
            cleanup_staging(&prepared.staging_dir);
            return Err(error);
        }
    };

    if let Some(record) = manifest.get_package(&package_id) {
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

    let mut risks = prepared.activation_risks.clone();
    if prepared.package_store_dir.exists() && manifest.get_package(&package_id).is_none() {
        risks.push(PlanRisk::Conflict {
            package_id: package_id.clone(),
            description: format!(
                "package store directory exists outside manifest management: {}",
                prepared.package_store_dir.display()
            ),
        });
    }

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

fn prepare_local_archive(
    paths: &FontbrewPaths,
    archive_path: PathBuf,
    reinstall: bool,
) -> Result<PreparedInstallPackage> {
    let staging_dir = new_staging_dir(paths)?;
    let result = extract_and_parse_archive(
        paths,
        archive_path.clone(),
        staging_dir.clone(),
        PackageVersion::new(LOCAL_ARCHIVE_VERSION),
        PreparedInstallSource::LocalArchive { path: archive_path },
        None,
        reinstall,
    );

    if result.is_err() {
        cleanup_staging(&staging_dir);
    }

    result
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteInstallOptions {
    pub(crate) asset_selector: Option<String>,
    pub(crate) package_id: Option<PackageId>,
    pub(crate) reinstall: bool,
}

impl RemoteInstallOptions {
    fn from_request(request: InstallRequest) -> Self {
        Self {
            asset_selector: request.asset_selector,
            package_id: None,
            reinstall: request.reinstall,
        }
    }

    fn with_package_id(mut self, package_id: PackageId) -> Self {
        self.package_id = Some(package_id);
        self
    }

    pub(crate) fn for_update(package_id: PackageId) -> Self {
        Self {
            asset_selector: None,
            package_id: Some(package_id),
            reinstall: false,
        }
    }
}

fn prepare_github_release_archive(
    paths: &FontbrewPaths,
    repo: &GitHubRepo,
    recipe_asset: Option<&RegistryAssetSelection>,
    fallback_package_id: PackageId,
    source: PreparedInstallSource,
    options: RemoteInstallOptions,
    http_client: &dyn HttpClient,
) -> Result<PreparedInstallPackage> {
    let staging_dir = new_staging_dir(paths)?;
    let result = download_and_parse_github_archive(
        paths,
        repo,
        recipe_asset,
        fallback_package_id,
        source,
        options,
        http_client,
        staging_dir.clone(),
    );

    if result.is_err() {
        cleanup_staging(&staging_dir);
    }

    result
}

fn download_and_parse_github_archive(
    paths: &FontbrewPaths,
    repo: &GitHubRepo,
    recipe_asset: Option<&RegistryAssetSelection>,
    fallback_package_id: PackageId,
    source: PreparedInstallSource,
    options: RemoteInstallOptions,
    http_client: &dyn HttpClient,
    staging_dir: PathBuf,
) -> Result<PreparedInstallPackage> {
    ensure_existing_path_does_not_cross_symlink(&paths.managed_store_dir(), &staging_dir)?;

    let asset = github::resolve_release_asset(
        http_client,
        repo,
        recipe_asset,
        options.asset_selector.as_deref(),
        &fallback_package_id,
    )?;

    download_and_parse_resolved_github_archive(
        paths,
        asset,
        source,
        options,
        http_client,
        staging_dir,
    )
}

pub(crate) fn prepare_resolved_github_release_archive(
    paths: &FontbrewPaths,
    asset: github::ResolvedGitHubAsset,
    source: PreparedInstallSource,
    options: RemoteInstallOptions,
    http_client: &dyn HttpClient,
) -> Result<PreparedInstallPackage> {
    let staging_dir = new_staging_dir(paths)?;
    let result = download_and_parse_resolved_github_archive(
        paths,
        asset,
        source,
        options,
        http_client,
        staging_dir.clone(),
    );

    if result.is_err() {
        cleanup_staging(&staging_dir);
    }

    result
}

fn download_and_parse_resolved_github_archive(
    paths: &FontbrewPaths,
    asset: github::ResolvedGitHubAsset,
    source: PreparedInstallSource,
    options: RemoteInstallOptions,
    http_client: &dyn HttpClient,
    staging_dir: PathBuf,
) -> Result<PreparedInstallPackage> {
    fs::create_dir_all(&staging_dir)?;
    let archive_path = staging_dir.join("download.zip");
    github::download_release_asset_to_file(http_client, &asset.download_url, &archive_path)?;

    extract_and_parse_archive(
        paths,
        archive_path,
        staging_dir,
        asset.version,
        source,
        options.package_id,
        options.reinstall,
    )
}

fn extract_and_parse_archive(
    paths: &FontbrewPaths,
    archive_path: PathBuf,
    staging_dir: PathBuf,
    version: PackageVersion,
    source: PreparedInstallSource,
    package_id_hint: Option<PackageId>,
    reinstall: bool,
) -> Result<PreparedInstallPackage> {
    ensure_existing_path_does_not_cross_symlink(&paths.managed_store_dir(), &staging_dir)?;

    let extracted_fonts = ZipArchiveExtractor::new(ArchiveExtractionOptions::default())
        .extract(&archive_path, &staging_dir)?;

    if extracted_fonts.is_empty() {
        cleanup_staging(&staging_dir);
        return Err(FontbrewError::ArchiveRejected {
            reason: "archive contains no desktop font files".to_string(),
        });
    }

    let reader = TtfParserMetadataReader::default();
    let mut family_names = BTreeSet::new();
    let mut parsed_files = Vec::with_capacity(extracted_fonts.len());

    for extracted_font in extracted_fonts {
        let faces = match reader.read_file(&extracted_font.path) {
            Ok(faces) => faces,
            Err(error) => {
                cleanup_staging(&staging_dir);
                return Err(error);
            }
        };

        if faces.is_empty() {
            cleanup_staging(&staging_dir);
            return Err(FontbrewError::FontParse {
                message: format!(
                    "font file has no readable faces: {}",
                    extracted_font.path.display()
                ),
            });
        }

        for face in &faces {
            family_names.insert(face.family_name.as_str().to_string());
        }

        parsed_files.push((extracted_font.path, faces));
    }

    let Some(package_family) = family_names.iter().next() else {
        cleanup_staging(&staging_dir);
        return Err(FontbrewError::FontParse {
            message: "archive contained no readable font families".to_string(),
        });
    };

    let package_id = match package_id_hint {
        Some(package_id) => package_id,
        None => PackageId::normalize(package_family)?,
    };
    let package_store_dir = paths.package_store_dir(&package_id, &version);
    let files_dir = package_store_dir.join("files");
    let families = family_names.into_iter().map(FamilyName::new).collect();
    let mut font_files = Vec::with_capacity(parsed_files.len());
    let mut activation_sources = Vec::with_capacity(parsed_files.len());

    for (staging_path, faces) in parsed_files {
        let relative_path =
            staging_path
                .strip_prefix(&staging_dir)
                .map_err(|_| FontbrewError::PathResolution {
                    message: format!(
                        "staged font path is outside staging directory: {}",
                        staging_path.display()
                    ),
                })?;
        let stored_path = files_dir.join(relative_path);
        let prepared_faces = faces.iter().map(prepared_face_from_metadata).collect();

        activation_sources.push(stored_path.clone());
        font_files.push(PreparedFontFile {
            staging_path,
            stored_path,
            faces: prepared_faces,
        });
    }

    let config = match FontbrewConfig::load(&paths.config_path()) {
        Ok(config) => config,
        Err(error) => {
            cleanup_staging(&staging_dir);
            return Err(error);
        }
    };
    let activation_plan = ActivationPlanner::plan(ActivationRequest {
        package_id: package_id.clone(),
        font_files: activation_sources,
        activation_dir: paths.activation_dir(),
        strategy: config.activation_strategy,
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
) -> Result<InstallReport> {
    reject_unmanaged_package_store(paths, prepared)?;

    let backup_dir = if prepared.reinstall && prepared.package_store_dir.exists() {
        Some(backup_existing_package_store(paths, prepared)?)
    } else {
        None
    };

    if let Err(error) = copy_prepared_files(paths, prepared) {
        rollback_package_store(&prepared.package_store_dir, backup_dir.as_deref());
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
                rollback_package_store(&prepared.package_store_dir, backup_dir.as_deref());
                return Err(error);
            }
        };
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
            );
            return Err(error);
        }
    };
    let manifest_record = manifest_record_from_prepared(prepared, activation_artifacts.clone())?;

    manifest.insert_package(manifest_record.clone())?;
    if let Err(error) = ManifestStore::write(&paths.manifest_path(), manifest) {
        let rollback_artifacts =
            rollback_activation_artifacts(&activation_artifacts, &preexisting_activation_paths);
        rollback_install(
            paths,
            &rollback_artifacts,
            &prepared.package_store_dir,
            backup_dir.as_deref(),
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

fn manifest_font_format(format: &FontFormat) -> ManifestFontFileFormat {
    match format {
        FontFormat::Ttf => ManifestFontFileFormat::Ttf,
        FontFormat::Otf => ManifestFontFileFormat::Otf,
        FontFormat::Ttc => ManifestFontFileFormat::Ttc,
        FontFormat::Otc => ManifestFontFileFormat::Otc,
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
) {
    let _ = deactivate(&paths.activation_dir(), activation_artifacts);
    rollback_package_store(package_store_dir, backup_dir);
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
        PreparedInstallSource::Registry { id, .. } => ManifestSource::Registry { id: id.clone() },
    }
}

fn manifest_update_source_from_prepared(source: &PreparedInstallSource) -> Option<ManifestSource> {
    match source {
        PreparedInstallSource::LocalArchive { .. } => None,
        PreparedInstallSource::GitHub { owner, repo } => Some(ManifestSource::GitHub {
            owner: owner.clone(),
            repo: repo.clone(),
        }),
        PreparedInstallSource::Registry {
            github_owner,
            github_repo,
            ..
        } => Some(ManifestSource::GitHub {
            owner: github_owner.clone(),
            repo: github_repo.clone(),
        }),
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

fn source_label(source: &ManifestSource) -> String {
    match source {
        ManifestSource::Registry { id } => format!("registry:{id}"),
        ManifestSource::GitHub { owner, repo } => format!("github:{owner}/{repo}"),
        ManifestSource::Provider { provider, id } => format!("provider:{provider:?}:{id}"),
        ManifestSource::LocalArchive { path } => format!("local archive:{}", path.display()),
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
