use std::{
    cmp::Ordering,
    env, fs, io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use flate2::read::GzDecoder;
use fontbrew_core::{
    fetch::{HttpHeader, HttpRequest, NetworkClient},
    fs::GlobalFileLock,
    platform::FontbrewPaths,
    CancellationToken, FontbrewError,
};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    confirm::Confirmer,
    exit::{CliError, CliResult},
    reporter::Reporter,
};

const DEFAULT_FONTBREW_REPO: &str = "yyyanghj/fontbrew";
const FONTBREW_REPO_ENV_VAR: &str = "FONTBREW_REPO";
const GITHUB_TOKEN_ENV_VAR: &str = "GITHUB_TOKEN";
const GITHUB_API_BASE_URL: &str = "https://api.github.com";
const MAX_SELF_UPDATE_DOWNLOAD_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct SelfUpdateRequest {
    pub dry_run: bool,
    pub assume_yes: bool,
    pub force: bool,
    pub current_executable: PathBuf,
    pub current_version: String,
    pub lock_path: PathBuf,
    pub repo: String,
}

impl SelfUpdateRequest {
    pub fn from_environment(dry_run: bool, assume_yes: bool, force: bool) -> CliResult<Self> {
        let paths = FontbrewPaths::resolve()?;
        Ok(Self {
            dry_run,
            assume_yes,
            force,
            current_executable: env::current_exe()?,
            current_version: env!("CARGO_PKG_VERSION").to_string(),
            lock_path: paths.managed_store_dir().join("self-update.lock"),
            repo: env::var(FONTBREW_REPO_ENV_VAR)
                .ok()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| DEFAULT_FONTBREW_REPO.to_string()),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SelfUpdateReport {
    pub current_version: String,
    pub latest_version: String,
    pub target_version: String,
    pub executable_path: PathBuf,
    pub install_method: SelfUpdateInstallMethod,
    pub status: SelfUpdateStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub enum SelfUpdateInstallMethod {
    Standalone,
    Homebrew,
    Macports,
    Cargo,
    DevBuild,
    System,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SelfUpdateStatus {
    UpToDate,
    Planned,
    Updated,
    Reinstalled,
    SkippedNewerCurrent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReleaseSelection {
    latest_version: Version,
    archive_asset: GitHubReleaseAsset,
    checksum_asset: GitHubReleaseAsset,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlannedAction {
    SkipUpToDate,
    SkipNewerCurrent,
    Update,
    Reinstall,
}

pub async fn run(
    request: SelfUpdateRequest,
    reporter: &mut dyn Reporter,
    confirmer: &mut dyn Confirmer,
    cancellation: std::sync::Arc<dyn CancellationToken>,
) -> CliResult<()> {
    let network_client = NetworkClient::new()?;

    run_with_network_client(request, &network_client, reporter, confirmer, cancellation).await
}

async fn run_with_network_client(
    request: SelfUpdateRequest,
    network_client: &NetworkClient,
    reporter: &mut dyn Reporter,
    confirmer: &mut dyn Confirmer,
    cancellation: std::sync::Arc<dyn CancellationToken>,
) -> CliResult<()> {
    run_with_network_client_with_target_override(
        request,
        network_client,
        reporter,
        confirmer,
        cancellation,
        None,
    )
    .await
}

#[cfg(test)]
async fn run_with_network_client_for_target(
    request: SelfUpdateRequest,
    network_client: &NetworkClient,
    github_api_base_url: &str,
    reporter: &mut dyn Reporter,
    confirmer: &mut dyn Confirmer,
    cancellation: std::sync::Arc<dyn CancellationToken>,
    target: &str,
) -> CliResult<()> {
    run_with_network_client_with_target_override(
        request,
        network_client,
        reporter,
        confirmer,
        cancellation,
        SelfUpdateRunOverrides {
            target: Some(target),
            github_api_base_url,
        },
    )
    .await
}

async fn run_with_network_client_with_target_override(
    request: SelfUpdateRequest,
    network_client: &NetworkClient,
    reporter: &mut dyn Reporter,
    confirmer: &mut dyn Confirmer,
    cancellation: std::sync::Arc<dyn CancellationToken>,
    overrides: impl Into<SelfUpdateRunOverrides<'_>>,
) -> CliResult<()> {
    ensure_not_cancelled(cancellation.as_ref())?;

    let install_method = detect_install_method(&request.current_executable, home_dir().as_deref())?;
    let overrides = overrides.into();
    let target = match overrides.target {
        Some(target) => target,
        None => current_target()?,
    };
    reporter.self_update_progress("Checking latest fontbrew release...")?;
    let release = resolve_latest_release(
        network_client,
        &request.repo,
        overrides.github_api_base_url,
        target,
    )
    .await?;
    let current_version = parse_current_version(&request.current_version)?;
    let action = planned_action(&current_version, &release.latest_version, request.force);
    let latest_version = release.latest_version.to_string();

    if action == PlannedAction::SkipUpToDate || action == PlannedAction::SkipNewerCurrent {
        let status = match action {
            PlannedAction::SkipUpToDate => SelfUpdateStatus::UpToDate,
            PlannedAction::SkipNewerCurrent => SelfUpdateStatus::SkippedNewerCurrent,
            PlannedAction::Update | PlannedAction::Reinstall => unreachable!(),
        };
        return reporter.render_self_update_report(SelfUpdateReport {
            current_version: current_version.to_string(),
            latest_version: latest_version.clone(),
            target_version: latest_version,
            executable_path: request.current_executable,
            install_method,
            status,
            backup_path: None,
        });
    }

    if request.dry_run {
        return reporter.render_self_update_report(SelfUpdateReport {
            current_version: current_version.to_string(),
            latest_version: latest_version.clone(),
            target_version: latest_version,
            executable_path: request.current_executable,
            install_method,
            status: SelfUpdateStatus::Planned,
            backup_path: None,
        });
    }

    confirmer.confirm_self_update(
        &request.current_executable,
        &latest_version,
        request.assume_yes,
    )?;

    let staging_dir = create_staging_dir_for(&request.current_executable)?;
    let result = async {
        let prepared = prepare_release(
            network_client,
            &release,
            target,
            &staging_dir,
            reporter,
            cancellation.clone(),
        )
        .await?;
        ensure_not_cancelled(cancellation.as_ref())?;
        reporter.self_update_progress(&format!(
            "Installing {}...",
            request.current_executable.display()
        ))?;
        replace_prepared_release(&request, &release.latest_version, &prepared).await
    }
    .await;
    let _ = fs::remove_dir_all(&staging_dir);
    let locked_outcome = result?;

    reporter.render_self_update_report(SelfUpdateReport {
        current_version: locked_outcome.current_version.to_string(),
        latest_version: latest_version.clone(),
        target_version: latest_version,
        executable_path: request.current_executable,
        install_method,
        status: locked_outcome.status,
        backup_path: None,
    })
}

struct SelfUpdateRunOverrides<'a> {
    target: Option<&'a str>,
    github_api_base_url: &'a str,
}

impl<'a> From<Option<&'a str>> for SelfUpdateRunOverrides<'a> {
    fn from(target: Option<&'a str>) -> Self {
        Self {
            target,
            github_api_base_url: GITHUB_API_BASE_URL,
        }
    }
}

#[derive(Debug, Clone)]
struct PreparedSelfUpdate {
    new_binary_path: PathBuf,
}

struct PreparedReleaseInput {
    archive_path: PathBuf,
    checksum_path: PathBuf,
    expected_archive_name: String,
    target: String,
    new_binary_path: PathBuf,
}

struct LockedReplaceInput {
    current_executable: PathBuf,
    lock_path: PathBuf,
    force: bool,
    latest_version: Version,
    new_binary_path: PathBuf,
}

struct LockedReplaceOutcome {
    current_version: Version,
    status: SelfUpdateStatus,
}

async fn prepare_release(
    network_client: &NetworkClient,
    release: &ReleaseSelection,
    target: &str,
    staging_dir: &Path,
    reporter: &mut dyn Reporter,
    cancellation: std::sync::Arc<dyn CancellationToken>,
) -> CliResult<PreparedSelfUpdate> {
    let archive_path = staging_dir.join(&release.archive_asset.name);
    let checksum_path = staging_dir.join(&release.checksum_asset.name);
    let new_binary_path = staging_dir.join("fontbrew.new");

    reporter.self_update_progress(&format!("Downloading {}...", release.archive_asset.name))?;
    download_asset(
        network_client,
        &release.archive_asset.browser_download_url,
        &archive_path,
        cancellation.clone(),
    )
    .await?;
    ensure_not_cancelled(cancellation.as_ref())?;

    reporter.self_update_progress(&format!("Downloading {}...", release.checksum_asset.name))?;
    download_asset(
        network_client,
        &release.checksum_asset.browser_download_url,
        &checksum_path,
        cancellation.clone(),
    )
    .await?;
    ensure_not_cancelled(cancellation.as_ref())?;

    reporter.self_update_progress("Verifying checksum...")?;
    ensure_not_cancelled(cancellation.as_ref())?;
    prepare_release_blocking(PreparedReleaseInput {
        archive_path,
        checksum_path,
        expected_archive_name: release.archive_asset.name.clone(),
        target: target.to_string(),
        new_binary_path,
    })
    .await
}

async fn prepare_release_blocking(input: PreparedReleaseInput) -> CliResult<PreparedSelfUpdate> {
    spawn_self_update_blocking(move || {
        verify_checksum_file(
            &input.archive_path,
            &input.checksum_path,
            &input.expected_archive_name,
        )?;
        extract_expected_binary(&input.archive_path, &input.target, &input.new_binary_path)?;
        set_executable(&input.new_binary_path)?;
        smoke_test_binary(&input.new_binary_path)?;

        Ok(PreparedSelfUpdate {
            new_binary_path: input.new_binary_path,
        })
    })
    .await
}

async fn resolve_latest_release(
    network_client: &NetworkClient,
    repo: &str,
    github_api_base_url: &str,
    target: &str,
) -> CliResult<ReleaseSelection> {
    let (owner, name) = repo
        .split_once('/')
        .ok_or_else(|| CliError::SelfUpdateInvalidRelease {
            message: format!("{FONTBREW_REPO_ENV_VAR} must be owner/repo, found {repo:?}"),
        })?;
    if owner.is_empty() || name.is_empty() || name.contains('/') {
        return Err(CliError::SelfUpdateInvalidRelease {
            message: format!("{FONTBREW_REPO_ENV_VAR} must be owner/repo, found {repo:?}"),
        });
    }

    let url = format!(
        "{}/repos/{owner}/{name}/releases",
        github_api_base_url.trim_end_matches('/')
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
        serde_json::from_slice(&body).map_err(|source| CliError::SelfUpdateInvalidRelease {
            message: format!("could not parse GitHub releases for {repo}: {source}"),
        })?;
    let release = releases
        .into_iter()
        .find(|release| !release.draft && !release.prerelease)
        .ok_or_else(|| CliError::SelfUpdateInvalidRelease {
            message: format!("GitHub repository {repo} has no stable releases"),
        })?;
    let latest_version = parse_latest_release_version(&release.tag_name)?;
    let archive_name = format!("fontbrew-{target}.tar.gz");
    let checksum_name = format!("{archive_name}.sha256");
    let archive_asset = exact_release_asset(&release, &archive_name)?;
    let checksum_asset = exact_release_asset(&release, &checksum_name)?;

    Ok(ReleaseSelection {
        latest_version,
        archive_asset,
        checksum_asset,
    })
}

fn successful_response_body(status: u16, body: Vec<u8>, url: &str) -> CliResult<Vec<u8>> {
    if (200..300).contains(&status) {
        return Ok(body);
    }

    Err(FontbrewError::Network {
        message: format!("HTTP request failed with status {status} for {url}"),
    }
    .into())
}

fn exact_release_asset(release: &GitHubRelease, name: &str) -> CliResult<GitHubReleaseAsset> {
    release
        .assets
        .iter()
        .find(|asset| asset.name == name)
        .cloned()
        .ok_or_else(|| CliError::SelfUpdateInvalidRelease {
            message: format!(
                "GitHub release {} is missing required asset {name}",
                release.tag_name
            ),
        })
}

async fn download_asset(
    network_client: &NetworkClient,
    url: &str,
    destination: &Path,
    cancellation: std::sync::Arc<dyn CancellationToken>,
) -> CliResult<()> {
    network_client
        .download_to_file(
            HttpRequest {
                url: url.to_string(),
                display_url: None,
                headers: github_headers(),
            },
            destination,
            MAX_SELF_UPDATE_DOWNLOAD_BYTES,
            cancellation,
        )
        .await?;

    Ok(())
}

fn verify_checksum_file(
    archive_path: &Path,
    checksum_path: &Path,
    expected_filename: &str,
) -> CliResult<()> {
    let checksum = fs::read_to_string(checksum_path)?;
    let expected = parse_sha256_checksum(&checksum, expected_filename)?;
    let actual = sha256_file(archive_path)?;
    if actual == expected {
        return Ok(());
    }

    Err(CliError::SelfUpdateChecksumMismatch {
        message: format!("checksum mismatch for {expected_filename}"),
    })
}

fn parse_sha256_checksum(contents: &str, expected_filename: &str) -> CliResult<[u8; 32]> {
    let line = contents
        .lines()
        .find(|line| !line.trim().is_empty())
        .ok_or_else(|| CliError::SelfUpdateInvalidRelease {
            message: "checksum file is empty".to_string(),
        })?;
    let mut parts = line.split_whitespace();
    let hash = parts
        .next()
        .ok_or_else(|| CliError::SelfUpdateInvalidRelease {
            message: "checksum file is missing the SHA-256 hash".to_string(),
        })?;
    let filename = parts
        .next()
        .ok_or_else(|| CliError::SelfUpdateInvalidRelease {
            message: "checksum file is missing the asset filename".to_string(),
        })?;
    if parts.next().is_some() {
        return Err(CliError::SelfUpdateInvalidRelease {
            message: "checksum file has unexpected extra fields".to_string(),
        });
    }

    let filename = filename.strip_prefix('*').unwrap_or(filename);
    if filename != expected_filename {
        return Err(CliError::SelfUpdateInvalidRelease {
            message: format!("checksum file is for {filename}, expected {expected_filename}"),
        });
    }

    parse_sha256_hex(hash)
}

fn parse_sha256_hex(hash: &str) -> CliResult<[u8; 32]> {
    if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(CliError::SelfUpdateInvalidRelease {
            message: "checksum file contains a malformed SHA-256 hash".to_string(),
        });
    }

    let mut bytes = [0_u8; 32];
    for (index, byte) in bytes.iter_mut().enumerate() {
        let start = index * 2;
        *byte = u8::from_str_radix(&hash[start..start + 2], 16).map_err(|source| {
            CliError::SelfUpdateInvalidRelease {
                message: format!("checksum file contains a malformed SHA-256 hash: {source}"),
            }
        })?;
    }

    Ok(bytes)
}

fn sha256_file(path: &Path) -> CliResult<[u8; 32]> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    io::copy(&mut file, &mut hasher)?;
    Ok(hasher.finalize().into())
}

fn extract_expected_binary(archive_path: &Path, target: &str, destination: &Path) -> CliResult<()> {
    let archive_file = fs::File::open(archive_path)?;
    let decoder = GzDecoder::new(archive_file);
    let mut archive = tar::Archive::new(decoder);
    let expected_path = PathBuf::from(format!("fontbrew-{target}/fontbrew"));

    for entry in archive.entries()? {
        let mut entry = entry?;
        let entry_path = entry.path()?.into_owned();
        if entry_path != expected_path {
            continue;
        }

        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(destination)?;
        io::copy(&mut entry, &mut output)?;
        output.sync_all()?;
        return Ok(());
    }

    Err(CliError::SelfUpdateInvalidRelease {
        message: format!(
            "release archive is missing expected entry {}",
            expected_path.display()
        ),
    })
}

async fn replace_prepared_release(
    request: &SelfUpdateRequest,
    latest_version: &Version,
    prepared: &PreparedSelfUpdate,
) -> CliResult<LockedReplaceOutcome> {
    replace_prepared_release_blocking(LockedReplaceInput {
        current_executable: request.current_executable.clone(),
        lock_path: request.lock_path.clone(),
        force: request.force,
        latest_version: latest_version.clone(),
        new_binary_path: prepared.new_binary_path.clone(),
    })
    .await
}

async fn replace_prepared_release_blocking(
    input: LockedReplaceInput,
) -> CliResult<LockedReplaceOutcome> {
    spawn_self_update_blocking(move || {
        let _lock = GlobalFileLock::try_exclusive(&input.lock_path)?;
        detect_install_method(&input.current_executable, home_dir().as_deref())?;
        let locked_current_version = read_executable_version(&input.current_executable)?;
        let action = planned_action(&locked_current_version, &input.latest_version, input.force);
        let status = locked_replace_status(action);
        match action {
            PlannedAction::Update | PlannedAction::Reinstall => {
                replace_executable(&input.current_executable, &input.new_binary_path)?;
            }
            PlannedAction::SkipUpToDate | PlannedAction::SkipNewerCurrent => {}
        }

        Ok(LockedReplaceOutcome {
            current_version: locked_current_version,
            status,
        })
    })
    .await
}

fn locked_replace_status(action: PlannedAction) -> SelfUpdateStatus {
    match action {
        PlannedAction::Update => SelfUpdateStatus::Updated,
        PlannedAction::Reinstall => SelfUpdateStatus::Reinstalled,
        PlannedAction::SkipUpToDate => SelfUpdateStatus::UpToDate,
        PlannedAction::SkipNewerCurrent => SelfUpdateStatus::SkippedNewerCurrent,
    }
}

async fn spawn_self_update_blocking<T>(
    work: impl FnOnce() -> CliResult<T> + Send + 'static,
) -> CliResult<T>
where
    T: Send + 'static,
{
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|source| CliError::SelfUpdateFailed {
            message: format!("self-update blocking task failed: {source}"),
        })?
}

fn replace_executable(current_executable: &Path, new_binary_path: &Path) -> CliResult<()> {
    let parent = current_executable
        .parent()
        .ok_or_else(|| CliError::SelfUpdateFailed {
            message: format!(
                "current executable has no parent directory: {}",
                current_executable.display()
            ),
        })?;
    let backup_path = backup_path_for(current_executable)?;

    inject_replacement_failure(ReplacementFailurePoint::BeforeBackup)?;
    fs::rename(current_executable, &backup_path).map_err(|source| CliError::SelfUpdateFailed {
        message: format!(
            "could not create backup {}: {source}",
            backup_path.display()
        ),
    })?;
    sync_directory(parent)?;

    if let Err(replace_error) = inject_replacement_failure(ReplacementFailurePoint::AfterBackup) {
        return restore_after_failed_replace_error(current_executable, &backup_path, replace_error);
    }
    if let Err(replace_error) = fs::rename(new_binary_path, current_executable) {
        return restore_after_failed_replace(current_executable, &backup_path, replace_error);
    }
    sync_directory(parent)?;

    if let Err(smoke_error) = inject_replacement_failure(ReplacementFailurePoint::AfterInstall)
        .and_then(|_| smoke_test_binary(current_executable))
    {
        return restore_after_failed_smoke(current_executable, &backup_path, smoke_error);
    }

    fs::remove_file(&backup_path).map_err(|source| CliError::SelfUpdateFailed {
        message: format!(
            "updated fontbrew, but could not remove backup {}: {source}",
            backup_path.display()
        ),
    })?;
    sync_directory(parent)?;

    Ok(())
}

fn restore_after_failed_replace(
    current_executable: &Path,
    backup_path: &Path,
    replace_error: io::Error,
) -> CliResult<()> {
    restore_after_failed_replace_message(
        current_executable,
        backup_path,
        &replace_error.to_string(),
    )
}

fn restore_after_failed_replace_error(
    current_executable: &Path,
    backup_path: &Path,
    replace_error: CliError,
) -> CliResult<()> {
    restore_after_failed_replace_message(current_executable, backup_path, &replace_error.message())
}

fn restore_after_failed_replace_message(
    current_executable: &Path,
    backup_path: &Path,
    replace_error: &str,
) -> CliResult<()> {
    match fs::rename(backup_path, current_executable) {
        Ok(()) => Err(CliError::SelfUpdateFailed {
            message: format!(
                "could not replace {}: {replace_error}; restored original executable",
                current_executable.display()
            ),
        }),
        Err(restore_error) => Err(CliError::SelfUpdateFailed {
            message: format!(
                "could not replace {}: {replace_error}; restore failed: {restore_error}; backup remains at {}",
                current_executable.display(),
                backup_path.display()
            ),
        }),
    }
}

fn restore_after_failed_smoke(
    current_executable: &Path,
    backup_path: &Path,
    smoke_error: CliError,
) -> CliResult<()> {
    match fs::rename(backup_path, current_executable) {
        Ok(()) => Err(CliError::SelfUpdateFailed {
            message: format!(
                "installed fontbrew failed its smoke test: {}; restored original executable",
                smoke_error.message()
            ),
        }),
        Err(restore_error) => Err(CliError::SelfUpdateFailed {
            message: format!(
                "installed fontbrew failed its smoke test: {}; restore failed: {restore_error}; backup remains at {}",
                smoke_error.message(),
                backup_path.display()
            ),
        }),
    }
}

fn backup_path_for(current_executable: &Path) -> CliResult<PathBuf> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|source| CliError::SelfUpdateFailed {
            message: format!("system clock is before Unix epoch: {source}"),
        })?
        .as_secs();
    let file_name = current_executable
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| CliError::SelfUpdateFailed {
            message: format!(
                "current executable path has no valid filename: {}",
                current_executable.display()
            ),
        })?;

