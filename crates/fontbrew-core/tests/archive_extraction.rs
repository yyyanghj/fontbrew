use std::{fs::File, io::Write, path::Path};

use fontbrew_core::{
    archives::{ArchiveExtractionOptions, ZipArchiveExtractor},
    fonts::FontFileFormat,
    FontbrewError,
};
use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

struct ZipEntry<'a> {
    name: &'a str,
    contents: &'a [u8],
    mode: u32,
}

fn write_zip(path: &Path, entries: &[ZipEntry<'_>]) {
    let file = File::create(path).expect("create zip file");
    let mut zip = ZipWriter::new(file);

    for entry in entries {
        let options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .unix_permissions(entry.mode);
        zip.start_file(entry.name, options)
            .expect("start zip entry");
        zip.write_all(entry.contents).expect("write zip entry");
    }

    zip.finish().expect("finish zip file");
}

fn write_zip_with_special_mode(path: &Path, name: &str, contents: &[u8], mode: u32) {
    write_zip(
        path,
        &[ZipEntry {
            name,
            contents,
            mode: 0o644,
        }],
    );

    let mut zip_bytes = std::fs::read(path).expect("read generated zip");
    let central_header = zip_bytes
        .windows(4)
        .position(|window| window == b"PK\x01\x02")
        .expect("central directory header should exist");
    let external_attributes_offset = central_header + 38;
    zip_bytes[external_attributes_offset..external_attributes_offset + 4]
        .copy_from_slice(&(mode << 16).to_le_bytes());
    std::fs::write(path, zip_bytes).expect("write patched zip");
}

fn write_zip_with_symlink(path: &Path, name: &str, target: &str) {
    let file = File::create(path).expect("create zip file");
    let mut zip = ZipWriter::new(file);
    zip.add_symlink(name, target, SimpleFileOptions::default())
        .expect("add symlink entry");
    zip.finish().expect("finish zip file");
}

fn extract_archive(
    archive_path: &Path,
    staging_dir: &Path,
    options: ArchiveExtractionOptions,
) -> fontbrew_core::Result<Vec<fontbrew_core::archives::ExtractedFontFile>> {
    ZipArchiveExtractor::new(options).extract(archive_path, staging_dir)
}

#[test]
fn archives_extract_safe_desktop_fonts_into_staging() {
    let temp = tempfile::tempdir().expect("tempdir");
    let archive_path = temp.path().join("fonts.zip");
    let staging_dir = temp.path().join("staging");
    write_zip(
        &archive_path,
        &[
            ZipEntry {
                name: "family/Regular.ttf",
                contents: b"ttf bytes",
                mode: 0o100644,
            },
            ZipEntry {
                name: "family/Bold.otf",
                contents: b"otf bytes",
                mode: 0o100644,
            },
        ],
    );

    let files = extract_archive(
        &archive_path,
        &staging_dir,
        ArchiveExtractionOptions::default(),
    )
    .expect("safe archive should extract");

    assert_eq!(files.len(), 2);
    assert_eq!(files[0].path, staging_dir.join("family/Regular.ttf"));
    assert_eq!(files[0].format, FontFileFormat::Ttf);
    assert_eq!(files[1].path, staging_dir.join("family/Bold.otf"));
    assert_eq!(files[1].format, FontFileFormat::Otf);
    assert_eq!(
        std::fs::read(staging_dir.join("family/Regular.ttf")).expect("read extracted font"),
        b"ttf bytes"
    );
}

#[test]
fn archives_reject_path_traversal_entries_without_writing_outside_staging() {
    let temp = tempfile::tempdir().expect("tempdir");
    let archive_path = temp.path().join("evil.zip");
    let staging_dir = temp.path().join("staging");
    let outside_path = temp.path().join("escape.ttf");
    write_zip(
        &archive_path,
        &[ZipEntry {
            name: "../escape.ttf",
            contents: b"outside",
            mode: 0o100644,
        }],
    );

    let error = extract_archive(
        &archive_path,
        &staging_dir,
        ArchiveExtractionOptions::default(),
    )
    .expect_err("path traversal should be rejected");

    assert!(matches!(error, FontbrewError::ArchiveRejected { .. }));
    assert!(
        !outside_path.exists(),
        "archive extraction must not write outside staging"
    );
}

#[test]
fn archives_reject_absolute_path_entries_without_writing_outside_staging() {
    let temp = tempfile::tempdir().expect("tempdir");
    let archive_path = temp.path().join("absolute.zip");
    let staging_dir = temp.path().join("staging");
    let outside_path = temp.path().join("absolute-evil.ttf");
    let entry_name = outside_path.to_string_lossy();
    write_zip(
        &archive_path,
        &[ZipEntry {
            name: entry_name.as_ref(),
            contents: b"outside",
            mode: 0o100644,
        }],
    );

    let error = extract_archive(
        &archive_path,
        &staging_dir,
        ArchiveExtractionOptions::default(),
    )
    .expect_err("absolute path should be rejected");

    assert!(matches!(error, FontbrewError::ArchiveRejected { .. }));
    assert!(
        !outside_path.exists(),
        "archive extraction must not write absolute entry paths"
    );
}

#[test]
fn archives_ignore_webfont_only_archives() {
    let temp = tempfile::tempdir().expect("tempdir");
    let archive_path = temp.path().join("web.zip");
    let staging_dir = temp.path().join("staging");
    write_zip(
        &archive_path,
        &[
            ZipEntry {
                name: "web/Font.woff2",
                contents: b"woff2 bytes",
                mode: 0o100644,
            },
            ZipEntry {
                name: "web/styles.css",
                contents: b"@font-face {}",
                mode: 0o100644,
            },
        ],
    );

    let files = extract_archive(
        &archive_path,
        &staging_dir,
        ArchiveExtractionOptions::default(),
    )
    .expect("web-only archive should be ignored without failing");

    assert!(files.is_empty());
    assert!(!staging_dir.join("web/Font.woff2").exists());
    assert!(!staging_dir.join("web/styles.css").exists());
}

#[test]
fn archives_extract_desktop_fonts_from_mixed_web_and_desktop_archives() {
    let temp = tempfile::tempdir().expect("tempdir");
    let archive_path = temp.path().join("mixed.zip");
    let staging_dir = temp.path().join("staging");
    write_zip(
        &archive_path,
        &[
            ZipEntry {
                name: "Inter-Regular.woff2",
                contents: b"webfont",
                mode: 0o100644,
            },
            ZipEntry {
                name: "Inter-Regular.ttf",
                contents: b"desktop",
                mode: 0o100644,
            },
            ZipEntry {
                name: "README.md",
                contents: b"docs",
                mode: 0o100644,
            },
        ],
    );

    let files = extract_archive(
        &archive_path,
        &staging_dir,
        ArchiveExtractionOptions::default(),
    )
    .expect("mixed archive should extract desktop fonts");

    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path, staging_dir.join("Inter-Regular.ttf"));
    assert_eq!(files[0].format, FontFileFormat::Ttf);
    assert!(!staging_dir.join("Inter-Regular.woff2").exists());
    assert!(!staging_dir.join("README.md").exists());
}

