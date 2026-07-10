use std::{fs, path::Path};

use crate::{
    config::{FontbrewConfig, LoadedFontbrewConfig},
    error::{FontbrewError, Result},
    fs::ensure_existing_path_does_not_cross_symlink,
    manifest::ManifestStore,
    model::{
        ensure_not_cancelled, CancellationToken, InstallCandidate, InstallCandidateFont,
        InstallCandidateId, InstallCandidateSource, InstallPlan, PreparedInstallPackage,
        PreparedInstallSource, ProgressEvent, ProgressSink,
    },
    platform::FontbrewPaths,
    FamilyName, PackageId,
};

use super::apply::{
    cleanup_install_plan_staging, unmanaged_overlap_risks_for_families,
    unmanaged_same_family_overlap_risks_for_prepared_packages,
};
use super::prepare::prepare_package_from_parsed_archive_with_config;
use super::staging::{create_active_staging_dir, ensure_path_inside, StagingCleanupGuard};
use super::{
    cleanup_staging, face_family_list, face_style, family_matches_any,
    install_plan_from_prepared_with_manifest, package_id_override_unsupported_source_error,
    prepared_package_id, validate_archive_family_boundary, InstallFamilyBoundary,
    ParsedArchiveInstallTarget, ParsedFontArchive, ParsedFontFile,
};

fn prepare_family_package_from_parsed_archive_target(
    paths: &FontbrewPaths,
    parsed_archive: &ParsedFontArchive,
    target: ParsedArchiveInstallTarget,
    loaded_config: &LoadedFontbrewConfig,
    cancellation: &dyn CancellationToken,
) -> Result<PreparedInstallPackage> {
    ensure_not_cancelled(cancellation)?;
    let staging_dir = create_active_staging_dir(paths)?;
    let mut staging_cleanup = StagingCleanupGuard::new(staging_dir);
    let boundary =
        InstallFamilyBoundary::from_selected_families(vec![target.family]).ok_or_else(|| {
            FontbrewError::ArchiveRejected {
                reason: "selected family boundary matched no font files".to_string(),
            }
        })?;
    let mut copied_archive = copy_parsed_archive_to_staging(
        paths,
        parsed_archive,
        staging_cleanup.path(),
        &boundary,
        cancellation,
    )?;
    copied_archive.reinstall = target.reinstall;
    let package_id_hint = target.package_id_override.or(target.package_id);
    let prepared = prepare_package_from_parsed_archive_with_config(
        paths,
        copied_archive,
        package_id_hint,
        Some(&boundary),
        None,
        loaded_config,
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
    boundary: &InstallFamilyBoundary,
    cancellation: &dyn CancellationToken,
) -> Result<ParsedFontArchive> {
    ensure_existing_path_does_not_cross_symlink(&paths.managed_store_dir(), staging_dir)?;
    fs::create_dir_all(staging_dir)?;

    let mut parsed_files = Vec::with_capacity(parsed_archive.parsed_files.len());
    for parsed_file in &parsed_archive.parsed_files {
        ensure_not_cancelled(cancellation)?;
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

pub(crate) fn install_candidates_from_parsed_archive(
    parsed_archive: &ParsedFontArchive,
    single_family_package_id_hint: Option<PackageId>,
) -> Result<Vec<InstallCandidate>> {
    let single_family_package_id_hint =
        (parsed_archive.archive_families.len() == 1).then_some(single_family_package_id_hint);
    let single_family_package_id_hint = single_family_package_id_hint.flatten();
    let mut candidates = Vec::with_capacity(parsed_archive.archive_families.len());

    for (index, family) in parsed_archive.archive_families.iter().enumerate() {
        let package_id = match &single_family_package_id_hint {
            Some(package_id) => Some(package_id.clone()),
            None => PackageId::from_family_name(family).ok(),
        };
        candidates.push(InstallCandidate {
            id: InstallCandidateId::new(format!("candidate-{index}")),
            package_id,
            families: vec![family.clone()],
            version: Some(parsed_archive.version.clone()),
            source: install_candidate_source(&parsed_archive.source),
            fonts: install_candidate_fonts_for_family(parsed_archive, family),
        });
    }

    Ok(candidates)
}

pub(crate) fn install_candidate_from_prepared(
    prepared: &PreparedInstallPackage,
) -> InstallCandidate {
    InstallCandidate {
        id: InstallCandidateId::new("candidate-0"),
        package_id: Some(prepared.package_id.clone()),
        families: prepared.families.clone(),
        version: Some(prepared.version.clone()),
        source: install_candidate_source(&prepared.source),
        fonts: prepared
            .font_files
            .iter()
            .flat_map(|font_file| {
                font_file.faces.iter().map(|face| InstallCandidateFont {
                    family: face.family.clone(),
                    style: face.style.clone(),
                    weight: face.weight,
                    format: face.format,
                })
            })
            .collect(),
    }
}

pub(crate) fn install_plans_from_parsed_archive_targets(
    paths: &FontbrewPaths,
    parsed_archive: ParsedFontArchive,
    targets: Vec<ParsedArchiveInstallTarget>,
    progress: &mut dyn ProgressSink,
    cancellation: &dyn CancellationToken,
) -> Result<Vec<InstallPlan>> {
    if targets.is_empty() {
        cleanup_staging(&parsed_archive.staging_dir);
        return Err(FontbrewError::Config {
            message: "install requires at least one target".to_string(),
        });
    }

    if targets
        .iter()
        .any(|target| target.package_id_override.is_some())
        && !matches!(
            parsed_archive.source,
            PreparedInstallSource::LocalArchive { .. } | PreparedInstallSource::GitHub { .. }
        )
    {
        cleanup_staging(&parsed_archive.staging_dir);
        return Err(package_id_override_unsupported_source_error());
    }

    let selected_families = targets
        .iter()
        .map(|target| target.family.clone())
        .collect::<Vec<_>>();
    let Some(all_selected_boundary) =
        InstallFamilyBoundary::from_selected_families(selected_families)
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
    let loaded_config = match FontbrewConfig::load_with_sources(&paths.config_path()) {
        Ok(config) => config,
        Err(error) => {
            cleanup_staging(&parsed_archive.staging_dir);
            return Err(error);
        }
    };
    for target in targets {
        let prepared_result = prepare_family_package_from_parsed_archive_target(
            paths,
            &parsed_archive,
            target,
            &loaded_config,
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

fn install_candidate_source(source: &PreparedInstallSource) -> InstallCandidateSource {
    match source {
        PreparedInstallSource::LocalArchive { path } => {
            InstallCandidateSource::LocalArchive { path: path.clone() }
        }
        PreparedInstallSource::GitHub { owner, repo } => InstallCandidateSource::GitHub {
            owner: owner.clone(),
            repo: repo.clone(),
        },
        PreparedInstallSource::Provider { provider, id } => InstallCandidateSource::Provider {
            provider: provider.clone(),
            id: id.clone(),
        },
    }
}

fn install_candidate_fonts_for_family(
    parsed_archive: &ParsedFontArchive,
    family: &FamilyName,
) -> Vec<InstallCandidateFont> {
    parsed_archive
        .parsed_files
        .iter()
        .flat_map(|file| {
            file.faces
                .iter()
                .filter(|face| family_matches_any(std::slice::from_ref(family), &face.family_name))
                .map(|face| InstallCandidateFont {
                    family: face.family_name.clone(),
                    style: face_style(face),
                    weight: face.weight.unwrap_or(400),
                    format: file.format,
                })
        })
        .collect()
}
