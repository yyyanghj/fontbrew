use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::PathBuf,
    sync::Arc,
};

use super::{
    cleanup_staging, create_active_staging_dir, dedupe_formats,
    ensure_existing_path_does_not_cross_symlink, ensure_not_cancelled,
    ensure_not_cancelled_after_prepare, ensure_path_inside, face_style,
    filter_parsed_files_by_family_boundary, font_format_from_reader_format, font_format_label,
    github, prepared_face_from_metadata, providers, reader_format_from_font_format,
    should_reject_unbounded_multiple_families, validate_archive_family_boundary,
    validate_expected_family_boundary, ActivationPlanner, ActivationRequest,
    ArchiveExtractionOptions, ArchiveFormatPreference, CancellationToken, ExtractedFontFile,
    FamilyName, FontFaceMetadata, FontFileFormat, FontFormat, FontMetadataReader, FontbrewConfig,
    FontbrewError, FontbrewPaths, GitHubRepo, InstallFamilyBoundary, LoadedFontbrewConfig,
    NetworkClient, NoProgress, PackageId, PackageVersion, ParsedFontArchive, ParsedFontFile,
    PreparedFontFile, PreparedInstallPackage, PreparedInstallSource, ProgressEvent, ProgressSink,
    ProgressSubject, ProviderFontAsset, RemoteInstallOptions, ResolvedProviderPackage, Result,
    StagingCleanupGuard, TtfParserMetadataReader, ZipArchiveExtractor,
    MAX_PROVIDER_FONT_DOWNLOAD_BYTES, MAX_PROVIDER_FONT_FILES, MAX_PROVIDER_TOTAL_DOWNLOAD_BYTES,
};

pub(super) fn prepare_package_from_parsed_archive(
    paths: &FontbrewPaths,
    parsed_archive: ParsedFontArchive,
    package_id_hint: Option<PackageId>,
    family_boundary: Option<&InstallFamilyBoundary>,
    package_families: Option<Vec<FamilyName>>,
    cancellation: &dyn CancellationToken,
) -> Result<PreparedInstallPackage> {
    let loaded_config = match FontbrewConfig::load_with_sources(&paths.config_path()) {
        Ok(config) => config,
        Err(error) => {
            cleanup_staging(&parsed_archive.staging_dir);
            return Err(error);
        }
    };
    prepare_package_from_parsed_archive_with_config(
        paths,
        parsed_archive,
        package_id_hint,
        family_boundary,
        package_families,
        &loaded_config,
        cancellation,
    )
}

pub(super) fn prepare_package_from_parsed_archive_with_config(
    paths: &FontbrewPaths,
    parsed_archive: ParsedFontArchive,
    package_id_hint: Option<PackageId>,
    family_boundary: Option<&InstallFamilyBoundary>,
    package_families: Option<Vec<FamilyName>>,
    loaded_config: &LoadedFontbrewConfig,
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
        None => match PackageId::from_family_name(package_family) {
            Ok(package_id) => package_id,
            Err(error) => {
                cleanup_staging(&staging_dir);
                return Err(error);
            }
        },
    };
    ensure_not_cancelled(cancellation)?;
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
    })?;

    Ok(PreparedInstallPackage {
        package_id,
        version,
        source,
        families,
        font_files,
        activation_dir: activation_plan.activation_dir,
        activation_artifacts: activation_plan.artifacts,
        activation_risks: activation_plan.risks,
        staging_dir,
        files_dir,
        package_store_dir,
        reinstall,
    })
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