#[test]
fn archives_extract_ttc_and_otc_desktop_formats() {
    let temp = tempfile::tempdir().expect("tempdir");
    let archive_path = temp.path().join("collections.zip");
    let staging_dir = temp.path().join("staging");
    write_zip(
        &archive_path,
        &[
            ZipEntry {
                name: "collections/Family.ttc",
                contents: b"ttc bytes",
                mode: 0o100644,
            },
            ZipEntry {
                name: "collections/Family.otc",
                contents: b"otc bytes",
                mode: 0o100644,
            },
        ],
    );

    let files = extract_archive(
        &archive_path,
        &staging_dir,
        ArchiveExtractionOptions::default(),
    )
    .expect("collection formats should extract");

    assert_eq!(files.len(), 2);
    assert_eq!(files[0].path, staging_dir.join("collections/Family.ttc"));
    assert_eq!(files[0].format, FontFileFormat::Ttc);
    assert_eq!(files[1].path, staging_dir.join("collections/Family.otc"));
    assert_eq!(files[1].format, FontFileFormat::Otc);
}

#[test]
fn archives_reject_entries_larger_than_single_file_limit() {
    let temp = tempfile::tempdir().expect("tempdir");
    let archive_path = temp.path().join("oversized.zip");
    let staging_dir = temp.path().join("staging");
    write_zip(
        &archive_path,
        &[ZipEntry {
            name: "Big.ttf",
            contents: b"too large",
            mode: 0o100644,
        }],
    );

    let error = extract_archive(
        &archive_path,
        &staging_dir,
        ArchiveExtractionOptions {
            max_single_file_size: 4,
            ..ArchiveExtractionOptions::default()
        },
    )
    .expect_err("oversized archive should be rejected");

    assert!(matches!(error, FontbrewError::ArchiveRejected { .. }));
    assert!(!staging_dir.join("Big.ttf").exists());
}

