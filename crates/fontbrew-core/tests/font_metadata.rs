use std::path::{Path, PathBuf};

use fontbrew_core::fonts::{FontFileFormat, FontMetadataReader, TtfParserMetadataReader};

fn fixture_path(filename: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/fonts")
        .join(filename)
}

#[test]
fn fonts_read_ttf_metadata_for_grouping_and_style() {
    let reader = TtfParserMetadataReader::default();

    let faces = reader
        .read_file(&fixture_path("SourceCodePro-It.ttf"))
        .expect("TTF metadata should parse");

    assert_eq!(faces.len(), 1);
    let face = &faces[0];
    assert_eq!(face.family_name.as_str(), "Source Code Pro");
    assert_eq!(face.subfamily_name.as_deref(), Some("Italic"));
    assert_eq!(face.full_name.as_deref(), Some("Source Code Pro Italic"));
    assert_eq!(face.postscript_name.as_deref(), Some("SourceCodePro-It"));
    assert_eq!(face.weight, Some(400));
    assert!(face.is_italic);
    assert!(!face.is_oblique);
    assert_eq!(face.format, FontFileFormat::Ttf);
    assert_eq!(face.face_index, 0);
}

#[test]
fn fonts_read_otf_metadata() {
    let reader = TtfParserMetadataReader::default();

    let faces = reader
        .read_file(&fixture_path("SourceCodePro-Regular.otf"))
        .expect("OTF metadata should parse");

    assert_eq!(faces.len(), 1);
    let face = &faces[0];
    assert_eq!(face.family_name.as_str(), "Source Code Pro");
    assert_eq!(face.subfamily_name.as_deref(), Some("Regular"));
    assert_eq!(
        face.postscript_name.as_deref(),
        Some("SourceCodePro-Regular")
    );
    assert_eq!(face.weight, Some(400));
    assert!(!face.is_italic);
    assert_eq!(face.format, FontFileFormat::Otf);
    assert_eq!(face.face_index, 0);
}

#[test]
fn fonts_read_variable_ttf_metadata() {
    let reader = TtfParserMetadataReader::default();

    let faces = reader
        .read_file(&fixture_path("Inter-Variable.ttf"))
        .expect("variable TTF metadata should parse");

    assert_eq!(faces.len(), 1);
    let face = &faces[0];
    assert_eq!(face.family_name.as_str(), "Inter");
    assert_eq!(face.postscript_name.as_deref(), Some("Inter-Regular"));
    assert_eq!(face.weight, Some(400));
    assert!(!face.is_italic);
    assert_eq!(face.format, FontFileFormat::Ttf);
    assert_eq!(face.face_index, 0);
}

#[test]
fn fonts_read_ttc_metadata_for_each_face() {
    let reader = TtfParserMetadataReader::default();

    let faces = reader
        .read_file(&fixture_path("SourceCodePro-Collection.ttc"))
        .expect("TTC metadata should parse");

    assert_eq!(faces.len(), 2);
    assert_eq!(faces[0].family_name.as_str(), "Source Code Pro");
    assert_eq!(faces[0].subfamily_name.as_deref(), Some("Regular"));
    assert_eq!(faces[0].weight, Some(400));
    assert_eq!(faces[0].format, FontFileFormat::Ttc);
    assert_eq!(faces[0].face_index, 0);

    assert_eq!(faces[1].family_name.as_str(), "Source Code Pro");
    assert_eq!(faces[1].subfamily_name.as_deref(), Some("Bold"));
    assert_eq!(faces[1].weight, Some(700));
    assert_eq!(faces[1].format, FontFileFormat::Ttc);
    assert_eq!(faces[1].face_index, 1);
}
