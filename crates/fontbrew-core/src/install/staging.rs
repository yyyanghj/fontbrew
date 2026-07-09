use std::{
    fs,
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{
    error::{FontbrewError, Result},
    fs::ensure_existing_path_does_not_cross_symlink,
    platform::FontbrewPaths,
};

const ACTIVE_STAGING_MARKER: &str = ".fontbrew-active";
const ACTIVE_STAGING_LEASE_SECS: u64 = 7 * 24 * 60 * 60;
static OPERATION_COUNTER: AtomicU64 = AtomicU64::new(0);

pub(super) struct StagingCleanupGuard {
    path: PathBuf,
    cleanup_on_drop: bool,
}

impl StagingCleanupGuard {
    pub(super) fn new(path: PathBuf) -> Self {
        Self {
            path,
            cleanup_on_drop: true,
        }
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    pub(super) fn disarm(&mut self) {
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

pub(super) fn create_active_staging_dir(paths: &FontbrewPaths) -> Result<PathBuf> {
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

pub(crate) fn cleanup_staging(path: &Path) {
    let _ = fs::remove_dir_all(path);
}

pub(super) fn ensure_path_inside(parent: &Path, child: &Path) -> Result<()> {
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

fn new_staging_dir(paths: &FontbrewPaths) -> Result<PathBuf> {
    Ok(paths
        .staging_dir()
        .join(format!("install-{}", operation_suffix()?)))
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

pub(super) fn operation_suffix() -> Result<String> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| FontbrewError::PathResolution {
            message: format!("system clock is before unix epoch: {error}"),
        })?
        .as_nanos();
    let counter = OPERATION_COUNTER.fetch_add(1, Ordering::Relaxed);

    Ok(format!("{timestamp}-{counter}"))
}
