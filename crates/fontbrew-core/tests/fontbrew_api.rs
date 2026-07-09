use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

use fontbrew_core::{
    ApplyOptions, ExecutionPolicy, ExtractArchiveRequest, FetchInstallMetadataRequest,
    FontFileInput, FontFormat, Fontbrew, FontbrewError, FontbrewOptions, InstallCandidateId,
    InstallSource, InstallTarget, NoCancellation, NoProgress, PackageId, ParseFontsRequest,
    PlanInstallRequest, PrepareInstallAssetRequest, PrepareInstallSourceRequest,
};
use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

fn package_id(id: &str) -> PackageId {
    PackageId::parse(id).expect("test package id should be valid")
}

fn fixture_font_path(filename: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/fonts")
        .join(filename)
}

fn write_fixture_archive(archive_path: &Path, entries: &[(&str, &str)]) {
    let file = File::create(archive_path).expect("create archive");
    let mut zip = ZipWriter::new(file);

    for (entry_name, fixture_name) in entries {
        let options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .unix_permissions(0o100644);
        zip.start_file(entry_name, options)
            .expect("start archive entry");

        let mut fixture = File::open(fixture_font_path(fixture_name)).expect("open fixture font");
        let mut bytes = Vec::new();
        fixture.read_to_end(&mut bytes).expect("read fixture font");
        zip.write_all(&bytes).expect("write archive entry");
    }

    zip.finish().expect("finish archive");
}

fn write_archive_entry_bytes(archive_path: &Path, entry_name: &str, bytes: &[u8]) {
    let file = File::create(archive_path).expect("create archive");
    let mut zip = ZipWriter::new(file);
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .unix_permissions(0o100644);
    zip.start_file(entry_name, options)
        .expect("start archive entry");
    zip.write_all(bytes).expect("write archive entry");
    zip.finish().expect("finish archive");
}

fn font_bytes_with_unsafe_family_name(fixture_name: &str) -> Vec<u8> {
    let mut bytes = fs::read(fixture_font_path(fixture_name)).expect("read fixture font");
    let from = utf16be("Source Code Pro");
    let to = utf16be("Source/Code Pro");
    let replacements = replace_all_same_length(&mut bytes, &from, &to);

    assert!(replacements > 0, "fixture should contain family name bytes");

    bytes
}

fn utf16be(value: &str) -> Vec<u8> {
    value
        .encode_utf16()
        .flat_map(u16::to_be_bytes)
        .collect::<Vec<_>>()
}

fn replace_all_same_length(bytes: &mut [u8], from: &[u8], to: &[u8]) -> usize {
    assert_eq!(from.len(), to.len());

    let mut replacements = 0;
    let mut offset = 0;
    while let Some(position) = bytes[offset..]
        .windows(from.len())
        .position(|window| window == from)
    {
        let start = offset + position;
        let end = start + from.len();
        bytes[start..end].copy_from_slice(to);
        replacements += 1;
        offset = end;
    }

    replacements
}

fn fontbrew_for_temp(temp: &tempfile::TempDir) -> Fontbrew {
    Fontbrew::new(FontbrewOptions {
        store_dir: Some(temp.path().join("store")),
        config_path: Some(temp.path().join("config/fontbrew.toml")),
        activation_dir: Some(temp.path().join("Fonts/Fontbrew")),
    })
    .expect("create Fontbrew")
}

#[test]
fn extract_archive_returns_font_inputs_for_caller_owned_destination() {
    let temp = tempfile::tempdir().expect("tempdir");
    let archive_path = temp.path().join("source-code-pro.zip");
    write_fixture_archive(
        &archive_path,
        &[("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf")],
    );
    let fontbrew = fontbrew_for_temp(&temp);

    let extracted = fontbrew
        .extract_archive(ExtractArchiveRequest {
            archive_path,
            destination_dir: temp.path().join("caller-staging"),
            options: None,
        })
        .expect("extract archive");

    assert_eq!(extracted.font_files.len(), 1);
    assert_eq!(extracted.font_files[0].format, Some(FontFormat::Ttf));
    assert!(extracted.font_files[0].path.exists());
    assert!(extracted.font_files[0]
        .path
        .starts_with(temp.path().join("caller-staging")));
}