    Ok(current_executable.with_file_name(format!(
        "{file_name}.old-{timestamp}-{}",
        std::process::id()
    )))
}

fn read_executable_version(path: &Path) -> CliResult<Version> {
    let output = Command::new(path)
        .arg("--version")
        .output()
        .map_err(|source| CliError::SelfUpdateFailed {
            message: format!("could not run {} --version: {source}", path.display()),
        })?;
    if !output.status.success() {
        return Err(CliError::SelfUpdateFailed {
            message: format!(
                "{} --version failed with status {}",
                path.display(),
                output.status
            ),
        });
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let version = stdout
        .split_whitespace()
        .find_map(|part| Version::parse(part.strip_prefix('v').unwrap_or(part)).ok())
        .ok_or_else(|| CliError::SelfUpdateFailed {
            message: format!(
                "could not read current fontbrew version from {} --version output",
                path.display()
            ),
        })?;

    Ok(version)
}

fn smoke_test_binary(path: &Path) -> CliResult<()> {
    let output = Command::new(path)
        .arg("--version")
        .output()
        .map_err(|source| CliError::SelfUpdateFailed {
            message: format!("could not run {} --version: {source}", path.display()),
        })?;
    if output.status.success() {
        return Ok(());
    }

    Err(CliError::SelfUpdateFailed {
        message: format!(
            "{} --version failed with status {}",
            path.display(),
            output.status
        ),
    })
}

fn detect_install_method(
    current_executable: &Path,
    home_dir: Option<&Path>,
) -> CliResult<SelfUpdateInstallMethod> {
    let path_text = current_executable.to_string_lossy();

    if protected_path_starts_with(current_executable, "/opt/homebrew")
        || protected_path_starts_with(current_executable, "/usr/local/Cellar")
        || protected_path_starts_with(current_executable, "/usr/local/Homebrew")
        || protected_path_starts_with(current_executable, "/usr/local/opt")
    {
        return unavailable(
            "This fontbrew binary appears to be managed by Homebrew. Run: brew upgrade fontbrew",
        );
    }

    if protected_path_starts_with(current_executable, "/opt/local") {
        return unavailable(
            "This fontbrew binary appears to be managed by MacPorts. Run: sudo port selfupdate && sudo port upgrade fontbrew",
        );
    }

    if let Some(home_dir) = home_dir {
        if current_executable.starts_with(home_dir.join(".cargo/bin")) {
            return unavailable(
                "This fontbrew binary appears to be managed by Cargo. Run: cargo install fontbrew-cli --force",
            );
        }
    }

    if path_text.contains("/target/debug/") || path_text.contains("/target/release/") {
        return unavailable(
            "This fontbrew binary is running from a development build. Build or install a release binary before using self-update",
        );
    }

    if protected_path_starts_with(current_executable, "/nix/store")
        || protected_path_starts_with(current_executable, "/run/current-system")
        || protected_path_starts_with(current_executable, "/usr/bin")
        || protected_path_starts_with(current_executable, "/bin")
    {
        return unavailable(
            "This fontbrew binary is in a protected system or package-manager path. Use that system to update fontbrew",
        );
    }

    if current_executable
        .file_name()
        .and_then(|name| name.to_str())
        != Some("fontbrew")
    {
        return unavailable("This executable is not named fontbrew, so it cannot be self-updated");
    }

    let metadata =
        fs::metadata(current_executable).map_err(|source| CliError::SelfUpdateUnavailable {
            message: format!(
                "could not inspect current executable {}: {source}",
                current_executable.display()
            ),
        })?;
    if !metadata.is_file() {
        return unavailable("The current fontbrew executable path is not a regular file");
    }
    ensure_current_executable_has_write_bit(current_executable, &metadata)?;
    ensure_parent_writable(current_executable)?;

    Ok(SelfUpdateInstallMethod::Standalone)
}

fn unavailable<T>(message: &str) -> CliResult<T> {
    Err(CliError::SelfUpdateUnavailable {
        message: message.to_string(),
    })
}

fn protected_path_starts_with(path: &Path, prefix: &str) -> bool {
    path.starts_with(prefix)
}

fn ensure_current_executable_has_write_bit(
    current_executable: &Path,
    metadata: &fs::Metadata,
) -> CliResult<()> {
    if metadata.permissions().mode() & 0o222 != 0 {
        return Ok(());
    }

    Err(CliError::SelfUpdateUnavailable {
        message: format!(
            "current executable is not writable: {}",
            current_executable.display()
        ),
    })
}

fn ensure_parent_writable(current_executable: &Path) -> CliResult<()> {
    let parent = current_executable
        .parent()
        .ok_or_else(|| CliError::SelfUpdateUnavailable {
            message: format!(
                "current executable has no parent directory: {}",
                current_executable.display()
            ),
        })?;
    let probe_path = parent.join(format!(".fontbrew-write-check-{}", std::process::id()));
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe_path)
        .and_then(|file| {
            drop(file);
            fs::remove_file(&probe_path)
        })
        .map_err(|source| CliError::SelfUpdateUnavailable {
            message: format!(
                "current executable directory is not writable: {}: {source}",
                parent.display()
            ),
        })
}

