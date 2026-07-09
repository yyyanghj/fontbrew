use globset::Glob;
use serde::Deserialize;
use std::{path::Path, sync::Arc};

use crate::{
    error::{FontbrewError, Result},
    fetch::{HttpHeader, HttpRequest, NetworkClient},
    model::{CancellationToken, PackageVersion},
    sources::GitHubRepo,
};

const GITHUB_TOKEN_ENV_VAR: &str = "GITHUB_TOKEN";
const MAX_RELEASE_ASSET_DOWNLOAD_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedGitHubAsset {
    pub version: PackageVersion,
    pub download_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedGitHubRelease {
    pub version: PackageVersion,
    assets: Vec<ResolvedGitHubReleaseAsset>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedGitHubReleaseAsset {
    name: String,
    download_url: String,
}

impl ResolvedGitHubRelease {
    pub(crate) fn installable_asset_names(&self) -> Vec<String> {
        self.assets
            .iter()
            .filter(|asset| is_installable_archive_asset(&asset.name))
            .map(|asset| asset.name.clone())
            .collect()
    }
}

pub(crate) async fn resolve_release_asset(
    network_client: &NetworkClient,
    repo: &GitHubRepo,
    asset_selector: Option<&str>,
    source: &str,
) -> Result<ResolvedGitHubAsset> {
    let release = resolve_latest_stable_release(network_client, repo).await?;

    select_resolved_release_asset(&release, asset_selector, source)
}

pub(crate) async fn resolve_latest_stable_release(
    network_client: &NetworkClient,
    repo: &GitHubRepo,
) -> Result<ResolvedGitHubRelease> {
    let release = fetch_latest_stable_release(network_client, repo).await?;
    let version = release_version(&release)?;
    let assets = release
        .assets
        .into_iter()
        .map(|asset| ResolvedGitHubReleaseAsset {
            name: asset.name,
            download_url: asset.browser_download_url,
        })
        .collect();

    Ok(ResolvedGitHubRelease { version, assets })
}

pub(crate) fn select_resolved_release_asset(
    release: &ResolvedGitHubRelease,
    asset_selector: Option<&str>,
    source: &str,
) -> Result<ResolvedGitHubAsset> {
    let asset = select_release_asset(release, asset_selector, source)?;

    Ok(ResolvedGitHubAsset {
        version: release.version.clone(),
        download_url: asset.download_url,
    })
}

pub(crate) async fn resolve_latest_stable_release_version(
    network_client: &NetworkClient,
    repo: &GitHubRepo,
) -> Result<PackageVersion> {
    let release = fetch_latest_stable_release(network_client, repo).await?;

    release_version(&release)
}

pub(crate) async fn download_release_asset_to_file(
    network_client: &NetworkClient,
    url: &str,
    destination: &Path,
    cancellation: Arc<dyn CancellationToken>,
) -> Result<u64> {
    network_client
        .download_to_file(
            HttpRequest {
                url: url.to_string(),
                display_url: None,
                headers: github_headers(),
            },
            destination,
            MAX_RELEASE_ASSET_DOWNLOAD_BYTES,
            cancellation,
        )
        .await
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

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct GitHubErrorResponse {
    message: String,
    documentation_url: Option<String>,
}

async fn fetch_latest_stable_release(
    network_client: &NetworkClient,
    repo: &GitHubRepo,
) -> Result<GitHubRelease> {
    let url = format!(
        "{}/repos/{}/{}/releases",
        network_client.github_api_base_url(),
        repo.owner,
        repo.repo
    );
    let response = network_client
        .get(HttpRequest {
            url: url.clone(),
            display_url: None,
            headers: github_headers(),
        })
        .await?;
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
    release: &ResolvedGitHubRelease,
    asset_selector: Option<&str>,
    source: &str,
) -> Result<ResolvedGitHubReleaseAsset> {
    let mut candidates = release
        .assets
        .iter()
        .filter(|asset| is_installable_archive_asset(&asset.name))
        .cloned()
        .collect::<Vec<_>>();

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
                release.version.as_str()
            ),
        }),
        1 => Ok(candidates.remove(0)),
        _ => Err(FontbrewError::AmbiguousAssets {
            source_label: source.to_string(),
            assets: candidates.into_iter().map(|asset| asset.name).collect(),
        }),
    }
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

fn is_installable_archive_asset(name: &str) -> bool {
    name.to_ascii_lowercase().ends_with(".zip")
}

fn successful_response_body(status: u16, body: Vec<u8>, url: &str) -> Result<Vec<u8>> {
    if (200..300).contains(&status) {
        return Ok(body);
    }

    Err(FontbrewError::Network {
        message: github_api_error_message(status, &body, url),
    })
}

fn github_api_error_message(status: u16, body: &[u8], url: &str) -> String {
    let fallback = || format!("HTTP request failed with status {status} for {url}");
    let Ok(error) = serde_json::from_slice::<GitHubErrorResponse>(body) else {
        return fallback();
    };
    let github_message = error.message.trim();
    if github_message.is_empty() {
        return fallback();
    }

    let mut message = if github_message
        .to_ascii_lowercase()
        .contains("rate limit exceeded")
    {
        format!(
            "GitHub API rate limit exceeded for {url}; set GITHUB_TOKEN to use authenticated requests. GitHub response: {github_message}"
        )
    } else {
        format!("HTTP request failed with status {status} for {url}: {github_message}")
    };

    if let Some(documentation_url) = error.documentation_url.as_deref() {
        let documentation_url = documentation_url.trim();
        if !documentation_url.is_empty() {
            message.push_str("; docs: ");
            message.push_str(documentation_url);
        }
    }

    message
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