#[test]
fn parse_fonts_accepts_caller_constructed_font_file_inputs() {
    let temp = tempfile::tempdir().expect("tempdir");
    let fontbrew = fontbrew_for_temp(&temp);

    let parsed = fontbrew
        .parse_fonts(ParseFontsRequest {
            files: vec![FontFileInput {
                path: fixture_font_path("SourceCodePro-Regular.ttf"),
                format: None,
            }],
        })
        .expect("parse fonts");

    assert_eq!(parsed.files.len(), 1);
    assert_eq!(parsed.files[0].faces[0].family.as_str(), "Source Code Pro");
    assert_eq!(parsed.files[0].faces[0].style, "Regular");
    assert_eq!(parsed.files[0].faces[0].format, FontFormat::Ttf);
}

#[test]
fn parse_fonts_uses_explicit_format_for_paths_without_font_extension() {
    let temp = tempfile::tempdir().expect("tempdir");
    let font_path = temp.path().join("source-code-pro-regular");
    fs::copy(fixture_font_path("SourceCodePro-Regular.ttf"), &font_path).expect("copy fixture");
    let fontbrew = fontbrew_for_temp(&temp);

    let parsed = fontbrew
        .parse_fonts(ParseFontsRequest {
            files: vec![FontFileInput {
                path: font_path,
                format: Some(FontFormat::Ttf),
            }],
        })
        .expect("parse fonts");

    assert_eq!(parsed.files.len(), 1);
    assert_eq!(parsed.files[0].faces[0].family.as_str(), "Source Code Pro");
    assert_eq!(parsed.files[0].faces[0].format, FontFormat::Ttf);
}

