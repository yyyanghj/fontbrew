use globset::{Glob, GlobSetBuilder};
use serde::Deserialize;
use std::path::Path;

use crate::{
    error::{FontbrewError, Result},
    fetch::{HttpClient, HttpHeader, HttpRequest},
    model::{CancellationToken, PackageId, PackageVersion},
    registry::RegistryAssetSelection,
    sources::GitHubRepo,
};

const GITHUB_API_BASE_URL: &str = "https://api.github.com";
const GITHUB_TOKEN_ENV_VAR: &str = "GITHUB_TOKEN";
const MAX_RELEASE_ASSET_DOWNLOAD_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedGitHubAsset {
    pub version: PackageVersion,
    pub download_url: String,
}

pub(crate) fn resolve_release_asset(
    http_client: &dyn HttpClient,
    repo: &GitHubRepo,
    recipe_asset: Option<&RegistryAssetSelection>,
    asset_selector: Option<&str>,
    package_id: &PackageId,
) -> Result<ResolvedGitHubAsset> {
    let release = fetch_latest_stable_release(http_client, repo)?;
    let version = release_version(&release)?;
    let asset = select_release_asset(&release, recipe_asset, asset_selector, package_id)?;

    Ok(ResolvedGitHubAsset {
        version,
        download_url: asset.browser_download_url,
    })
}

pub(crate) fn resolve_latest_stable_release_version(
    http_client: &dyn HttpClient,
    repo: &GitHubRepo,
) -> Result<PackageVersion> {
    let release = fetch_latest_stable_release(http_client, repo)?;

    release_version(&release)
}

pub(crate) fn download_release_asset_to_file(
    http_client: &dyn HttpClient,
    url: &str,
    destination: &Path,
    cancellation: &dyn CancellationToken,
) -> Result<u64> {
    http_client.download_to_file(
        HttpRequest {
            url: url.to_string(),
            headers: github_headers(),
        },
        destination,
        MAX_RELEASE_ASSET_DOWNLOAD_BYTES,
        cancellation,
    )
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<GitHubReleaseAsset>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct GitHubReleaseAsset {
    name: String,
    browser_download_url: String,
}

fn fetch_latest_stable_release(
    http_client: &dyn HttpClient,
    repo: &GitHubRepo,
) -> Result<GitHubRelease> {
    let url = format!(
        "{GITHUB_API_BASE_URL}/repos/{}/{}/releases",
        repo.owner, repo.repo
    );
    let response = http_client.get(HttpRequest {
        url: url.clone(),
        headers: github_headers(),
    })?;
    let body = successful_response_body(response.status, response.body, &url)?;
    let releases: Vec<GitHubRelease> =
        serde_json::from_slice(&body).map_err(|source| FontbrewError::Network {
            message: format!(
                "could not parse GitHub releases for {}: {source}",
                repo.label()
            ),
        })?;

    releases
        .into_iter()
        .find(|release| !release.draft && !release.prerelease)
        .ok_or_else(|| FontbrewError::ArchiveRejected {
            reason: format!("GitHub repository {} has no stable releases", repo.label()),
        })
}

fn release_version(release: &GitHubRelease) -> Result<PackageVersion> {
    PackageVersion::parse(&release.tag_name)
}

fn select_release_asset(
    release: &GitHubRelease,
    recipe_asset: Option<&RegistryAssetSelection>,
    asset_selector: Option<&str>,
    package_id: &PackageId,
) -> Result<GitHubReleaseAsset> {
    let mut candidates = release
        .assets
        .iter()
        .filter(|asset| is_installable_archive_asset(&asset.name))
        .cloned()
        .collect::<Vec<_>>();

    if let Some(recipe_asset) = recipe_asset {
        candidates = filter_assets_by_recipe(candidates, recipe_asset)?;
    }

    if let Some(selector) = asset_selector {
        let mut selected = Vec::new();
        for asset in candidates {
            if asset_matches_selector(&asset.name, selector)? {
                selected.push(asset);
            }
        }
        candidates = selected;
    }

    match candidates.len() {
        0 => Err(FontbrewError::ArchiveRejected {
            reason: format!(
                "GitHub release {} has no matching installable zip assets",
                release.tag_name
            ),
        }),
        1 => Ok(candidates.remove(0)),
        _ => Err(FontbrewError::AmbiguousAssets {
            package_id: package_id.clone(),
            assets: candidates.into_iter().map(|asset| asset.name).collect(),
        }),
    }
}

fn filter_assets_by_recipe(
    assets: Vec<GitHubReleaseAsset>,
    recipe_asset: &RegistryAssetSelection,
) -> Result<Vec<GitHubReleaseAsset>> {
    let include = compile_glob_set(&recipe_asset.include)?;
    let exclude = compile_glob_set(&recipe_asset.exclude)?;

    Ok(assets
        .into_iter()
        .filter(|asset| {
            let included = recipe_asset.include.is_empty() || include.is_match(&asset.name);
            let excluded = !recipe_asset.exclude.is_empty() && exclude.is_match(&asset.name);
            included && !excluded
        })
        .collect())
}

fn asset_matches_selector(asset_name: &str, selector: &str) -> Result<bool> {
    if asset_name == selector {
        return Ok(true);
    }

    let matcher = Glob::new(selector)
        .map_err(|source| FontbrewError::ArchiveRejected {
            reason: format!("invalid asset selector {selector:?}: {source}"),
        })?
        .compile_matcher();

    Ok(matcher.is_match(asset_name))
}

fn compile_glob_set(patterns: &[String]) -> Result<globset::GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(
            Glob::new(pattern).map_err(|source| FontbrewError::ArchiveRejected {
                reason: format!("invalid asset glob {pattern:?}: {source}"),
            })?,
        );
    }

    builder
        .build()
        .map_err(|source| FontbrewError::ArchiveRejected {
            reason: format!("could not compile asset globs: {source}"),
        })
}

fn is_installable_archive_asset(name: &str) -> bool {
    name.to_ascii_lowercase().ends_with(".zip")
}

fn successful_response_body(status: u16, body: Vec<u8>, url: &str) -> Result<Vec<u8>> {
    if (200..300).contains(&status) {
        return Ok(body);
    }

    Err(FontbrewError::Network {
        message: format!("HTTP request failed with status {status} for {url}"),
    })
}

fn github_headers() -> Vec<HttpHeader> {
    let mut headers = vec![
        HttpHeader {
            name: "User-Agent".to_string(),
            value: "fontbrew".to_string(),
        },
        HttpHeader {
            name: "Accept".to_string(),
            value: "application/vnd.github+json".to_string(),
        },
        HttpHeader {
            name: "X-GitHub-Api-Version".to_string(),
            value: "2022-11-28".to_string(),
        },
    ];

    if let Ok(token) = std::env::var(GITHUB_TOKEN_ENV_VAR) {
        let token = token.trim();
        if !token.is_empty() {
            headers.push(HttpHeader {
                name: "Authorization".to_string(),
                value: format!("Bearer {token}"),
            });
        }
    }

    headers
}
