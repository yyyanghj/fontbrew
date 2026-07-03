use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use fs2::FileExt;
use tempfile::NamedTempFile;

use crate::error::{FontbrewError, Result};

pub fn write_atomically(path: &Path, content: &[u8]) -> Result<()> {
    let parent = path.parent().ok_or_else(|| FontbrewError::PathResolution {
        message: format!("target path has no parent: {path:?}"),
    })?;

    fs::create_dir_all(parent)?;

    let mut temp_file = NamedTempFile::new_in(parent)?;
    temp_file.write_all(content)?;
    temp_file.flush()?;
    temp_file.as_file().sync_all()?;
    temp_file.persist(path).map_err(|error| error.error)?;
    sync_directory(parent)?;

    Ok(())
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
