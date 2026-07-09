use std::path::Path;

use crate::{FamilyName, FontbrewError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontFileFormat {
    Ttf,
    Otf,
    Ttc,
    Otc,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontFaceMetadata {
    pub family_name: FamilyName,
    pub subfamily_name: Option<String>,
    pub full_name: Option<String>,
    pub postscript_name: Option<String>,
    pub weight: Option<u16>,
    pub is_italic: bool,
    pub is_oblique: bool,
    pub format: FontFileFormat,
    pub face_index: u32,
}

pub trait FontMetadataReader {
    fn read_file(&self, path: &Path) -> Result<Vec<FontFaceMetadata>>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct TtfParserMetadataReader;

impl TtfParserMetadataReader {
    pub(crate) fn read_file_with_format(
        &self,
        path: &Path,
        format: FontFileFormat,
    ) -> Result<Vec<FontFaceMetadata>> {
        let data = std::fs::read(path)?;
        let face_count = match format {
            FontFileFormat::Ttc | FontFileFormat::Otc => ttf_parser::fonts_in_collection(&data)
                .ok_or_else(|| FontbrewError::FontParse {
                    message: format!(
                        "font collection has no collection header: {}",
                        path.display()
                    ),
                })?,
            FontFileFormat::Ttf | FontFileFormat::Otf => 1,
        };

        let mut faces = Vec::with_capacity(face_count as usize);
        for face_index in 0..face_count {
            let face = ttf_parser::Face::parse(&data, face_index).map_err(|source| {
                FontbrewError::FontParse {
                    message: format!(
                        "could not parse face {face_index} in {}: {source}",
                        path.display()
                    ),
                }
            })?;

            faces.push(read_face_metadata(&face, format, face_index, path)?);
        }

        Ok(faces)
    }
}

impl FontMetadataReader for TtfParserMetadataReader {
    fn read_file(&self, path: &Path) -> Result<Vec<FontFaceMetadata>> {
        let format = FontFileFormat::from_path(path)?;
        self.read_file_with_format(path, format)
    }
}

impl FontFileFormat {
    fn from_path(path: &Path) -> Result<Self> {
        let extension = path
            .extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase);

        match extension.as_deref() {
            Some("ttf") => Ok(Self::Ttf),
            Some("otf") => Ok(Self::Otf),
            Some("ttc") => Ok(Self::Ttc),
            Some("otc") => Ok(Self::Otc),
            _ => Err(FontbrewError::FontParse {
                message: format!("unsupported font file extension: {}", path.display()),
            }),
        }
    }
}

fn read_face_metadata(
    face: &ttf_parser::Face<'_>,
    format: FontFileFormat,
    face_index: u32,
    path: &Path,
) -> Result<FontFaceMetadata> {
    let family_name = preferred_name(
        face,
        &[
            ttf_parser::name_id::TYPOGRAPHIC_FAMILY,
            ttf_parser::name_id::WWS_FAMILY,
            ttf_parser::name_id::FAMILY,
        ],
    )
    .ok_or_else(|| FontbrewError::FontParse {
        message: format!(
            "face {face_index} in {} has no decodable family name",
            path.display()
        ),
    })?;

    Ok(FontFaceMetadata {
        family_name: FamilyName::new(family_name),
        subfamily_name: preferred_name(
            face,
            &[
                ttf_parser::name_id::TYPOGRAPHIC_SUBFAMILY,
                ttf_parser::name_id::WWS_SUBFAMILY,
                ttf_parser::name_id::SUBFAMILY,
            ],
        ),
        full_name: preferred_name(face, &[ttf_parser::name_id::FULL_NAME]),
        postscript_name: preferred_name(face, &[ttf_parser::name_id::POST_SCRIPT_NAME]),
        weight: Some(face.weight().to_number()),
        is_italic: face.is_italic(),
        is_oblique: face.is_oblique(),
        format,
        face_index,
    })
}

fn preferred_name(face: &ttf_parser::Face<'_>, name_ids: &[u16]) -> Option<String> {
    for name_id in name_ids {
        if let Some(name) = english_unicode_name(face, *name_id) {
            return Some(name);
        }
    }

    for name_id in name_ids {
        if let Some(name) = unicode_name(face, *name_id) {
            return Some(name);
        }
    }

    None
}

fn english_unicode_name(face: &ttf_parser::Face<'_>, name_id: u16) -> Option<String> {
    face.names()
        .into_iter()
        .filter(|name| name.name_id == name_id && name.is_unicode())
        .find(|name| name.language() == ttf_parser::Language::English_UnitedStates)
        .and_then(|name| non_empty_name(name.to_string()))
}

fn unicode_name(face: &ttf_parser::Face<'_>, name_id: u16) -> Option<String> {
    face.names()
        .into_iter()
        .filter(|name| name.name_id == name_id && name.is_unicode())
        .find_map(|name| non_empty_name(name.to_string()))
}

fn non_empty_name(name: Option<String>) -> Option<String> {
    let name = name?;
    let name = name.trim();

    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}
