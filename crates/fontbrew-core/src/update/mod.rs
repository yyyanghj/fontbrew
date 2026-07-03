use crate::{
    error::{FontbrewError, Result},
    fetch::HttpClient,
    github,
    manifest::{ManifestPackageRecord, ManifestSource, ManifestStore},
    model::{NotUpdatablePackage, OutdatedPackage, OutdatedReport, OutdatedRequest},
    platform::FontbrewPaths,
    sources::GitHubRepo,
    version::{compare_versions, VersionComparison},
    PackageId,
};

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
        let Some(repo) = github_update_repo(record)? else {
            not_updatable.push(not_updatable_package(record, "no GitHub update source"));
            continue;
        };

        if request.offline {
            not_updatable.push(not_updatable_package(
                record,
                "offline mode cannot check GitHub releases",
            ));
            continue;
        }

        let latest_version = github::resolve_latest_stable_release_version(http_client, &repo)?;
        match compare_versions(&record.version, &latest_version) {
            VersionComparison::CandidateIsNewer => packages.push(OutdatedPackage {
                package_id: record.package_id.clone(),
                current_version: record.version.clone(),
                latest_version,
            }),
            VersionComparison::Equal | VersionComparison::CurrentIsNewer => {}
            VersionComparison::Unknown => not_updatable.push(not_updatable_package(
                record,
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

fn selected_records<'a>(
    manifest: &'a crate::manifest::ManifestV1,
    package_ids: &[PackageId],
) -> Result<Vec<&'a ManifestPackageRecord>> {
    if package_ids.is_empty() {
        return Ok(manifest.packages.values().collect());
    }

    let mut records = Vec::with_capacity(package_ids.len());
    for package_id in package_ids {
        let record = manifest
            .get_package(package_id)
            .ok_or_else(|| package_not_installed_error(package_id))?;
        records.push(record);
    }

    Ok(records)
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
        ManifestSource::Registry { .. }
        | ManifestSource::Provider { .. }
        | ManifestSource::LocalArchive { .. } => Ok(None),
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