pub(super) fn selected_family_names(parsed_files: &[ParsedFontFile]) -> Vec<FamilyName> {
    let mut families = BTreeSet::new();

    for parsed_file in parsed_files {
        for face in &parsed_file.faces {
            families.insert(face.family_name.as_str().to_string());
        }
    }

    families.into_iter().map(FamilyName::new).collect()
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn prepare_github_release_archive(
    paths: &FontbrewPaths,
    repo: &GitHubRepo,
    source_label: String,
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
        source_label,
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
pub(super) async fn prepare_github_release_parsed_archive(
    paths: &FontbrewPaths,
    repo: &GitHubRepo,
    source_label: String,
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
        source_label,
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

pub(crate) async fn prepare_provider_package(
    paths: &FontbrewPaths,
    resolved: ResolvedProviderPackage,
    options: RemoteInstallOptions,
    progress: &mut dyn ProgressSink,
    network_client: &NetworkClient,
    cancellation: Arc<dyn CancellationToken>,
) -> Result<PreparedInstallPackage> {
    ensure_not_cancelled(cancellation.as_ref())?;
    let (resolved, loaded_config) = select_provider_assets_for_format(paths, resolved, &options)?;
    let staging_dir = create_active_staging_dir(paths)?;
    let mut staging_cleanup = StagingCleanupGuard::new(staging_dir);
    ensure_not_cancelled(cancellation.as_ref())?;
    let result = download_and_parse_provider_fonts(
        paths,
        resolved,
        options,
        loaded_config,
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

fn select_provider_assets_for_format(
    paths: &FontbrewPaths,
    resolved: ResolvedProviderPackage,
    options: &RemoteInstallOptions,
) -> Result<(ResolvedProviderPackage, LoadedFontbrewConfig)> {
    let ResolvedProviderPackage {
        package_id,
        provider,
        provider_id,
        version,
        families,
        assets,
    } = resolved;
    let loaded_config = FontbrewConfig::load_with_sources(&paths.config_path())?;
    let archive_format_preference = ArchiveFormatPreference {
        explicit_format_preference: options.explicit_format_preference.clone(),
    };
    let selected_assets = select_preferred_provider_assets(
        &package_id,
        assets,
        &format_selection(
            &archive_format_preference,
            &loaded_config.config.format_preference,
            loaded_config.has_format_preference,
        ),
    )?;

    Ok((
        ResolvedProviderPackage {
            package_id,
            provider,
            provider_id,
            version,
            families,
            assets: selected_assets,
        },
        loaded_config,
    ))
}

fn select_preferred_provider_assets(
    package_id: &PackageId,
    assets: Vec<ProviderFontAsset>,
    format_selection: &FormatSelection,
) -> Result<Vec<ProviderFontAsset>> {
    let available_formats = assets
        .iter()
        .map(|asset| asset.format)
        .collect::<BTreeSet<_>>();
    if format_selection.explicit {
        let selected_format =
            requested_available_provider_format(package_id, format_selection, &available_formats)?;
        return Ok(assets
            .into_iter()
            .filter(|asset| asset.format == selected_format)
            .collect());
    }

    if available_formats.len() <= 1 {
        return Ok(assets);
    }

    let Some(selected_format) = format_selection
        .preference
        .iter()
        .find(|format| available_formats.contains(format))
        .copied()
        .or_else(|| available_formats.iter().next().copied())
    else {
        return Ok(assets);
    };

    Ok(assets
        .into_iter()
        .filter(|asset| asset.format == selected_format)
        .collect())
}

fn requested_available_provider_format(
    package_id: &PackageId,
    format_selection: &FormatSelection,
    available_formats: &BTreeSet<FontFormat>,
) -> Result<FontFormat> {
    format_selection
        .preference
        .iter()
        .find(|format| available_formats.contains(format))
        .copied()
        .ok_or_else(|| FontbrewError::Conflict {
            package_id: package_id.clone(),
            message: format!(
                "requested font formats are not available for {}; requested: {}; available: {}",
                package_id.as_str(),
                format_list_label(&format_selection.preference),
                format_list_label(available_formats)
            ),
        })
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

#[allow(clippy::too_many_arguments)]
async fn download_and_parse_provider_fonts(
    paths: &FontbrewPaths,
    resolved: ResolvedProviderPackage,
    options: RemoteInstallOptions,
    loaded_config: LoadedFontbrewConfig,
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
        subject: ProgressSubject::package(&resolved.package_id),
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
            subject: ProgressSubject::package(&resolved.package_id),
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
        loaded_config,
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
    source_label: String,
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
        &source_label,
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
    source_label: String,
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
        &source_label,
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

pub(crate) async fn prepare_resolved_github_release_parsed_archive(
    paths: &FontbrewPaths,
    asset: github::ResolvedGitHubAsset,
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
    let result = download_and_parse_resolved_github_archive_to_parsed_archive(
        paths,
        asset,
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
    let progress_subject = options
        .progress_subject
        .clone()
        .or_else(|| options.package_id.as_ref().map(ProgressSubject::package));
    if let Some(subject) = &progress_subject {
        progress.emit(ProgressEvent::DownloadStarted {
            subject: subject.clone(),
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
        progress_subject,
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
    let progress_subject = options
        .progress_subject
        .clone()
        .or_else(|| options.package_id.as_ref().map(ProgressSubject::package));
    if let Some(subject) = &progress_subject {
        progress.emit(ProgressEvent::DownloadStarted {
            subject: subject.clone(),
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
        progress_subject,
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
    progress_subject: Option<ProgressSubject>,
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
    progress_subject: Option<ProgressSubject>,
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
    loaded_config: LoadedFontbrewConfig,
    cancellation: Arc<dyn CancellationToken>,
}

#[derive(Default)]
struct RecordingProgressSink {
    events: Vec<ProgressEvent>,
}

pub(super) struct StagedFontFile {
    pub(super) path: PathBuf,
    pub(super) format: FontFileFormat,
    pub(super) weight_override: Option<u16>,
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
            input.progress_subject,
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
            input.progress_subject,
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
                subject: ProgressSubject::package(package_id),
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
            Some(&input.loaded_config),
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
pub(super) fn extract_and_parse_archive(
    paths: &FontbrewPaths,
    archive_path: PathBuf,
    staging_dir: PathBuf,
    version: PackageVersion,
    source: PreparedInstallSource,
    package_id_hint: Option<PackageId>,
    progress_subject: Option<ProgressSubject>,
    reinstall: bool,
    archive_format_preference: ArchiveFormatPreference,
    family_boundary: Option<InstallFamilyBoundary>,
    progress: &mut dyn ProgressSink,
    cancellation: &dyn CancellationToken,
) -> Result<PreparedInstallPackage> {
    ensure_existing_path_does_not_cross_symlink(&paths.managed_store_dir(), &staging_dir)?;
    ensure_not_cancelled(cancellation)?;

    if let Some(subject) = &progress_subject {
        progress.emit(ProgressEvent::ExtractingArchive {
            subject: subject.clone(),
        });
    }
    let extracted_fonts = ZipArchiveExtractor::new(ArchiveExtractionOptions::default())
        .extract(&archive_path, &staging_dir)?;
    ensure_not_cancelled(cancellation)?;

    if let Some(subject) = &progress_subject {
        progress.emit(ProgressEvent::ParsingFonts {
            subject: subject.clone(),
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
        None,
        cancellation,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn extract_archive_to_parsed_archive(
    paths: &FontbrewPaths,
    archive_path: PathBuf,
    staging_dir: PathBuf,
    version: PackageVersion,
    source: PreparedInstallSource,
    progress_subject: Option<ProgressSubject>,
    reinstall: bool,
    archive_format_preference: ArchiveFormatPreference,
    progress: &mut dyn ProgressSink,
    cancellation: &dyn CancellationToken,
) -> Result<ParsedFontArchive> {
    ensure_existing_path_does_not_cross_symlink(&paths.managed_store_dir(), &staging_dir)?;
    ensure_not_cancelled(cancellation)?;

    if let Some(subject) = &progress_subject {
        progress.emit(ProgressEvent::ExtractingArchive {
            subject: subject.clone(),
        });
    }
    let extracted_fonts = ZipArchiveExtractor::new(ArchiveExtractionOptions::default())
        .extract(&archive_path, &staging_dir)?;
    ensure_not_cancelled(cancellation)?;

    if let Some(subject) = &progress_subject {
        progress.emit(ProgressEvent::ParsingFonts {
            subject: subject.clone(),
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
    loaded_config: Option<&LoadedFontbrewConfig>,
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

    match loaded_config {
        Some(loaded_config) => prepare_package_from_parsed_archive_with_config(
            paths,
            parsed_archive,
            package_id_hint,
            family_boundary.as_ref(),
            package_families,
            loaded_config,
            cancellation,
        ),
        None => prepare_package_from_parsed_archive(
            paths,
            parsed_archive,
            package_id_hint,
            family_boundary.as_ref(),
            package_families,
            cancellation,
        ),
    }
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
