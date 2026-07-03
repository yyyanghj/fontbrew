use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

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