fn planned_action(current: &Version, latest: &Version, force: bool) -> PlannedAction {
    match current.cmp(latest) {
        Ordering::Less => PlannedAction::Update,
        Ordering::Equal if force => PlannedAction::Reinstall,
        Ordering::Equal => PlannedAction::SkipUpToDate,
        Ordering::Greater if force => PlannedAction::Reinstall,
        Ordering::Greater => PlannedAction::SkipNewerCurrent,
    }
}

fn parse_current_version(version: &str) -> CliResult<Version> {
    Version::parse(version).map_err(|source| CliError::SelfUpdateFailed {
        message: format!("current fontbrew version {version:?} is not valid semver: {source}"),
    })
}

fn parse_latest_release_version(version: &str) -> CliResult<Version> {
    let normalized = version.strip_prefix('v').unwrap_or(version);
    let parsed =
        Version::parse(normalized).map_err(|source| CliError::SelfUpdateInvalidRelease {
            message: format!("latest release tag {version:?} is not valid semver: {source}"),
        })?;
    if !parsed.pre.is_empty() {
        return Err(CliError::SelfUpdateInvalidRelease {
            message: format!("latest release tag {version:?} is a prerelease"),
        });
    }

    Ok(parsed)
}

fn current_target() -> CliResult<&'static str> {
    match (env::consts::OS, env::consts::ARCH) {
        ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
        ("macos", "x86_64") => Ok("x86_64-apple-darwin"),
        _ => Err(CliError::SelfUpdateUnavailable {
            message: format!(
                "fontbrew self-update does not support {}-{}",
                env::consts::ARCH,
                env::consts::OS
            ),
        }),
    }
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

    if let Ok(token) = env::var(GITHUB_TOKEN_ENV_VAR) {
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

fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME").map(PathBuf::from)
}

