use std::{
    fs::{self, File},
    io,
    path::{Component, Path, PathBuf},
};

use zip::{CompressionMethod, ZipArchive};

use crate::{
    error::{FontbrewError, Result},
    fonts::FontFileFormat,
    fs::ensure_existing_path_does_not_cross_symlink,
};

const DEFAULT_MAX_TOTAL_EXTRACTED_SIZE: u64 = 512 * 1024 * 1024;
const DEFAULT_MAX_EXTRACTED_FILES: usize = 256;
const DEFAULT_MAX_SINGLE_FILE_SIZE: u64 = 64 * 1024 * 1024;

const UNIX_MODE_TYPE_MASK: u32 = 0o170000;
const UNIX_MODE_REGULAR_FILE: u32 = 0o100000;
const UNIX_MODE_DIRECTORY: u32 = 0o040000;
const UNIX_MODE_SYMLINK: u32 = 0o120000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArchiveExtractionOptions {
    pub max_total_extracted_size: u64,
    pub max_extracted_files: usize,
    pub max_single_file_size: u64,
}

impl Default for ArchiveExtractionOptions {
    fn default() -> Self {
        Self {
            max_total_extracted_size: DEFAULT_MAX_TOTAL_EXTRACTED_SIZE,
            max_extracted_files: DEFAULT_MAX_EXTRACTED_FILES,
            max_single_file_size: DEFAULT_MAX_SINGLE_FILE_SIZE,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedFontFile {
    pub path: PathBuf,
    pub format: FontFileFormat,
}

#[derive(Debug, Clone, Copy)]
pub struct ZipArchiveExtractor {
    options: ArchiveExtractionOptions,
}

impl ZipArchiveExtractor {
    pub fn new(options: ArchiveExtractionOptions) -> Self {
        Self { options }
    }

    pub fn extract(
        &self,
        archive_path: impl AsRef<Path>,
        staging_dir: impl AsRef<Path>,
    ) -> Result<Vec<ExtractedFontFile>> {
        let archive_path = archive_path.as_ref();
        let staging_dir = staging_dir.as_ref();
        let planned_fonts = self.plan_extraction(archive_path, staging_dir)?;

        let staging_root = staging_dir.parent().unwrap_or(staging_dir);
        ensure_existing_path_does_not_cross_symlink(staging_root, staging_dir)?;
        fs::create_dir_all(staging_dir)?;

        let file = File::open(archive_path)?;
        let mut archive = zip_archive(file)?;
        let mut extracted_fonts = Vec::with_capacity(planned_fonts.len());

        for planned_font in planned_fonts {
            let mut entry = zip_entry(&mut archive, planned_font.index)?;
            let destination_path = staging_dir.join(&planned_font.relative_path);

            reject_existing_symlink_ancestors(staging_dir, &destination_path)?;

            if let Some(parent) = destination_path.parent() {
                fs::create_dir_all(parent)?;
            }

            let mut destination = File::create(&destination_path)?;
            io::copy(&mut entry, &mut destination)?;

            extracted_fonts.push(ExtractedFontFile {
                path: destination_path,
                format: planned_font.format,
            });
        }

        Ok(extracted_fonts)
    }

    fn plan_extraction(
        &self,
        archive_path: &Path,
        staging_dir: &Path,
    ) -> Result<Vec<PlannedFontExtraction>> {
        let file = File::open(archive_path)?;
        let mut archive = zip_archive(file)?;
        let mut planned_fonts = Vec::new();
        let mut total_extracted_size = 0_u64;

        for index in 0..archive.len() {
            let entry = zip_entry(&mut archive, index)?;
            let relative_path = safe_relative_path(entry.name())?;

            reject_unsupported_compression(entry.compression())?;
            reject_encrypted_entry(entry.encrypted(), entry.name())?;
            reject_unsafe_mode(entry.unix_mode(), entry.is_dir(), entry.name())?;

            if entry.is_dir() {
                continue;
            }

            let Some(format) = desktop_font_format(&relative_path) else {
                continue;
            };

            let entry_size = entry.size();
            if entry_size > self.options.max_single_file_size {
                return archive_rejected(format!(
                    "entry {} exceeds single file size limit",
                    entry.name()
                ));
            }

            total_extracted_size =
                total_extracted_size
                    .checked_add(entry_size)
                    .ok_or_else(|| FontbrewError::ArchiveRejected {
                        reason: "archive extracted size overflowed".to_string(),
                    })?;

            if total_extracted_size > self.options.max_total_extracted_size {
                return archive_rejected("archive exceeds total extracted size limit");
            }

            if planned_fonts.len() + 1 > self.options.max_extracted_files {
                return archive_rejected("archive exceeds extracted font file count limit");
            }

            let destination_path = staging_dir.join(&relative_path);
            if !absolute_path(&destination_path)?.starts_with(&absolute_path(staging_dir)?) {
                return archive_rejected(format!(
                    "entry {} would extract outside staging",
                    entry.name()
                ));
            }

            planned_fonts.push(PlannedFontExtraction {
                index,
                relative_path,
                format,
            });
        }

        Ok(planned_fonts)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PlannedFontExtraction {
    index: usize,
    relative_path: PathBuf,
    format: FontFileFormat,
}

fn zip_archive(file: File) -> Result<ZipArchive<File>> {
    ZipArchive::new(file).map_err(|source| FontbrewError::ArchiveRejected {
        reason: format!("could not read zip archive: {source}"),
    })
}

fn zip_entry<R: io::Read + io::Seek>(
    archive: &mut ZipArchive<R>,
    index: usize,
) -> Result<zip::read::ZipFile<'_, R>> {
    archive
        .by_index(index)
        .map_err(|source| FontbrewError::ArchiveRejected {
            reason: format!("could not read zip entry {index}: {source}"),
        })
}

fn safe_relative_path(entry_name: &str) -> Result<PathBuf> {
    if entry_name.is_empty() {
        return archive_rejected("archive entry has an empty path");
    }

    let path = Path::new(entry_name);
    let mut safe_path = PathBuf::new();

    for component in path.components() {
        match component {
            Component::Normal(part) => safe_path.push(part),
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => {
                return archive_rejected(format!("archive entry has an unsafe path: {entry_name}"));
            }
        }
    }

    if safe_path.as_os_str().is_empty() {
        return archive_rejected("archive entry has an empty path");
    }

    Ok(safe_path)
}

fn reject_unsupported_compression(method: CompressionMethod) -> Result<()> {
    if method == CompressionMethod::Stored || method == CompressionMethod::DEFLATE {
        Ok(())
    } else {
        archive_rejected(format!("unsupported zip compression method: {method:?}"))
    }
}

fn reject_encrypted_entry(is_encrypted: bool, entry_name: &str) -> Result<()> {
    if is_encrypted {
        return archive_rejected(format!("archive entry is encrypted: {entry_name}"));
    }

    Ok(())
}

fn reject_unsafe_mode(mode: Option<u32>, is_directory: bool, entry_name: &str) -> Result<()> {
    let Some(mode) = mode else {
        return Ok(());
    };

    let file_type = mode & UNIX_MODE_TYPE_MASK;
    if file_type == UNIX_MODE_SYMLINK {
        return archive_rejected(format!("archive entry is a symlink: {entry_name}"));
    }

    if is_directory && (file_type == 0 || file_type == UNIX_MODE_DIRECTORY) {
        return Ok(());
    }

    if !is_directory && (file_type == 0 || file_type == UNIX_MODE_REGULAR_FILE) {
        return Ok(());
    }

    archive_rejected(format!(
        "archive entry is not a regular file or directory: {entry_name}"
    ))
}

fn desktop_font_format(path: &Path) -> Option<FontFileFormat> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();

    match extension.as_str() {
        "ttf" => Some(FontFileFormat::Ttf),
        "otf" => Some(FontFileFormat::Otf),
        "ttc" => Some(FontFileFormat::Ttc),
        "otc" => Some(FontFileFormat::Otc),
        _ => None,
    }
}

fn reject_existing_symlink_ancestors(staging_dir: &Path, destination_path: &Path) -> Result<()> {
    let mut current_path = PathBuf::from(staging_dir);
    let relative_path =
        destination_path
            .strip_prefix(staging_dir)
            .map_err(|_| FontbrewError::ArchiveRejected {
                reason: "destination is outside staging".to_string(),
            })?;

    for component in relative_path.components() {
        current_path.push(component.as_os_str());
        if let Ok(metadata) = fs::symlink_metadata(&current_path) {
            if metadata.file_type().is_symlink() {
                return archive_rejected(format!(
                    "destination path crosses an existing symlink: {}",
                    current_path.display()
                ));
            }
        }
    }

    Ok(())
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn archive_rejected<T>(reason: impl Into<String>) -> Result<T> {
    Err(FontbrewError::ArchiveRejected {
        reason: reason.into(),
    })
}