fn staging_entries(temp: &tempfile::TempDir) -> Vec<String> {
    let staging_dir = temp.path().join("store/staging");
    if !staging_dir.exists() {
        return Vec::new();
    }

    let mut entries = fs::read_dir(staging_dir)
        .expect("read staging dir")
        .map(|entry| {
            entry
                .expect("read staging entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect::<Vec<_>>();
    entries.sort();
    entries
}

#[tokio::test]
async fn prepare_install_source_returns_candidates_without_installing_single_family() {
    let temp = tempfile::tempdir().expect("tempdir");
    let archive_path = temp.path().join("source-code-pro.zip");
    write_fixture_archive(
        &archive_path,
        &[("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf")],
    );
    let fontbrew = fontbrew_for_temp(&temp);

    let preparation = fontbrew
        .prepare_install_source(PrepareInstallSourceRequest {
            source: InstallSource::LocalPath(archive_path),
            asset_selector: None,
            format_preference: None,
        })
        .await
        .expect("prepare install source");

    assert_eq!(preparation.candidates().len(), 1);
    assert_eq!(
        preparation.candidates()[0].package_id,
        Some(package_id("source-code-pro"))
    );
    assert!(!temp.path().join("store/manifest.json").exists());
    assert!(!temp.path().join("Fonts/Fontbrew").exists());
}

#[tokio::test]
async fn staged_local_archive_fetch_metadata_then_prepare_asset_returns_candidates() {
    let temp = tempfile::tempdir().expect("tempdir");
    let archive_path = temp.path().join("source-code-pro.zip");
    write_fixture_archive(
        &archive_path,
        &[("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf")],
    );
    let fontbrew = fontbrew_for_temp(&temp);

    let metadata = fontbrew
        .fetch_install_metadata(FetchInstallMetadataRequest {
            source: InstallSource::LocalPath(archive_path),
        })
        .await
        .expect("fetch local metadata");

    assert_eq!(metadata.package_id(), None);
    assert!(metadata.assets().is_empty());
    assert!(staging_entries(&temp).is_empty());

    let mut progress = NoProgress;
    let preparation = fontbrew
        .prepare_install_asset(
            PrepareInstallAssetRequest {
                metadata,
                asset_selector: None,
                format_preference: Vec::new(),
            },
            &mut progress,
            Arc::new(NoCancellation),
        )
        .await
        .expect("prepare local archive");

    assert_eq!(preparation.candidates().len(), 1);
    assert_eq!(
        preparation.candidates()[0].package_id,
        Some(package_id("source-code-pro"))
    );
    assert!(!staging_entries(&temp).is_empty());
    drop(preparation);
    assert!(staging_entries(&temp).is_empty());
}

#[tokio::test]
async fn plan_and_apply_install_installs_selected_candidate() {
    let temp = tempfile::tempdir().expect("tempdir");
    let archive_path = temp.path().join("source-code-pro.zip");
    write_fixture_archive(
        &archive_path,
        &[("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf")],
    );
    let fontbrew = fontbrew_for_temp(&temp);
    let preparation = fontbrew
        .prepare_install_source(PrepareInstallSourceRequest {
            source: InstallSource::LocalPath(archive_path),
            asset_selector: None,
            format_preference: None,
        })
        .await
        .expect("prepare install source");
    let candidate_id = preparation.candidates()[0].id.clone();

    let plans = fontbrew
        .plan_install(PlanInstallRequest {
            preparation,
            targets: vec![InstallTarget {
                candidate_id,
                package_id_override: None,
                reinstall: false,
            }],
        })
        .expect("plan install");
    assert_eq!(plans.plans().len(), 1);
    assert!(plans.risks().is_empty());

    let reports = fontbrew
        .apply_install(
            plans,
            ApplyOptions {
                policy: ExecutionPolicy::SafeOnly,
            },
        )
        .await
        .expect("apply install");

    assert_eq!(reports.packages.len(), 1);
    assert_eq!(
        reports.packages[0].package_id,
        package_id("source-code-pro")
    );
    assert!(temp.path().join("store/manifest.json").exists());
    assert!(temp.path().join("Fonts/Fontbrew").exists());
}

#[tokio::test]
async fn local_archive_candidate_without_default_package_id_can_be_installed_with_override() {
    let temp = tempfile::tempdir().expect("tempdir");
    let archive_path = temp.path().join("unsafe-family.zip");
    let unsafe_family_font = font_bytes_with_unsafe_family_name("SourceCodePro-Regular.ttf");
    write_archive_entry_bytes(&archive_path, "UnsafeFamily.ttf", &unsafe_family_font);
    let fontbrew = fontbrew_for_temp(&temp);

    let preparation = fontbrew
        .prepare_install_source(PrepareInstallSourceRequest {
            source: InstallSource::LocalPath(archive_path.clone()),
            asset_selector: None,
            format_preference: None,
        })
        .await
        .expect("prepare install source");
    let candidate = &preparation.candidates()[0];
    assert_eq!(candidate.package_id, None);
    assert_eq!(candidate.families[0].as_str(), "Source/Code Pro");
    let candidate_id = candidate.id.clone();

    let error = fontbrew
        .plan_install(PlanInstallRequest {
            preparation,
            targets: vec![InstallTarget {
                candidate_id,
                package_id_override: None,
                reinstall: false,
            }],
        })
        .expect_err("missing package id override should fail during planning");

    assert!(matches!(error, FontbrewError::InvalidPackageId { .. }));
    assert!(staging_entries(&temp).is_empty());

    let preparation = fontbrew
        .prepare_install_source(PrepareInstallSourceRequest {
            source: InstallSource::LocalPath(archive_path),
            asset_selector: None,
            format_preference: None,
        })
        .await
        .expect("prepare install source");
    let candidate_id = preparation.candidates()[0].id.clone();
    let plans = fontbrew
        .plan_install(PlanInstallRequest {
            preparation,
            targets: vec![InstallTarget {
                candidate_id,
                package_id_override: Some(package_id("custom-local")),
                reinstall: false,
            }],
        })
        .expect("plan install with package id override");

    let reports = fontbrew
        .apply_install(
            plans,
            ApplyOptions {
                policy: ExecutionPolicy::SafeOnly,
            },
        )
        .await
        .expect("apply install");

    assert_eq!(reports.packages[0].package_id, package_id("custom-local"));
    assert_eq!(reports.packages[0].families[0].as_str(), "Source/Code Pro");
}

#[tokio::test]
async fn plan_install_accepts_multiple_targets_from_one_preparation() {
    let temp = tempfile::tempdir().expect("tempdir");
    let archive_path = temp.path().join("font-families.zip");
    write_fixture_archive(
        &archive_path,
        &[
            ("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf"),
            ("Inter-Variable.ttf", "Inter-Variable.ttf"),
        ],
    );
    let fontbrew = fontbrew_for_temp(&temp);
    let preparation = fontbrew
        .prepare_install_source(PrepareInstallSourceRequest {
            source: InstallSource::LocalPath(archive_path),
            asset_selector: None,
            format_preference: None,
        })
        .await
        .expect("prepare install source");
    let targets = preparation
        .candidates()
        .iter()
        .map(|candidate| InstallTarget {
            candidate_id: candidate.id.clone(),
            package_id_override: None,
            reinstall: false,
        })
        .collect::<Vec<_>>();

    assert_eq!(targets.len(), 2);
    let plans = fontbrew
        .plan_install(PlanInstallRequest {
            preparation,
            targets,
        })
        .expect("plan install");

    assert_eq!(plans.plans().len(), 2);
}

#[tokio::test]
async fn dropping_preparation_cleans_staging() {
    let temp = tempfile::tempdir().expect("tempdir");
    let archive_path = temp.path().join("source-code-pro.zip");
    write_fixture_archive(
        &archive_path,
        &[("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf")],
    );
    let fontbrew = fontbrew_for_temp(&temp);
    let preparation = fontbrew
        .prepare_install_source(PrepareInstallSourceRequest {
            source: InstallSource::LocalPath(archive_path),
            asset_selector: None,
            format_preference: None,
        })
        .await
        .expect("prepare install source");

    assert!(!staging_entries(&temp).is_empty());
    drop(preparation);

    assert!(staging_entries(&temp).is_empty());
}

#[tokio::test]
async fn dropping_plan_set_cleans_staging() {
    let temp = tempfile::tempdir().expect("tempdir");
    let archive_path = temp.path().join("source-code-pro.zip");
    write_fixture_archive(
        &archive_path,
        &[("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf")],
    );
    let fontbrew = fontbrew_for_temp(&temp);
    let preparation = fontbrew
        .prepare_install_source(PrepareInstallSourceRequest {
            source: InstallSource::LocalPath(archive_path),
            asset_selector: None,
            format_preference: None,
        })
        .await
        .expect("prepare install source");
    let candidate_id = preparation.candidates()[0].id.clone();
    let plans = fontbrew
        .plan_install(PlanInstallRequest {
            preparation,
            targets: vec![InstallTarget {
                candidate_id,
                package_id_override: None,
                reinstall: false,
            }],
        })
        .expect("plan install");

    assert!(!staging_entries(&temp).is_empty());
    drop(plans);

    assert!(staging_entries(&temp).is_empty());
}

#[tokio::test]
async fn plan_install_with_unknown_candidate_cleans_staging() {
    let temp = tempfile::tempdir().expect("tempdir");
    let archive_path = temp.path().join("source-code-pro.zip");
    write_fixture_archive(
        &archive_path,
        &[("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf")],
    );
    let fontbrew = fontbrew_for_temp(&temp);
    let preparation = fontbrew
        .prepare_install_source(PrepareInstallSourceRequest {
            source: InstallSource::LocalPath(archive_path),
            asset_selector: None,
            format_preference: None,
        })
        .await
        .expect("prepare install source");
    let unknown_candidate_id: InstallCandidateId =
        serde_json::from_str("\"unknown-candidate\"").expect("deserialize candidate id");

    let error = fontbrew
        .plan_install(PlanInstallRequest {
            preparation,
            targets: vec![InstallTarget {
                candidate_id: unknown_candidate_id,
                package_id_override: None,
                reinstall: false,
            }],
        })
        .expect_err("unknown candidate should fail");

    assert!(error.to_string().contains("unknown install candidate"));
    assert!(staging_entries(&temp).is_empty());
}

#[tokio::test]
async fn install_reads_format_preference_from_config_path_for_each_operation() {
    let temp = tempfile::tempdir().expect("tempdir");
    let archive_path = temp.path().join("source-code-pro.zip");
    write_fixture_archive(
        &archive_path,
        &[
            ("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf"),
            ("SourceCodePro-Regular.otf", "SourceCodePro-Regular.otf"),
        ],
    );
    let config_path = temp.path().join("config/fontbrew.toml");
    fs::create_dir_all(config_path.parent().expect("config parent")).expect("create config parent");
    fs::write(
        &config_path,
        r#"
schema_version = 1

[install]
format_preference = ["ttf", "otf"]
"#,
    )
    .expect("write config");
    let fontbrew = Fontbrew::new(FontbrewOptions {
        store_dir: Some(temp.path().join("store")),
        config_path: Some(config_path),
        activation_dir: Some(temp.path().join("Fonts/Fontbrew")),
    })
    .expect("create Fontbrew");
    let preparation = fontbrew
        .prepare_install_source(PrepareInstallSourceRequest {
            source: InstallSource::LocalPath(archive_path),
            asset_selector: None,
            format_preference: None,
        })
        .await
        .expect("prepare install source");
    let candidate_id = preparation.candidates()[0].id.clone();
    let plans = fontbrew
        .plan_install(PlanInstallRequest {
            preparation,
            targets: vec![InstallTarget {
                candidate_id,
                package_id_override: None,
                reinstall: false,
            }],
        })
        .expect("plan install");

    fontbrew
        .apply_install(
            plans,
            ApplyOptions {
                policy: ExecutionPolicy::SafeOnly,
            },
        )
        .await
        .expect("apply install");

    assert!(temp
        .path()
        .join("store/packages/source-code-pro/local/files/SourceCodePro-Regular.ttf")
        .exists());
    assert!(!temp
        .path()
        .join("store/packages/source-code-pro/local/files/SourceCodePro-Regular.otf")
        .exists());
}

#[tokio::test]
async fn request_format_preference_overrides_config_path_preference() {
    let temp = tempfile::tempdir().expect("tempdir");
    let archive_path = temp.path().join("source-code-pro.zip");
    write_fixture_archive(
        &archive_path,
        &[
            ("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf"),
            ("SourceCodePro-Regular.otf", "SourceCodePro-Regular.otf"),
        ],
    );
    let config_path = temp.path().join("config/fontbrew.toml");
    fs::create_dir_all(config_path.parent().expect("config parent")).expect("create config parent");
    fs::write(
        &config_path,
        r#"
schema_version = 1

[install]
format_preference = ["ttf", "otf"]
"#,
    )
    .expect("write config");
    let fontbrew = Fontbrew::new(FontbrewOptions {
        store_dir: Some(temp.path().join("store")),
        config_path: Some(config_path),
        activation_dir: Some(temp.path().join("Fonts/Fontbrew")),
    })
    .expect("create Fontbrew");
    let preparation = fontbrew
        .prepare_install_source(PrepareInstallSourceRequest {
            source: InstallSource::LocalPath(archive_path),
            asset_selector: None,
            format_preference: Some(vec![FontFormat::Otf]),
        })
        .await
        .expect("prepare install source");
    let candidate_id = preparation.candidates()[0].id.clone();
    let plans = fontbrew
        .plan_install(PlanInstallRequest {
            preparation,
            targets: vec![InstallTarget {
                candidate_id,
                package_id_override: None,
                reinstall: false,
            }],
        })
        .expect("plan install");

    fontbrew
        .apply_install(
            plans,
            ApplyOptions {
                policy: ExecutionPolicy::SafeOnly,
            },
        )
        .await
        .expect("apply install");

    assert!(!temp
        .path()
        .join("store/packages/source-code-pro/local/files/SourceCodePro-Regular.ttf")
        .exists());
    assert!(temp
        .path()
        .join("store/packages/source-code-pro/local/files/SourceCodePro-Regular.otf")
        .exists());
}
