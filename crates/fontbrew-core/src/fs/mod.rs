use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use fs2::FileExt;
use tempfile::NamedTempFile;

use crate::error::{FontbrewError, Result};

pub fn write_atomically(path: &Path, content: &[u8]) -> Result<()> {
    write_atomically_with_commit_status(path, content).map_err(AtomicWriteError::into_error)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtomicWriteCommitStatus {
    NotCommitted,
    Uncertain,
}

#[derive(Debug)]
pub struct AtomicWriteError {
    pub commit_status: AtomicWriteCommitStatus,
    pub error: FontbrewError,
}

impl AtomicWriteError {
    pub fn into_error(self) -> FontbrewError {
        self.error
    }
}

pub fn write_atomically_with_commit_status(
    path: &Path,
    content: &[u8],
) -> std::result::Result<(), AtomicWriteError> {
    let parent = path
        .parent()
        .ok_or_else(|| FontbrewError::PathResolution {
            message: format!("target path has no parent: {path:?}"),
        })
        .map_err(not_committed)?;

    fs::create_dir_all(parent).map_err(|error| not_committed(FontbrewError::from(error)))?;

    let mut temp_file =
        NamedTempFile::new_in(parent).map_err(|error| not_committed(FontbrewError::from(error)))?;
    temp_file
        .write_all(content)
        .map_err(|error| not_committed(FontbrewError::from(error)))?;
    temp_file
        .flush()
        .map_err(|error| not_committed(FontbrewError::from(error)))?;
    temp_file
        .as_file()
        .sync_all()
        .map_err(|error| not_committed(FontbrewError::from(error)))?;

    if let Some(error) =
        take_debug_atomic_write_failure(path, DebugAtomicWriteFailure::BeforePersist)
    {
        return Err(not_committed(FontbrewError::from(error)));
    }

    temp_file
        .persist(path)
        .map_err(|error| not_committed(FontbrewError::from(error.error)))?;

    if let Some(error) =
        take_debug_atomic_write_failure(path, DebugAtomicWriteFailure::AfterPersist)
    {
        return Err(uncertain(FontbrewError::from(error)));
    }

    sync_directory(parent).map_err(uncertain)?;

    Ok(())
}

fn not_committed(error: FontbrewError) -> AtomicWriteError {
    AtomicWriteError {
        commit_status: AtomicWriteCommitStatus::NotCommitted,
        error,
    }
}

fn uncertain(error: FontbrewError) -> AtomicWriteError {
    AtomicWriteError {
        commit_status: AtomicWriteCommitStatus::Uncertain,
        error,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DebugAtomicWriteFailure {
    BeforePersist,
    AfterPersist,
}

#[cfg(debug_assertions)]
#[doc(hidden)]
pub fn debug_fail_next_atomic_write(path: &Path, failure: DebugAtomicWriteFailure) {
    let mut failures = debug_atomic_write_failures()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    failures.insert(path.to_path_buf(), failure);
}

#[cfg(debug_assertions)]
fn take_debug_atomic_write_failure(
    path: &Path,
    failure: DebugAtomicWriteFailure,
) -> Option<std::io::Error> {
    let mut failures = debug_atomic_write_failures()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    if failures.get(path).copied() != Some(failure) {
        return None;
    }

    failures.remove(path);
    Some(std::io::Error::other(format!(
        "forced atomic write failure at {failure:?} for {}",
        path.display()
    )))
}

#[cfg(not(debug_assertions))]
fn take_debug_atomic_write_failure(
    path: &Path,
    failure: DebugAtomicWriteFailure,
) -> Option<std::io::Error> {
    let _ = (path, failure);
    None
}

#[cfg(debug_assertions)]
static DEBUG_ATOMIC_WRITE_FAILURES: OnceLock<Mutex<BTreeMap<PathBuf, DebugAtomicWriteFailure>>> =
    OnceLock::new();

#[cfg(debug_assertions)]
fn debug_atomic_write_failures() -> &'static Mutex<BTreeMap<PathBuf, DebugAtomicWriteFailure>> {
    DEBUG_ATOMIC_WRITE_FAILURES.get_or_init(|| Mutex::new(BTreeMap::new()))
}

pub fn ensure_existing_path_does_not_cross_symlink(root: &Path, target: &Path) -> Result<()> {
    let relative_path = target
        .strip_prefix(root)
        .map_err(|_| FontbrewError::PathResolution {
            message: format!(
                "path must stay under {}: {}",
                root.display(),
                target.display()
            ),
        })?;

    reject_existing_symlink(root)?;

    let mut current_path = root.to_path_buf();
    for component in relative_path.components() {
        match component {
            Component::Normal(name) => {
                current_path.push(name);
                reject_existing_symlink(&current_path)?;
            }
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => {
                return Err(FontbrewError::PathResolution {
                    message: format!(
                        "path contains an unsafe component under {}: {}",
                        root.display(),
                        target.display()
                    ),
                });
            }
        }
    }

    Ok(())
}

fn reject_existing_symlink(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(FontbrewError::PathResolution {
            message: format!("path must not cross a symlink: {}", path.display()),
        }),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[derive(Debug)]
pub struct GlobalFileLock {
    path: PathBuf,
    file: File,
}

impl GlobalFileLock {
    pub fn try_exclusive(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;

        file.try_lock_exclusive()
            .map_err(|source| FontbrewError::Lock {
                path: path.to_path_buf(),
                source,
            })?;

        Ok(Self {
            path: path.to_path_buf(),
            file,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for GlobalFileLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}