#[test]
fn archives_reject_archives_over_total_size_limit() {
    let temp = tempfile::tempdir().expect("tempdir");
    let archive_path = temp.path().join("oversized-total.zip");
    let staging_dir = temp.path().join("staging");
    write_zip(
        &archive_path,
        &[
            ZipEntry {
                name: "One.ttf",
                contents: b"1234",
                mode: 0o100644,
            },
            ZipEntry {
                name: "Two.otf",
                contents: b"5678",
                mode: 0o100644,
            },
        ],
    );

    let error = extract_archive(
        &archive_path,
        &staging_dir,
        ArchiveExtractionOptions {
            max_total_extracted_size: 6,
            ..ArchiveExtractionOptions::default()
        },
    )
    .expect_err("total size limit should reject archive");

    assert!(matches!(error, FontbrewError::ArchiveRejected { .. }));
}

#[test]
fn archives_reject_when_desktop_font_count_exceeds_limit() {
    let temp = tempfile::tempdir().expect("tempdir");
    let archive_path = temp.path().join("too-many.zip");
    let staging_dir = temp.path().join("staging");
    write_zip(
        &archive_path,
        &[
            ZipEntry {
                name: "One.ttf",
                contents: b"one",
                mode: 0o100644,
            },
            ZipEntry {
                name: "Two.otf",
                contents: b"two",
                mode: 0o100644,
            },
        ],
    );

    let error = extract_archive(
        &archive_path,
        &staging_dir,
        ArchiveExtractionOptions {
            max_extracted_files: 1,
            ..ArchiveExtractionOptions::default()
        },
    )
    .expect_err("file count limit should reject archive");

    assert!(matches!(error, FontbrewError::ArchiveRejected { .. }));
}

#[test]
fn archives_reject_special_file_unix_modes() {
    let temp = tempfile::tempdir().expect("tempdir");
    let archive_path = temp.path().join("fifo.zip");
    let staging_dir = temp.path().join("staging");
    write_zip_with_special_mode(&archive_path, "Pipe.ttf", b"not a regular file", 0o010644);

    let error = extract_archive(
        &archive_path,
        &staging_dir,
        ArchiveExtractionOptions::default(),
    )
    .expect_err("special file mode should be rejected");

    assert!(matches!(error, FontbrewError::ArchiveRejected { .. }));
    assert!(!staging_dir.join("Pipe.ttf").exists());
}

#[test]
fn archives_reject_symlink_entries() {
    let temp = tempfile::tempdir().expect("tempdir");
    let archive_path = temp.path().join("symlink.zip");
    let staging_dir = temp.path().join("staging");
    write_zip_with_symlink(&archive_path, "Link.ttf", "../outside.ttf");

    let error = extract_archive(
        &archive_path,
        &staging_dir,
        ArchiveExtractionOptions::default(),
    )
    .expect_err("symlink entry should be rejected");

    assert!(matches!(error, FontbrewError::ArchiveRejected { .. }));
    assert!(!staging_dir.join("Link.ttf").exists());
}

#[cfg(unix)]
#[test]
fn archives_reject_destinations_that_cross_existing_symlink_ancestor() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("tempdir");
    let archive_path = temp.path().join("symlink-ancestor.zip");
    let staging_dir = temp.path().join("staging");
    let outside_dir = temp.path().join("outside");
    std::fs::create_dir_all(&outside_dir).expect("create outside dir");
    std::fs::create_dir_all(&staging_dir).expect("create staging dir");
    symlink(&outside_dir, staging_dir.join("linked")).expect("create symlink ancestor");
    write_zip(
        &archive_path,
        &[ZipEntry {
            name: "linked/Font.ttf",
            contents: b"font",
            mode: 0o100644,
        }],
    );

    let error = extract_archive(
        &archive_path,
        &staging_dir,
        ArchiveExtractionOptions::default(),
    )
    .expect_err("existing symlink ancestor should be rejected");

    assert!(matches!(error, FontbrewError::ArchiveRejected { .. }));
    assert!(!outside_dir.join("Font.ttf").exists());
}