fn create_staging_dir_for(current_executable: &Path) -> CliResult<PathBuf> {
    let parent = current_executable
        .parent()
        .ok_or_else(|| CliError::SelfUpdateFailed {
            message: format!(
                "current executable has no parent directory: {}",
                current_executable.display()
            ),
        })?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|source| CliError::SelfUpdateFailed {
            message: format!("system clock is before Unix epoch: {source}"),
        })?
        .as_nanos();

    for attempt in 0..100_u8 {
        let staging_dir = parent.join(format!(
            ".fontbrew-update-{}-{timestamp}-{attempt}",
            std::process::id()
        ));
        match fs::create_dir(&staging_dir) {
            Ok(()) => {
                fs::set_permissions(&staging_dir, fs::Permissions::from_mode(0o700))?;
                return Ok(staging_dir);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(CliError::SelfUpdateFailed {
                    message: format!(
                        "could not create staging directory {}: {error}",
                        staging_dir.display()
                    ),
                });
            }
        }
    }

    Err(CliError::SelfUpdateFailed {
        message: format!(
            "could not create a unique staging directory beside {}",
            current_executable.display()
        ),
    })
}

fn set_executable(path: &Path) -> CliResult<()> {
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

fn sync_directory(path: &Path) -> CliResult<()> {
    fs::File::open(path)?.sync_all()?;
    Ok(())
}

fn ensure_not_cancelled(cancellation: &dyn CancellationToken) -> CliResult<()> {
    if cancellation.is_cancelled() {
        return Err(CliError::Cancelled);
    }

    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReplacementFailurePoint {
    BeforeBackup,
    AfterBackup,
    AfterInstall,
}

#[cfg(test)]
fn inject_replacement_failure(point: ReplacementFailurePoint) -> CliResult<()> {
    let mut failure = replacement_failure()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if *failure == Some(point) {
        *failure = None;
        return Err(CliError::SelfUpdateFailed {
            message: format!("forced replacement failure at {point:?}"),
        });
    }

    Ok(())
}

#[cfg(not(test))]
fn inject_replacement_failure(_point: ReplacementFailurePoint) -> CliResult<()> {
    Ok(())
}

#[cfg(test)]
fn debug_fail_next_replacement(point: ReplacementFailurePoint) {
    let mut failure = replacement_failure()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *failure = Some(point);
}

#[cfg(test)]
fn replacement_failure() -> &'static std::sync::Mutex<Option<ReplacementFailurePoint>> {
    static FAILURE: std::sync::OnceLock<std::sync::Mutex<Option<ReplacementFailurePoint>>> =
        std::sync::OnceLock::new();
    FAILURE.get_or_init(|| std::sync::Mutex::new(None))
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        fs::File,
        io::{BufRead, BufReader, Write},
        net::{Shutdown, TcpListener},
        sync::{Arc, Mutex, OnceLock},
        thread,
    };

    use fontbrew_core::fetch::NetworkClient;
    use tempfile::TempDir;

    use super::*;
    use crate::confirm::{ConfirmationOptions, Confirmer};

    struct NeverCancelled;

    impl CancellationToken for NeverCancelled {
        fn is_cancelled(&self) -> bool {
            false
        }
    }

    struct CancelWhenPreparedBinaryExists {
        root: PathBuf,
    }

    impl CancellationToken for CancelWhenPreparedBinaryExists {
        fn is_cancelled(&self) -> bool {
            fs::read_dir(&self.root).is_ok_and(|entries| {
                entries.filter_map(Result::ok).any(|entry| {
                    let path = entry.path();
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.starts_with(".fontbrew-update-"))
                        && path.join("fontbrew.new").exists()
                })
            })
        }
    }

    struct TestHttpServer {
        base_url: String,
        routes: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
        requests: Arc<Mutex<Vec<String>>>,
    }

    impl TestHttpServer {
        fn start() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind self-update test server");
            let base_url = format!(
                "http://{}",
                listener
                    .local_addr()
                    .expect("self-update test server address")
            );
            let routes = Arc::new(Mutex::new(BTreeMap::<String, Vec<u8>>::new()));
            let requests = Arc::new(Mutex::new(Vec::<String>::new()));
            let server_routes = routes.clone();
            let server_requests = requests.clone();
            thread::spawn(move || {
                for stream in listener.incoming() {
                    let mut stream = stream.expect("accept self-update request");
                    let routes = server_routes.clone();
                    let requests = server_requests.clone();
                    thread::spawn(move || {
                        let mut reader =
                            BufReader::new(stream.try_clone().expect("clone request stream"));
                        let mut request_line = String::new();
                        reader.read_line(&mut request_line).expect("read request");
                        let path = request_line
                            .split_whitespace()
                            .nth(1)
                            .unwrap_or("/")
                            .to_string();
                        loop {
                            let mut line = String::new();
                            let read = reader.read_line(&mut line).expect("read request header");
                            if read == 0 || line == "\r\n" {
                                break;
                            }
                        }
                        requests.lock().expect("requests lock").push(request_line);
                        let body = routes
                            .lock()
                            .expect("routes lock")
                            .get(&path)
                            .cloned()
                            .unwrap_or_else(|| panic!("no test response for {path}"));
                        write!(
                            stream,
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        )
                        .expect("write response headers");
                        stream.write_all(&body).expect("write response body");
                        stream.flush().expect("flush response");
                        stream.shutdown(Shutdown::Write).expect("shutdown response");
                    });
                }
            });

            Self {
                base_url,
                routes,
                requests,
            }
        }

        fn url(&self, path: &str) -> String {
            format!("{}{}", self.base_url, path)
        }

        fn respond_text(&self, path: &str, body: impl Into<String>) {
            self.respond_bytes(path, body.into().into_bytes());
        }

        fn respond_bytes(&self, path: &str, bytes: Vec<u8>) {
            self.routes
                .lock()
                .expect("routes lock")
                .insert(path.to_string(), bytes);
        }

        fn network_client(&self) -> NetworkClient {
            NetworkClient::new().expect("network client")
        }

        fn request_lines(&self) -> Vec<String> {
            self.requests.lock().expect("requests lock").clone()
        }
    }

    #[test]
    fn install_method_detection_rejects_managed_and_development_paths() {
        let home = PathBuf::from("/Users/example");
        for (path, expected) in [
            ("/opt/homebrew/bin/fontbrew", "Homebrew"),
            ("/usr/local/Cellar/fontbrew/0.1.0/bin/fontbrew", "Homebrew"),
            ("/opt/local/bin/fontbrew", "MacPorts"),
            ("/Users/example/.cargo/bin/fontbrew", "Cargo"),
            ("/tmp/fontbrew/target/debug/fontbrew", "development build"),
            ("/tmp/fontbrew/target/release/fontbrew", "development build"),
        ] {
            let error = detect_install_method(Path::new(path), Some(&home))
                .expect_err("managed path should be rejected");

            assert_eq!(error.kind(), "self_update_unavailable");
            assert!(error.message().contains(expected), "{path}");
        }
    }

    #[test]
    fn install_method_detection_allows_writable_standalone_binary() {
        let temp = tempfile::tempdir().expect("tempdir");
        let bin_dir = temp.path().join("bin");
        fs::create_dir_all(&bin_dir).expect("create bin dir");
        let executable = bin_dir.join("fontbrew");
        fs::write(&executable, b"#!/bin/sh\nexit 0\n").expect("write executable");
        set_executable(&executable).expect("chmod executable");

        let method = detect_install_method(&executable, Some(temp.path()))
            .expect("standalone path should be allowed");

        assert_eq!(method, SelfUpdateInstallMethod::Standalone);
    }

    #[test]
    fn version_rules_select_expected_action() {
        let current = Version::parse("0.1.1").unwrap();
        let latest = Version::parse("0.1.2").unwrap();
        assert_eq!(
            planned_action(&current, &latest, false),
            PlannedAction::Update
        );

        let current = Version::parse("0.1.2").unwrap();
        assert_eq!(
            planned_action(&current, &latest, false),
            PlannedAction::SkipUpToDate
        );
        assert_eq!(
            planned_action(&current, &latest, true),
            PlannedAction::Reinstall
        );

        let current = Version::parse("0.1.3").unwrap();
        assert_eq!(
            planned_action(&current, &latest, false),
            PlannedAction::SkipNewerCurrent
        );
        assert_eq!(
            planned_action(&current, &latest, true),
            PlannedAction::Reinstall
        );
    }

    #[test]
    fn latest_release_version_requires_stable_semver() {
        assert_eq!(
            parse_latest_release_version("v0.1.2").expect("version"),
            Version::parse("0.1.2").unwrap()
        );
        assert!(matches!(
            parse_latest_release_version("latest"),
            Err(CliError::SelfUpdateInvalidRelease { .. })
        ));
        assert!(matches!(
            parse_latest_release_version("v1.0.0-beta.1"),
            Err(CliError::SelfUpdateInvalidRelease { .. })
        ));
    }

    #[test]
    fn checksum_parser_accepts_typical_shasum_line_and_rejects_bad_input() {
        let expected = "fontbrew-aarch64-apple-darwin.tar.gz";
        let hash = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

        assert!(parse_sha256_checksum(&format!("{hash}  {expected}\n"), expected).is_ok());

        assert!(matches!(
            parse_sha256_checksum(&format!("{hash}  other.tar.gz\n"), expected),
            Err(CliError::SelfUpdateInvalidRelease { .. })
        ));
        assert!(matches!(
            parse_sha256_checksum(&format!("bad  {expected}\n"), expected),
            Err(CliError::SelfUpdateInvalidRelease { .. })
        ));
    }

    #[test]
    fn checksum_verification_rejects_mismatch() {
        let temp = tempfile::tempdir().expect("tempdir");
        let archive = temp.path().join("fontbrew-aarch64-apple-darwin.tar.gz");
        let checksum = temp
            .path()
            .join("fontbrew-aarch64-apple-darwin.tar.gz.sha256");
        fs::write(&archive, b"archive").expect("write archive");
        fs::write(
            &checksum,
            "0000000000000000000000000000000000000000000000000000000000000000  fontbrew-aarch64-apple-darwin.tar.gz\n",
        )
        .expect("write checksum");

        let error =
            verify_checksum_file(&archive, &checksum, "fontbrew-aarch64-apple-darwin.tar.gz")
                .expect_err("checksum should mismatch");

        assert_eq!(error.kind(), "self_update_checksum_mismatch");
    }

    #[tokio::test]
    async fn release_asset_contract_selects_target_archive_and_checksum() {
        let server = TestHttpServer::start();
        server.respond_text(
            "/repos/yyyanghj/fontbrew/releases",
            r#"[
              {"tag_name":"v0.1.2","draft":false,"prerelease":false,"assets":[
                {"name":"fontbrew-aarch64-apple-darwin.tar.gz","browser_download_url":"https://downloads.example/archive"},
                {"name":"fontbrew-aarch64-apple-darwin.tar.gz.sha256","browser_download_url":"https://downloads.example/checksum"}
              ]}
            ]"#,
        );
        let client = server.network_client();

        let release = resolve_latest_release(
            &client,
            "yyyanghj/fontbrew",
            &server.base_url,
            "aarch64-apple-darwin",
        )
        .await
        .expect("resolve release");

        assert_eq!(release.latest_version, Version::parse("0.1.2").unwrap());
        assert_eq!(
            release.archive_asset.name,
            "fontbrew-aarch64-apple-darwin.tar.gz"
        );
        assert_eq!(
            release.checksum_asset.name,
            "fontbrew-aarch64-apple-darwin.tar.gz.sha256"
        );
        assert!(
            server.request_lines()[0].starts_with("GET /repos/yyyanghj/fontbrew/releases HTTP/1.1")
        );
    }

    #[tokio::test]
    async fn release_asset_contract_rejects_missing_target_asset() {
        let server = TestHttpServer::start();
        server.respond_text(
            "/repos/yyyanghj/fontbrew/releases",
            r#"[
              {"tag_name":"v0.1.2","draft":false,"prerelease":false,"assets":[]}
            ]"#,
        );
        let client = server.network_client();

        let error = resolve_latest_release(
            &client,
            "yyyanghj/fontbrew",
            &server.base_url,
            "aarch64-apple-darwin",
        )
        .await
        .expect_err("missing asset should fail");

        assert_eq!(error.kind(), "self_update_invalid_release");
        assert!(error.message().contains("missing required asset"));
    }

    #[test]
    fn archive_extraction_rejects_missing_expected_binary() {
        let temp = tempfile::tempdir().expect("tempdir");
        let archive = temp.path().join("archive.tar.gz");
        write_tar_gz(&archive, &[("wrong/fontbrew", b"binary".as_slice())]);

        let error = extract_expected_binary(
            &archive,
            "aarch64-apple-darwin",
            &temp.path().join("fontbrew.new"),
        )
        .expect_err("missing expected entry should fail");

        assert_eq!(error.kind(), "self_update_invalid_release");
    }

    #[test]
    fn archive_extraction_refuses_existing_destination() {
        let temp = tempfile::tempdir().expect("tempdir");
        let archive = temp.path().join("archive.tar.gz");
        let destination = temp.path().join("fontbrew.new");
        write_tar_gz(
            &archive,
            &[(
                "fontbrew-aarch64-apple-darwin/fontbrew",
                b"binary".as_slice(),
            )],
        );
        fs::write(&destination, b"existing").expect("write existing destination");

        let error = extract_expected_binary(&archive, "aarch64-apple-darwin", &destination)
            .expect_err("existing destination should fail");

        assert_eq!(error.kind(), "io");
        assert_eq!(
            fs::read(&destination).expect("existing destination remains"),
            b"existing"
        );
    }

    #[test]
    fn staging_dir_is_created_as_private_unique_directory() {
        let temp = tempfile::tempdir().expect("tempdir");
        let current = write_script(&temp, "fontbrew", "old");

        let first = create_staging_dir_for(&current).expect("create first staging dir");
        let second = create_staging_dir_for(&current).expect("create second staging dir");

        assert_ne!(first, second);
        assert!(first
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with(".fontbrew-update-"));
        assert_eq!(
            fs::metadata(&first).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&second).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[test]
    fn replacement_flow_replaces_binary_and_removes_backup() {
        let _guard = replacement_test_guard();
        let temp = tempfile::tempdir().expect("tempdir");
        let current = write_script(&temp, "fontbrew", "old");
        let new_binary = write_script(&temp, "fontbrew.new", "new");

        replace_executable(&current, &new_binary).expect("replace executable");

        let output = Command::new(&current)
            .arg("--version")
            .output()
            .expect("run replaced executable");
        assert!(String::from_utf8_lossy(&output.stdout).contains("new"));
        assert!(!new_binary.exists());
        assert_no_backup(temp.path());
    }

    #[test]
    fn replacement_failure_before_backup_leaves_current_untouched() {
        let _guard = replacement_test_guard();
        let temp = tempfile::tempdir().expect("tempdir");
        let current = write_script(&temp, "fontbrew", "old");
        let new_binary = write_script(&temp, "fontbrew.new", "new");
        debug_fail_next_replacement(ReplacementFailurePoint::BeforeBackup);

        let error = replace_executable(&current, &new_binary)
            .expect_err("forced pre-backup failure should fail");

        assert_eq!(error.kind(), "self_update_failed");
        let output = Command::new(&current)
            .arg("--version")
            .output()
            .expect("run original executable");
        assert!(String::from_utf8_lossy(&output.stdout).contains("old"));
        assert!(new_binary.exists());
        assert_no_backup(temp.path());
    }

    #[test]
    fn replacement_failure_after_backup_restores_original() {
        let _guard = replacement_test_guard();
        let temp = tempfile::tempdir().expect("tempdir");
        let current = write_script(&temp, "fontbrew", "old");
        let new_binary = write_script(&temp, "fontbrew.new", "new");
        debug_fail_next_replacement(ReplacementFailurePoint::AfterBackup);

        let error = replace_executable(&current, &new_binary)
            .expect_err("forced replacement failure should fail");

        assert_eq!(error.kind(), "self_update_failed");
        let output = Command::new(&current)
            .arg("--version")
            .output()
            .expect("run restored executable");
        assert!(String::from_utf8_lossy(&output.stdout).contains("old"));
    }

    #[test]
    fn post_replacement_smoke_failure_restores_original() {
        let _guard = replacement_test_guard();
        let temp = tempfile::tempdir().expect("tempdir");
        let current = write_script(&temp, "fontbrew", "old");
        let new_binary = temp.path().join("fontbrew.new");
        fs::write(&new_binary, b"not executable").expect("write invalid new binary");
        set_executable(&new_binary).expect("chmod invalid new binary");

        let error =
            replace_executable(&current, &new_binary).expect_err("smoke failure should fail");

        assert_eq!(error.kind(), "self_update_failed");
        let output = Command::new(&current)
            .arg("--version")
            .output()
            .expect("run restored executable");
        assert!(String::from_utf8_lossy(&output.stdout).contains("old"));
    }

    #[tokio::test]
    async fn prepare_release_downloads_verifies_extracts_and_replaces() {
        let _guard = replacement_test_guard_async().await;
        let temp = tempfile::tempdir().expect("tempdir");
        let current = write_script(&temp, "fontbrew", "old");
        let staging = create_staging_dir_for(&current).expect("create staging dir");
        let archive_bytes = archive_with_fontbrew_binary("aarch64-apple-darwin");
        let checksum = sha256_bytes(&archive_bytes);
        let server = TestHttpServer::start();
        server.respond_bytes("/archive", archive_bytes);
        server.respond_bytes(
            "/checksum",
            format!("{checksum}  fontbrew-aarch64-apple-darwin.tar.gz\n").into_bytes(),
        );
        let release = ReleaseSelection {
            latest_version: Version::parse("0.1.2").unwrap(),
            archive_asset: GitHubReleaseAsset {
                name: "fontbrew-aarch64-apple-darwin.tar.gz".to_string(),
                browser_download_url: server.url("/archive"),
            },
            checksum_asset: GitHubReleaseAsset {
                name: "fontbrew-aarch64-apple-darwin.tar.gz.sha256".to_string(),
                browser_download_url: server.url("/checksum"),
            },
        };
        let mut reporter = TestReporter::default();

        let prepared = prepare_release(
            &server.network_client(),
            &release,
            "aarch64-apple-darwin",
            &staging,
            &mut reporter,
            Arc::new(NeverCancelled),
        )
        .await
        .expect("install release");
        replace_executable(&current, &prepared.new_binary_path).expect("replace executable");

        let output = Command::new(&current)
            .arg("--version")
            .output()
            .expect("run installed executable");
        assert!(String::from_utf8_lossy(&output.stdout).contains("0.1.2"));
        assert!(reporter
            .progress
            .iter()
            .any(|line| line.contains("Verifying checksum")));
    }

    #[tokio::test]
    async fn run_dry_run_reports_planned_update_without_prompting_or_downloading() {
        let temp = tempfile::tempdir().expect("tempdir");
        let current = write_script(&temp, "fontbrew", "0.1.1");
        let server = TestHttpServer::start();
        respond_with_release(&server, "v0.1.2");
        let mut reporter = TestReporter::default();
        let mut confirmer = TestConfirmer::default();

        run_with_network_client_for_target(
            SelfUpdateRequest {
                dry_run: true,
                assume_yes: false,
                force: false,
                current_executable: current.clone(),
                current_version: "0.1.1".to_string(),
                lock_path: temp.path().join("self-update.lock"),
                repo: DEFAULT_FONTBREW_REPO.to_string(),
            },
            &server.network_client(),
            &server.base_url,
            &mut reporter,
            &mut confirmer,
            Arc::new(NeverCancelled),
            "aarch64-apple-darwin",
        )
        .await
        .expect("dry-run self-update");

        assert!(!confirmer.prompted);
        assert_eq!(reporter.self_update_reports.len(), 1);
        let report = &reporter.self_update_reports[0];
        assert_eq!(report.status, SelfUpdateStatus::Planned);
        assert_eq!(report.current_version, "0.1.1");
        assert_eq!(report.latest_version, "0.1.2");
        assert_eq!(report.executable_path, current);
    }

    #[tokio::test]
    async fn run_dry_run_fetches_release_without_taking_self_update_lock() {
        let temp = tempfile::tempdir().expect("tempdir");
        let current = write_script(&temp, "fontbrew", "0.1.1");
        let server = TestHttpServer::start();
        respond_with_release(&server, "v0.1.2");
        let lock_path = temp.path().join("self-update.lock");
        let _held_lock = GlobalFileLock::try_exclusive(&lock_path).expect("hold self-update lock");
        let mut reporter = TestReporter::default();
        let mut confirmer = TestConfirmer::default();

        run_with_network_client_for_target(
            SelfUpdateRequest {
                dry_run: true,
                assume_yes: false,
                force: false,
                current_executable: current,
                current_version: "0.1.1".to_string(),
                lock_path,
                repo: DEFAULT_FONTBREW_REPO.to_string(),
            },
            &server.network_client(),
            &server.base_url,
            &mut reporter,
            &mut confirmer,
            Arc::new(NeverCancelled),
            "aarch64-apple-darwin",
        )
        .await
        .expect("dry-run self-update should not take replacement lock");

        assert_eq!(
            reporter.self_update_reports[0].status,
            SelfUpdateStatus::Planned
        );
        assert_eq!(server.request_lines().len(), 1);
    }

    #[tokio::test]
    async fn cancellation_after_blocking_prepare_prevents_locked_replace_and_cleans_staging() {
        let _guard = replacement_test_guard_async().await;
        let temp = tempfile::tempdir().expect("tempdir");
        let current = write_script(&temp, "fontbrew", "0.1.1");
        let archive_bytes = archive_with_fontbrew_binary("aarch64-apple-darwin");
        let checksum = sha256_bytes(&archive_bytes);
        let server = TestHttpServer::start();
        respond_with_release(&server, "v0.1.2");
        server.respond_bytes("/archive", archive_bytes);
        server.respond_bytes(
            "/checksum",
            format!("{checksum}  fontbrew-aarch64-apple-darwin.tar.gz\n").into_bytes(),
        );
        let mut reporter = TestReporter::default();
        let mut confirmer = TestConfirmer::default();

        let error = run_with_network_client_for_target(
            SelfUpdateRequest {
                dry_run: false,
                assume_yes: true,
                force: false,
                current_executable: current.clone(),
                current_version: "0.1.1".to_string(),
                lock_path: temp.path().join("self-update.lock"),
                repo: DEFAULT_FONTBREW_REPO.to_string(),
            },
            &server.network_client(),
            &server.base_url,
            &mut reporter,
            &mut confirmer,
            Arc::new(CancelWhenPreparedBinaryExists {
                root: temp.path().to_path_buf(),
            }),
            "aarch64-apple-darwin",
        )
        .await
        .expect_err("post-prepare cancellation should stop replacement");

        assert_eq!(error.kind(), "cancelled");
        let output = Command::new(&current)
            .arg("--version")
            .output()
            .expect("run current executable");
        assert!(String::from_utf8_lossy(&output.stdout).contains("0.1.1"));
        assert!(fs::read_dir(temp.path())
            .expect("read temp dir")
            .all(|entry| !entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .starts_with(".fontbrew-update-")));
    }

    #[tokio::test]
    async fn locked_replace_revalidates_current_version_before_replacing() {
        let _guard = replacement_test_guard_async().await;
        let temp = tempfile::tempdir().expect("tempdir");
        let current = write_script(&temp, "fontbrew", "0.1.2");
        let new_binary = write_script(&temp, "fontbrew.new", "0.1.3");

        let outcome = replace_prepared_release(
            &SelfUpdateRequest {
                dry_run: false,
                assume_yes: true,
                force: false,
                current_executable: current.clone(),
                current_version: "0.1.1".to_string(),
                lock_path: temp.path().join("self-update.lock"),
                repo: DEFAULT_FONTBREW_REPO.to_string(),
            },
            &Version::parse("0.1.2").unwrap(),
            &PreparedSelfUpdate {
                new_binary_path: new_binary.clone(),
            },
        )
        .await
        .expect("locked replace revalidates current version");

        assert_eq!(outcome.status, SelfUpdateStatus::UpToDate);
        let output = Command::new(&current)
            .arg("--version")
            .output()
            .expect("run current executable");
        assert!(String::from_utf8_lossy(&output.stdout).contains("0.1.2"));
        assert!(new_binary.exists());
    }

    #[tokio::test]
    async fn run_up_to_date_reports_without_prompting_or_downloading() {
        let temp = tempfile::tempdir().expect("tempdir");
        let current = write_script(&temp, "fontbrew", "0.1.2");
        let server = TestHttpServer::start();
        respond_with_release(&server, "v0.1.2");
        let mut reporter = TestReporter::default();
        let mut confirmer = TestConfirmer::default();

        run_with_network_client_for_target(
            SelfUpdateRequest {
                dry_run: false,
                assume_yes: false,
                force: false,
                current_executable: current,
                current_version: "0.1.2".to_string(),
                lock_path: temp.path().join("self-update.lock"),
                repo: DEFAULT_FONTBREW_REPO.to_string(),
            },
            &server.network_client(),
            &server.base_url,
            &mut reporter,
            &mut confirmer,
            Arc::new(NeverCancelled),
            "aarch64-apple-darwin",
        )
        .await
        .expect("up-to-date self-update");

        assert!(!confirmer.prompted);
        assert_eq!(reporter.self_update_reports.len(), 1);
        assert_eq!(
            reporter.self_update_reports[0].status,
            SelfUpdateStatus::UpToDate
        );
    }

    #[derive(Default)]
    struct TestReporter {
        progress: Vec<String>,
        self_update_reports: Vec<SelfUpdateReport>,
    }

    impl Reporter for TestReporter {
        fn render_install_report(
            &mut self,
            _report: fontbrew_core::InstallReport,
        ) -> CliResult<()> {
            Ok(())
        }
        fn render_list_report(&mut self, _report: fontbrew_core::ListReport) -> CliResult<()> {
            Ok(())
        }
        fn render_info_report(&mut self, _report: fontbrew_core::InfoReport) -> CliResult<()> {
            Ok(())
        }
        fn render_remove_report(&mut self, _report: fontbrew_core::RemoveReport) -> CliResult<()> {
            Ok(())
        }
        fn render_search_report(&mut self, _report: fontbrew_core::SearchReport) -> CliResult<()> {
            Ok(())
        }
        fn render_outdated_report(
            &mut self,
            _report: fontbrew_core::OutdatedReport,
        ) -> CliResult<()> {
            Ok(())
        }
        fn render_update_report(&mut self, _report: fontbrew_core::UpdateReport) -> CliResult<()> {
            Ok(())
        }
        fn render_config_get_report(
            &mut self,
            _report: fontbrew_core::ConfigReport,
        ) -> CliResult<()> {
            Ok(())
        }
        fn render_config_set_report(
            &mut self,
            _report: fontbrew_core::ConfigReport,
        ) -> CliResult<()> {
            Ok(())
        }
        fn render_self_update_report(&mut self, report: SelfUpdateReport) -> CliResult<()> {
            self.self_update_reports.push(report);
            Ok(())
        }
        fn render_error(&mut self, _error: &CliError) -> CliResult<()> {
            Ok(())
        }
        fn warn(&mut self, _warning: &str) -> CliResult<()> {
            Ok(())
        }
        fn progress(&mut self, _event: &fontbrew_core::ProgressEvent) -> CliResult<()> {
            Ok(())
        }
        fn self_update_progress(&mut self, message: &str) -> CliResult<()> {
            self.progress.push(message.to_string());
            Ok(())
        }
    }

    #[derive(Default)]
    struct TestConfirmer {
        prompted: bool,
    }

    impl Confirmer for TestConfirmer {
        fn execution_policy(
            &mut self,
            _risks: &[fontbrew_core::PlanRisk],
            _options: ConfirmationOptions,
        ) -> CliResult<fontbrew_core::ExecutionPolicy> {
            Ok(fontbrew_core::ExecutionPolicy::SafeOnly)
        }

        fn confirm_self_update(
            &mut self,
            _executable_path: &Path,
            _target_version: &str,
            _assume_yes: bool,
        ) -> CliResult<()> {
            self.prompted = true;
            Ok(())
        }

        fn select_families(
            &mut self,
            _families: &[fontbrew_core::FamilyName],
        ) -> CliResult<Vec<fontbrew_core::FamilyName>> {
            unreachable!("self-update does not select font families")
        }
    }

    fn write_script(temp: &TempDir, name: &str, version: &str) -> PathBuf {
        let path = temp.path().join(name);
        fs::write(
            &path,
            format!("#!/bin/sh\nprintf 'fontbrew {version}\\n'\n"),
        )
        .expect("write script");
        set_executable(&path).expect("chmod script");
        path
    }

    fn write_tar_gz(path: &Path, entries: &[(&str, &[u8])]) {
        let file = File::create(path).expect("create tar.gz");
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);
        for (name, bytes) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder
                .append_data(&mut header, *name, *bytes)
                .expect("append tar entry");
        }
        builder.finish().expect("finish tar");
    }

    fn archive_with_fontbrew_binary(target: &str) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let encoder = flate2::write::GzEncoder::new(&mut bytes, flate2::Compression::default());
            let mut builder = tar::Builder::new(encoder);
            let script = b"#!/bin/sh\nprintf 'fontbrew 0.1.2\\n'\n";
            let mut header = tar::Header::new_gnu();
            header.set_size(script.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder
                .append_data(
                    &mut header,
                    format!("fontbrew-{target}/fontbrew"),
                    script.as_slice(),
                )
                .expect("append binary");
            builder.finish().expect("finish tar");
        }
        bytes
    }

    fn respond_with_release(server: &TestHttpServer, tag_name: &str) {
        server.respond_text(
            "/repos/yyyanghj/fontbrew/releases",
            format!(
                r#"[
                  {{"tag_name":"{tag_name}","draft":false,"prerelease":false,"assets":[
                    {{"name":"fontbrew-aarch64-apple-darwin.tar.gz","browser_download_url":"{}"}},
                    {{"name":"fontbrew-aarch64-apple-darwin.tar.gz.sha256","browser_download_url":"{}"}},
                    {{"name":"fontbrew-x86_64-apple-darwin.tar.gz","browser_download_url":"{}"}},
                    {{"name":"fontbrew-x86_64-apple-darwin.tar.gz.sha256","browser_download_url":"{}"}}
                  ]}}
                ]"#,
                server.url("/archive"),
                server.url("/checksum"),
                server.url("/archive-x86_64"),
                server.url("/checksum-x86_64"),
            ),
        );
    }

    fn sha256_bytes(bytes: &[u8]) -> String {
        let digest = Sha256::digest(bytes);
        digest.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    fn assert_no_backup(path: &Path) {
        let has_backup = fs::read_dir(path).expect("read temp dir").any(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .contains(".old-")
        });
        assert!(!has_backup);
    }

    fn replacement_test_guard() -> tokio::sync::MutexGuard<'static, ()> {
        replacement_test_lock().blocking_lock()
    }

    async fn replacement_test_guard_async() -> tokio::sync::MutexGuard<'static, ()> {
        replacement_test_lock().lock().await
    }

    fn replacement_test_lock() -> &'static tokio::sync::Mutex<()> {
        static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
    }
}
