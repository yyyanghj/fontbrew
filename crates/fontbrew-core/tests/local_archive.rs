use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};

use fontbrew_core::{
    activation::ActivationStrategy,
    manifest::{ManifestPackageRecord, ManifestSource, ManifestStore, ManifestV1},
    platform::FontbrewPaths,
    CancellationToken, ExecutionPolicy, FamilyName, FontFormat, FontbrewApp, FontbrewError,
    InfoRequest, InstallRequest, InstallSource, NoCancellation, PackageId, PackageVersion,
    PlanRisk, ProgressEvent, ProgressSink, ProviderKind, RemovePlan, RemoveRequest,
};
use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

struct NoProgress;

impl ProgressSink for NoProgress {
    fn emit(&mut self, _event: ProgressEvent) {}
}

struct NeverCancelled;

impl CancellationToken for NeverCancelled {
    fn is_cancelled(&self) -> bool {
        false
    }
}

struct AlwaysCancelled;

impl CancellationToken for AlwaysCancelled {
    fn is_cancelled(&self) -> bool {
        true
    }
}

struct CancelOnCheck {
    checks: AtomicUsize,
    cancel_on_check: usize,
}

impl CancelOnCheck {
    fn new(cancel_on_check: usize) -> Self {
        Self {
            checks: AtomicUsize::new(0),
            cancel_on_check,
        }
    }
}

impl CancellationToken for CancelOnCheck {
    fn is_cancelled(&self) -> bool {
        self.checks.fetch_add(1, Ordering::SeqCst) + 1 >= self.cancel_on_check
    }
}

struct CancelWhenInstallStagingExists {
    paths: FontbrewPaths,
}

impl CancellationToken for CancelWhenInstallStagingExists {
    fn is_cancelled(&self) -> bool {
        staging_entries(&self.paths)
            .iter()
            .any(|entry| entry.starts_with("install-"))
    }
}

fn package_id(id: &str) -> PackageId {
    PackageId::parse(id).expect("test package id should be valid")
}

fn fixture_font_path(filename: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/fonts")
        .join(filename)
}

fn test_paths(temp: &tempfile::TempDir) -> FontbrewPaths {
    FontbrewPaths::for_tests(
        temp.path().join("data"),
        temp.path().join("config"),
        temp.path().join("home"),
    )
}

fn local_archive_request(archive_path: &Path, reinstall: bool) -> InstallRequest {
    local_archive_request_with_formats(archive_path, reinstall, Vec::new())
}

fn local_archive_request_with_formats(
    archive_path: &Path,
    reinstall: bool,
    format_preference: Vec<FontFormat>,
) -> InstallRequest {
    InstallRequest {
        source: InstallSource::LocalPath(archive_path.to_path_buf()),
        package_id_override: None,
        format_preference,
        asset_selector: None,
        selected_families: Vec::new(),
        reinstall,
    }
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

fn local_archive_request_with_package_id_override(
    archive_path: &Path,
    package_id_override: PackageId,
) -> InstallRequest {
    InstallRequest {
        source: InstallSource::LocalPath(archive_path.to_path_buf()),
        package_id_override: Some(package_id_override),
        format_preference: Vec::new(),
        asset_selector: None,
        selected_families: Vec::new(),
        reinstall: false,
    }
}

fn local_archive_request_with_selected_families(
    archive_path: &Path,
    families: Vec<&str>,
) -> InstallRequest {
    InstallRequest {
        source: InstallSource::LocalPath(archive_path.to_path_buf()),
        package_id_override: None,
        format_preference: Vec::new(),
        asset_selector: None,
        selected_families: families.into_iter().map(FamilyName::new).collect(),
        reinstall: false,
    }
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
    while offset + from.len() <= bytes.len() {
        if &bytes[offset..offset + from.len()] == from {
            bytes[offset..offset + to.len()].copy_from_slice(to);
            replacements += 1;
            offset += from.len();
        } else {
            offset += 1;
        }
    }

    replacements
}

fn write_invalid_font_archive(archive_path: &Path) {
    let file = File::create(archive_path).expect("create archive");
    let mut zip = ZipWriter::new(file);
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .unix_permissions(0o100644);
    zip.start_file("Broken.ttf", options)
        .expect("start archive entry");
    zip.write_all(b"not a real font")
        .expect("write invalid font");
    zip.finish().expect("finish archive");
}

fn staging_entries(paths: &FontbrewPaths) -> Vec<String> {
    if !paths.staging_dir().exists() {
        return Vec::new();
    }

    let mut entries = fs::read_dir(paths.staging_dir())
        .expect("read staging root")
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

fn apply_install(app: &FontbrewApp, archive_path: &Path) -> fontbrew_core::Result<()> {
    let plan = app.install_plan(local_archive_request(archive_path, false))?;
    let mut progress = NoProgress;
    let cancellation = NeverCancelled;
    app.apply_install(
        plan,
        ExecutionPolicy::SafeOnly,
        &mut progress,
        &cancellation,
    )?;
    Ok(())
}

#[test]
fn no_cancellation_token_never_cancels() {
    assert!(!NoCancellation.is_cancelled());
}

#[test]
fn install_plan_serialization_does_not_expose_prepared_internal_paths() {
    let temp = tempfile::tempdir().expect("tempdir");
    let app = FontbrewApp::with_paths(test_paths(&temp));
    let archive_path = temp.path().join("source-code-pro.zip");
    write_fixture_archive(
        &archive_path,
        &[("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf")],
    );

    let plan = app
        .install_plan(local_archive_request(&archive_path, false))
        .expect("plan local archive install");
    let json = serde_json::to_value(&plan).expect("plan should serialize");

    assert!(json.get("prepared").is_none());
    assert!(!json.to_string().contains("staging"));
    assert!(!json.to_string().contains("package_store"));
}

#[test]
fn apply_install_cancelled_before_apply_cleans_staging_and_does_not_install() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let app = FontbrewApp::with_paths(paths.clone());
    let archive_path = temp.path().join("source-code-pro.zip");
    write_fixture_archive(
        &archive_path,
        &[("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf")],
    );
    let plan = app
        .install_plan(local_archive_request(&archive_path, false))
        .expect("plan local archive install");
    assert!(
        staging_entries(&paths)
            .iter()
            .any(|entry| entry.starts_with("install-")),
        "install planning should create staging"
    );

    let error = app
        .apply_install(
            plan,
            ExecutionPolicy::SafeOnly,
            &mut NoProgress,
            &AlwaysCancelled,
        )
        .expect_err("cancelled apply should fail");

    assert!(matches!(error, FontbrewError::Cancelled));
    assert!(staging_entries(&paths).is_empty());
    assert!(!paths.manifest_path().exists());
    assert!(!paths
        .package_store_dir(
            &package_id("source-code-pro"),
            &PackageVersion::new("local"),
        )
        .exists());
}

#[test]
fn install_plan_cancellation_after_local_staging_creation_cleans_staging() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let app = FontbrewApp::with_paths(paths.clone());
    let archive_path = temp.path().join("source-code-pro.zip");
    write_fixture_archive(
        &archive_path,
        &[("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf")],
    );

    let error = app
        .install_plan_with_cancellation(
            local_archive_request(&archive_path, false),
            &CancelWhenInstallStagingExists {
                paths: paths.clone(),
            },
        )
        .expect_err("cancellation after staging creation should fail");

    assert!(matches!(error, FontbrewError::Cancelled));
    assert!(staging_entries(&paths).is_empty());
}

#[cfg(unix)]
#[test]
fn install_plan_removes_stale_install_staging_without_touching_unrelated_paths_or_symlink_targets()
{
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let app = FontbrewApp::with_paths(paths.clone());
    let archive_path = temp.path().join("source-code-pro.zip");
    write_fixture_archive(
        &archive_path,
        &[("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf")],
    );
    let staging_root = paths.staging_dir();
    fs::create_dir_all(&staging_root).expect("create staging root");
    let stale_dir = staging_root.join("install-stale");
    fs::create_dir_all(&stale_dir).expect("create stale staging");
    fs::write(stale_dir.join("old"), b"old staging").expect("write stale file");
    let unrelated_dir = staging_root.join("keep-this");
    fs::create_dir_all(&unrelated_dir).expect("create unrelated staging");
    fs::write(unrelated_dir.join("keep"), b"keep").expect("write unrelated file");
    let outside_dir = temp.path().join("outside-target");
    fs::create_dir_all(&outside_dir).expect("create outside target");
    fs::write(outside_dir.join("keep"), b"outside").expect("write outside file");
    let symlink_trap = staging_root.join("install-symlink-trap");
    std::os::unix::fs::symlink(&outside_dir, &symlink_trap).expect("create stale symlink trap");

    let plan = app
        .install_plan(local_archive_request(&archive_path, false))
        .expect("plan local archive install");

    assert!(!stale_dir.exists());
    assert!(!symlink_trap.exists());
    assert!(outside_dir.join("keep").exists());
    assert!(unrelated_dir.join("keep").exists());
    let install_entries = staging_entries(&paths)
        .into_iter()
        .filter(|entry| entry.starts_with("install-"))
        .collect::<Vec<_>>();
    assert_eq!(install_entries.len(), 1);

    app.discard_install_plan(plan);
    assert!(unrelated_dir.join("keep").exists());
    assert!(outside_dir.join("keep").exists());
}

#[test]
fn install_plan_stale_cleanup_preserves_existing_prepared_plan_staging() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let app = FontbrewApp::with_paths(paths.clone());
    let first_archive = temp.path().join("source-code-pro.zip");
    let second_archive = temp.path().join("source-code-pro-copy.zip");
    write_fixture_archive(
        &first_archive,
        &[("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf")],
    );
    write_fixture_archive(
        &second_archive,
        &[("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf")],
    );

    let first_plan = app
        .install_plan(local_archive_request(&first_archive, false))
        .expect("plan first local archive install");
    let second_plan = app
        .install_plan(local_archive_request(&second_archive, false))
        .expect("planning second install should not delete first staging");

    assert_eq!(
        staging_entries(&paths)
            .into_iter()
            .filter(|entry| entry.starts_with("install-"))
            .count(),
        2
    );
    app.discard_install_plan(second_plan);
    app.apply_install(
        first_plan,
        ExecutionPolicy::SafeOnly,
        &mut NoProgress,
        &NeverCancelled,
    )
    .expect("first plan should remain applicable after second planning cleanup");
    assert!(paths
        .package_store_dir(
            &package_id("source-code-pro"),
            &PackageVersion::new("local"),
        )
        .join("files/SourceCodePro-Regular.ttf")
        .exists());
}

#[test]
fn install_plan_stale_cleanup_removes_abandoned_marker_but_preserves_live_prepared_plan_staging() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let app = FontbrewApp::with_paths(paths.clone());
    let first_archive = temp.path().join("source-code-pro.zip");
    let second_archive = temp.path().join("source-code-pro-copy.zip");
    write_fixture_archive(
        &first_archive,
        &[("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf")],
    );
    write_fixture_archive(
        &second_archive,
        &[("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf")],
    );

    let first_plan = app
        .install_plan(local_archive_request(&first_archive, false))
        .expect("plan first local archive install");
    let abandoned_staging = paths.staging_dir().join("install-abandoned");
    fs::create_dir_all(&abandoned_staging).expect("create abandoned staging");
    fs::write(abandoned_staging.join(".fontbrew-active"), b"active\n")
        .expect("write abandoned old active marker");

    let second_plan = app
        .install_plan(local_archive_request(&second_archive, false))
        .expect("planning second install should clean abandoned staging");

    assert!(!abandoned_staging.exists());
    assert_eq!(
        staging_entries(&paths)
            .into_iter()
            .filter(|entry| entry.starts_with("install-"))
            .count(),
        2
    );

    app.discard_install_plan(second_plan);
    app.apply_install(
        first_plan,
        ExecutionPolicy::SafeOnly,
        &mut NoProgress,
        &NeverCancelled,
    )
    .expect("first plan should remain applicable after abandoned cleanup");
    assert!(paths
        .package_store_dir(
            &package_id("source-code-pro"),
            &PackageVersion::new("local"),
        )
        .join("files/SourceCodePro-Regular.ttf")
        .exists());
}

#[test]
fn local_archive_install_list_info_remove_round_trip() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let app = FontbrewApp::with_paths(paths.clone());
    let archive_path = temp.path().join("source-code-pro.zip");
    write_fixture_archive(
        &archive_path,
        &[("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf")],
    );

    let plan = app
        .install_plan(local_archive_request(&archive_path, false))
        .expect("plan local archive install");

    assert_eq!(plan.package_id, package_id("source-code-pro"));
    assert_eq!(
        plan.target_version
            .as_ref()
            .expect("local archive target version")
            .as_str(),
        "local"
    );
    assert!(!plan.already_installed);
    assert!(!plan.changes.is_empty());
    assert!(plan.risks.is_empty());

    let mut progress = NoProgress;
    let cancellation = NeverCancelled;
    let report = app
        .apply_install(
            plan,
            ExecutionPolicy::SafeOnly,
            &mut progress,
            &cancellation,
        )
        .expect("install local archive");

    assert_eq!(report.package_id, package_id("source-code-pro"));
    assert_eq!(report.installed_version.as_str(), "local");
    assert_eq!(report.families[0].as_str(), "Source Code Pro");
    assert!(report.installed);
    assert!(!report.already_installed);
    assert!(report.activated);

    let managed_font_path = paths
        .package_store_dir(&package_id("source-code-pro"), &report.installed_version)
        .join("files/SourceCodePro-Regular.ttf");
    let activation_path = paths.activation_dir().join("SourceCodePro-Regular.ttf");
    assert!(managed_font_path.exists());
    assert_eq!(
        fs::read_link(&activation_path).expect("activation symlink"),
        managed_font_path
    );

    let list = app.list_packages().expect("list packages");
    assert_eq!(list.packages.len(), 1);
    assert_eq!(list.packages[0].package_id, package_id("source-code-pro"));
    assert_eq!(list.packages[0].families[0].as_str(), "Source Code Pro");
    assert!(list.packages[0].activated);

    let info = app
        .package_info(InfoRequest {
            package_id: package_id("source-code-pro"),
        })
        .expect("package info");
    assert_eq!(info.package.package_id, package_id("source-code-pro"));
    assert_eq!(info.package.version.as_str(), "local");
    assert_eq!(info.package.families[0].as_str(), "Source Code Pro");
    assert!(info.package.source.starts_with("local archive:"));
    assert!(info.package.source.contains("source-code-pro.zip"));
    assert_eq!(info.package.update_source, None);
    assert!(info.package.activated);

    let remove_plan = app
        .remove_plan(RemoveRequest {
            package_id: package_id("source-code-pro"),
        })
        .expect("plan remove");
    assert!(!remove_plan.changes.is_empty());
    assert!(remove_plan.risks.is_empty());

    let remove_report = app
        .apply_remove(
            remove_plan,
            ExecutionPolicy::SafeOnly,
            &mut progress,
            &cancellation,
        )
        .expect("remove package");
    assert!(remove_report.removed);
    assert!(!activation_path.exists());
    assert!(!managed_font_path.exists());
    assert!(app
        .list_packages()
        .expect("list after remove")
        .packages
        .is_empty());
}

#[test]
fn local_archive_package_id_override_installs_non_normalizable_family_and_records_metadata() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let app = FontbrewApp::with_paths(paths.clone());
    let archive_path = temp.path().join("unsafe-family.zip");
    let unsafe_family_font = font_bytes_with_unsafe_family_name("SourceCodePro-Regular.ttf");
    write_archive_entry_bytes(&archive_path, "UnsafeFamily.ttf", &unsafe_family_font);

    let error = app
        .install_plan(local_archive_request(&archive_path, false))
        .expect_err("unsafe family should not normalize into a package id");

    assert!(matches!(error, FontbrewError::InvalidPackageId { .. }));
    assert!(format!("{error}").contains("Source/Code Pro"));
    assert!(staging_entries(&paths).is_empty());

    let plan = app
        .install_plan(local_archive_request_with_package_id_override(
            &archive_path,
            package_id("custom-local"),
        ))
        .expect("override should name the local package");

    assert_eq!(plan.package_id, package_id("custom-local"));

    let mut progress = NoProgress;
    let cancellation = NeverCancelled;
    let report = app
        .apply_install(
            plan,
            ExecutionPolicy::SafeOnly,
            &mut progress,
            &cancellation,
        )
        .expect("install local archive with override");

    assert_eq!(report.package_id, package_id("custom-local"));
    assert_eq!(report.families[0].as_str(), "Source/Code Pro");

    let manifest = ManifestStore::read_or_empty(&paths.manifest_path()).expect("read manifest");
    let record = manifest
        .get_package(&package_id("custom-local"))
        .expect("override package should be recorded");
    assert_eq!(record.package_id, package_id("custom-local"));
    assert_eq!(record.families[0].as_str(), "Source/Code Pro");
}

#[test]
fn direct_local_archive_requires_family_selection_for_multiple_families() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let app = FontbrewApp::with_paths(paths.clone());
    let archive_path = temp.path().join("mixed-families.zip");
    write_fixture_archive(
        &archive_path,
        &[
            ("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf"),
            ("Inter-Variable.ttf", "Inter-Variable.ttf"),
        ],
    );

    let error = app
        .install_plan(local_archive_request(&archive_path, false))
        .expect_err("multi-family local archive should require an explicit boundary");

    assert!(matches!(
        error,
        FontbrewError::FamilySelectionRequired { .. }
    ));
    let message = error.to_string();
    assert!(message.contains("multiple font families"));
    assert!(message.contains("Source Code Pro"));
    assert!(message.contains("Inter"));
    assert!(!paths.manifest_path().exists());
    assert!(staging_entries(&paths).is_empty());
}

#[test]
fn local_archive_package_id_override_does_not_bypass_multiple_family_boundary() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let app = FontbrewApp::with_paths(paths.clone());
    let archive_path = temp.path().join("mixed-families.zip");
    write_fixture_archive(
        &archive_path,
        &[
            ("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf"),
            ("Inter-Variable.ttf", "Inter-Variable.ttf"),
        ],
    );

    let error = app
        .install_plan(local_archive_request_with_package_id_override(
            &archive_path,
            package_id("custom-local"),
        ))
        .expect_err("override should not define a family boundary");

    assert!(matches!(
        error,
        FontbrewError::FamilySelectionRequired { .. }
    ));
    let message = error.to_string();
    assert!(message.contains("multiple font families"));
    assert!(message.contains("Source Code Pro"));
    assert!(message.contains("Inter"));
    assert!(!paths.manifest_path().exists());
    assert!(staging_entries(&paths).is_empty());
}

#[test]
fn direct_local_archive_selected_family_installs_one_package() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let app = FontbrewApp::with_paths(paths.clone());
    let archive_path = temp.path().join("mixed-families.zip");
    write_fixture_archive(
        &archive_path,
        &[
            ("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf"),
            ("Inter-Variable.ttf", "Inter-Variable.ttf"),
        ],
    );

    let plan = app
        .install_plan(local_archive_request_with_selected_families(
            &archive_path,
            vec!["Inter"],
        ))
        .expect("selected family should plan");

    assert_eq!(plan.package_id, package_id("inter"));

    let report = app
        .apply_install(
            plan,
            ExecutionPolicy::SafeOnly,
            &mut NoProgress,
            &NeverCancelled,
        )
        .expect("selected family should install");

    assert_eq!(report.package_id, package_id("inter"));
    assert_eq!(report.families, vec![FamilyName::new("Inter")]);
    let manifest = ManifestStore::read_or_empty(&paths.manifest_path()).expect("read manifest");
    assert!(manifest.get_package(&package_id("inter")).is_some());
    assert!(manifest
        .get_package(&package_id("source-code-pro"))
        .is_none());
    assert!(!paths
        .package_store_dir(&package_id("inter"), &PackageVersion::new("local"))
        .join("files/SourceCodePro-Regular.ttf")
        .exists());
}

#[test]
fn package_id_override_is_rejected_for_non_local_sources() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let app = FontbrewApp::with_paths(paths.clone());

    for request in [
        InstallRequest {
            source: InstallSource::RegistryName("source-code-pro".to_string()),
            package_id_override: Some(package_id("custom-local")),
            format_preference: Vec::new(),
            asset_selector: None,
            selected_families: Vec::new(),
            reinstall: false,
        },
        InstallRequest {
            source: InstallSource::GitHubRepo {
                owner: "adobe".to_string(),
                repo: "source-code-pro".to_string(),
            },
            package_id_override: Some(package_id("custom-local")),
            format_preference: Vec::new(),
            asset_selector: None,
            selected_families: Vec::new(),
            reinstall: false,
        },
        InstallRequest {
            source: InstallSource::Provider {
                provider: ProviderKind::Fontsource,
                id: "source-code-pro".to_string(),
            },
            package_id_override: Some(package_id("custom-local")),
            format_preference: Vec::new(),
            asset_selector: None,
            selected_families: Vec::new(),
            reinstall: false,
        },
        InstallRequest {
            source: InstallSource::Provider {
                provider: ProviderKind::Google,
                id: "source-sans-3".to_string(),
            },
            package_id_override: Some(package_id("custom-local")),
            format_preference: Vec::new(),
            asset_selector: None,
            selected_families: Vec::new(),
            reinstall: false,
        },
    ] {
        let error = app
            .install_plan(request)
            .expect_err("override should be local-only");

        assert!(matches!(error, FontbrewError::Config { .. }));
        assert!(error.to_string().contains("--id"));
        assert!(error.to_string().contains("local archive"));
    }

    assert!(!paths.manifest_path().exists());
    assert!(staging_entries(&paths).is_empty());
}

#[test]
fn remove_cancellation_after_mutation_starts_finishes_remove_transaction() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let app = FontbrewApp::with_paths(paths.clone());
    let archive_path = temp.path().join("source-code-pro.zip");
    write_fixture_archive(
        &archive_path,
        &[("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf")],
    );
    apply_install(&app, &archive_path).expect("install local archive");
    let package_store_dir = paths.package_store_dir(
        &package_id("source-code-pro"),
        &PackageVersion::new("local"),
    );
    let activation_path = paths.activation_dir().join("SourceCodePro-Regular.ttf");
    assert!(package_store_dir.exists());
    assert!(activation_path.exists());

    let remove_plan = app
        .remove_plan(RemoveRequest {
            package_id: package_id("source-code-pro"),
        })
        .expect("plan remove");
    let report = app
        .apply_remove(
            remove_plan,
            ExecutionPolicy::SafeOnly,
            &mut NoProgress,
            &CancelOnCheck::new(4),
        )
        .expect("remove should finish once mutation has started");

    assert!(report.removed);
    assert!(!activation_path.exists());
    assert!(!package_store_dir.exists());
    assert!(ManifestStore::read_or_empty(&paths.manifest_path())
        .expect("read manifest")
        .get_package(&package_id("source-code-pro"))
        .is_none());
}

#[test]
fn install_plan_rejects_reserved_copy_activation_config_and_cleans_staging() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let app = FontbrewApp::with_paths(paths.clone());
    let archive_path = temp.path().join("source-code-pro.zip");
    write_fixture_archive(
        &archive_path,
        &[("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf")],
    );
    let config_path = paths.config_path();
    fs::create_dir_all(config_path.parent().expect("config parent")).expect("create config dir");
    fs::write(
        &config_path,
        r#"
schema_version = 1

[install]
activation_strategy = "copy"
"#,
    )
    .expect("write copy activation config");

    let error = app
        .install_plan(local_archive_request(&archive_path, false))
        .expect_err("copy activation config should be rejected");

    assert!(matches!(error, FontbrewError::Config { .. }));
    let message = error.to_string();
    assert!(message.contains("copy activation"));
    assert!(message.contains("reserved"));
    assert!(message.contains("not supported"));
    assert!(!paths
        .package_store_dir(
            &package_id("source-code-pro"),
            &PackageVersion::new("local"),
        )
        .exists());
    assert!(!paths.manifest_path().exists());
    assert!(staging_entries(&paths).is_empty());
}

#[test]
fn local_archive_install_uses_global_format_preference_when_formats_have_equivalent_coverage() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let app = FontbrewApp::with_paths(paths.clone());
    let archive_path = temp.path().join("source-code-pro.zip");
    write_fixture_archive(
        &archive_path,
        &[
            ("SourceCodePro-Regular.otf", "SourceCodePro-Regular.otf"),
            ("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf"),
        ],
    );
    let config_path = paths.config_path();
    fs::create_dir_all(config_path.parent().expect("config parent")).expect("create config dir");
    fs::write(
        &config_path,
        r#"
schema_version = 1

[install]
format_preference = ["ttf", "otf"]
"#,
    )
    .expect("write config");

    apply_install(&app, &archive_path).expect("install preferred ttf");

    let package_dir = paths.package_store_dir(
        &package_id("source-code-pro"),
        &PackageVersion::new("local"),
    );
    assert!(package_dir.join("files/SourceCodePro-Regular.ttf").exists());
    assert!(!package_dir.join("files/SourceCodePro-Regular.otf").exists());
}

#[test]
fn local_archive_request_format_preference_overrides_global_config() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let app = FontbrewApp::with_paths(paths.clone());
    let archive_path = temp.path().join("source-code-pro.zip");
    write_fixture_archive(
        &archive_path,
        &[
            ("SourceCodePro-Regular.otf", "SourceCodePro-Regular.otf"),
            ("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf"),
        ],
    );
    let config_path = paths.config_path();
    fs::create_dir_all(config_path.parent().expect("config parent")).expect("create config dir");
    fs::write(
        &config_path,
        r#"
schema_version = 1

[install]
format_preference = ["ttf", "otf"]
"#,
    )
    .expect("write config");

    let plan = app
        .install_plan(local_archive_request_with_formats(
            &archive_path,
            false,
            vec![FontFormat::Otf],
        ))
        .expect("plan otf override");
    let mut progress = NoProgress;
    let cancellation = NeverCancelled;
    app.apply_install(
        plan,
        ExecutionPolicy::SafeOnly,
        &mut progress,
        &cancellation,
    )
    .expect("install preferred otf");

    let package_dir = paths.package_store_dir(
        &package_id("source-code-pro"),
        &PackageVersion::new("local"),
    );
    assert!(package_dir.join("files/SourceCodePro-Regular.otf").exists());
    assert!(!package_dir.join("files/SourceCodePro-Regular.ttf").exists());
}

#[test]
fn local_archive_install_rejects_implicit_format_coverage_mismatch() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let app = FontbrewApp::with_paths(paths.clone());
    let archive_path = temp.path().join("source-code-pro.zip");
    write_fixture_archive(
        &archive_path,
        &[
            ("SourceCodePro-Regular.otf", "SourceCodePro-Regular.otf"),
            ("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf"),
            ("SourceCodePro-Bold.ttf", "SourceCodePro-Bold.ttf"),
        ],
    );

    let error = app
        .install_plan(local_archive_request(&archive_path, false))
        .expect_err("format coverage mismatch should fail conservatively");

    assert!(matches!(error, FontbrewError::Conflict { .. }));
    assert!(format!("{error}").contains("format coverage differs"));
    assert!(!paths.manifest_path().exists());
}

#[test]
fn local_archive_explicit_format_preference_resolves_coverage_mismatch() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let app = FontbrewApp::with_paths(paths.clone());
    let archive_path = temp.path().join("source-code-pro.zip");
    write_fixture_archive(
        &archive_path,
        &[
            ("SourceCodePro-Regular.otf", "SourceCodePro-Regular.otf"),
            ("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf"),
            ("SourceCodePro-Bold.ttf", "SourceCodePro-Bold.ttf"),
        ],
    );

    let plan = app
        .install_plan(local_archive_request_with_formats(
            &archive_path,
            false,
            vec![FontFormat::Otf],
        ))
        .expect("explicit format preference should select requested format");
    let mut progress = NoProgress;
    let cancellation = NeverCancelled;
    app.apply_install(
        plan,
        ExecutionPolicy::SafeOnly,
        &mut progress,
        &cancellation,
    )
    .expect("install requested otf subset");

    let package_dir = paths.package_store_dir(
        &package_id("source-code-pro"),
        &PackageVersion::new("local"),
    );
    assert!(package_dir.join("files/SourceCodePro-Regular.otf").exists());
    assert!(!package_dir.join("files/SourceCodePro-Regular.ttf").exists());
    assert!(!package_dir.join("files/SourceCodePro-Bold.ttf").exists());
}

#[test]
fn local_archive_explicit_unavailable_format_fails_without_fallback() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let app = FontbrewApp::with_paths(paths.clone());
    let archive_path = temp.path().join("source-code-pro.zip");
    write_fixture_archive(
        &archive_path,
        &[("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf")],
    );

    let error = app
        .install_plan(local_archive_request_with_formats(
            &archive_path,
            false,
            vec![FontFormat::Otf],
        ))
        .expect_err("explicit unavailable format should not fall back silently");

    assert!(matches!(error, FontbrewError::Conflict { .. }));
    assert!(format!("{error}").contains("requested font formats are not available"));
    assert!(!paths.manifest_path().exists());
}

#[cfg(unix)]
#[test]
fn install_rejects_package_store_symlink_without_writing_outside_managed_root() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let app = FontbrewApp::with_paths(paths.clone());
    let archive_path = temp.path().join("source-code-pro.zip");
    write_fixture_archive(
        &archive_path,
        &[("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf")],
    );
    let outside_packages = temp.path().join("outside-packages");
    fs::create_dir_all(&outside_packages).expect("create outside packages");
    fs::create_dir_all(paths.managed_store_dir()).expect("create managed root");
    std::os::unix::fs::symlink(
        &outside_packages,
        paths.managed_store_dir().join("packages"),
    )
    .expect("create packages symlink");

    let plan = app
        .install_plan(local_archive_request(&archive_path, false))
        .expect("plan local archive install");
    let mut progress = NoProgress;
    let cancellation = NeverCancelled;
    let error = app
        .apply_install(
            plan,
            ExecutionPolicy::SafeOnly,
            &mut progress,
            &cancellation,
        )
        .expect_err("package store symlink should reject");

    assert!(matches!(error, FontbrewError::PathResolution { .. }));
    assert!(!outside_packages
        .join("source-code-pro/local/files/SourceCodePro-Regular.ttf")
        .exists());
    assert!(!paths.manifest_path().exists());
}

#[test]
fn remove_rejects_malformed_manifest_version_without_deleting_outside_directory() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let app = FontbrewApp::with_paths(paths.clone());
    let outside_dir = paths.managed_store_dir().join("outside");
    fs::create_dir_all(paths.managed_store_dir().join("packages/source-code-pro"))
        .expect("create traversal prefix");
    fs::create_dir_all(&outside_dir).expect("create outside dir");
    fs::write(outside_dir.join("keep.txt"), b"keep").expect("write outside marker");

    let mut manifest = ManifestV1::empty();
    manifest
        .insert_package(ManifestPackageRecord {
            package_id: package_id("source-code-pro"),
            version: PackageVersion::new("../../outside"),
            source: ManifestSource::LocalArchive {
                path: temp.path().join("source-code-pro.zip"),
            },
            update_source: None,
            families: Vec::new(),
            font_files: Vec::new(),
            activation_artifacts: Vec::new(),
            installed_at: "unix:0".to_string(),
            active_version: Some(PackageVersion::new("../../outside")),
        })
        .expect("insert malformed package record");
    ManifestStore::write(&paths.manifest_path(), &manifest).expect("write malformed manifest");

    let error = app
        .apply_remove(
            RemovePlan {
                package_id: package_id("source-code-pro"),
                changes: Vec::new(),
                risks: Vec::new(),
                font_files: Vec::new(),
                activation_artifacts: Vec::new(),
            },
            ExecutionPolicy::SafeOnly,
            &mut NoProgress,
            &NeverCancelled,
        )
        .expect_err("malformed manifest version should reject");

    assert!(matches!(
        error,
        FontbrewError::Manifest { .. } | FontbrewError::PathResolution { .. }
    ));
    assert_eq!(
        fs::read(outside_dir.join("keep.txt")).expect("outside marker remains"),
        b"keep"
    );
}

#[cfg(unix)]
#[test]
fn failed_reinstall_preserves_existing_activation_and_package_store() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let app = FontbrewApp::with_paths(paths.clone());
    let archive_path = temp.path().join("source-code-pro.zip");
    write_fixture_archive(
        &archive_path,
        &[("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf")],
    );
    apply_install(&app, &archive_path).expect("initial install");

    let managed_font_path = paths
        .package_store_dir(
            &package_id("source-code-pro"),
            &PackageVersion::new("local"),
        )
        .join("files/SourceCodePro-Regular.ttf");
    let activation_path = paths.activation_dir().join("SourceCodePro-Regular.ttf");
    let original_managed_bytes =
        fs::read(&managed_font_path).expect("read original managed font bytes");
    fs::write(paths.managed_store_dir().join(".fontbrew.lock"), b"lock")
        .expect("precreate lock file");

    let reinstall_plan = app
        .install_plan(local_archive_request(&archive_path, true))
        .expect("plan reinstall");
    let original_permissions = fs::metadata(paths.managed_store_dir())
        .expect("managed root metadata")
        .permissions();
    let mut read_only_permissions = original_permissions.clone();
    read_only_permissions.set_mode(0o555);
    fs::set_permissions(paths.managed_store_dir(), read_only_permissions)
        .expect("make managed root read-only");

    let mut progress = NoProgress;
    let cancellation = NeverCancelled;
    let result = app.apply_install(
        reinstall_plan,
        ExecutionPolicy::SafeOnly,
        &mut progress,
        &cancellation,
    );

    fs::set_permissions(paths.managed_store_dir(), original_permissions)
        .expect("restore managed root permissions");

    let error = result.expect_err("manifest write should fail");
    assert!(matches!(error, FontbrewError::Io(_)));
    assert_eq!(
        fs::read_link(&activation_path).expect("existing activation symlink remains"),
        managed_font_path
    );
    assert_eq!(
        fs::read(&managed_font_path).expect("existing managed font remains"),
        original_managed_bytes
    );
}

#[test]
fn repeated_local_archive_install_without_reinstall_is_noop() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let app = FontbrewApp::with_paths(paths.clone());
    let archive_path = temp.path().join("source-code-pro.zip");
    write_fixture_archive(
        &archive_path,
        &[("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf")],
    );
    apply_install(&app, &archive_path).expect("initial install");

    let managed_font_path = paths
        .package_store_dir(
            &package_id("source-code-pro"),
            &fontbrew_core::PackageVersion::new("local"),
        )
        .join("files/SourceCodePro-Regular.ttf");
    fs::write(&managed_font_path, b"sentinel managed bytes").expect("mark managed file");

    let plan = app
        .install_plan(local_archive_request(&archive_path, false))
        .expect("plan repeated install");
    assert!(plan.already_installed);
    assert!(plan.changes.is_empty());
    assert!(plan.risks.is_empty());

    let mut progress = NoProgress;
    let cancellation = NeverCancelled;
    let report = app
        .apply_install(
            plan,
            ExecutionPolicy::SafeOnly,
            &mut progress,
            &cancellation,
        )
        .expect("apply repeated install");

    assert!(!report.installed);
    assert!(report.already_installed);
    assert_eq!(
        fs::read(&managed_font_path).expect("managed file should not be rewritten"),
        b"sentinel managed bytes"
    );
}

#[test]
fn remove_keeps_unmanaged_files_and_registry_config_provider_metadata() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let app = FontbrewApp::with_paths(paths.clone());
    let archive_path = temp.path().join("source-code-pro.zip");
    write_fixture_archive(
        &archive_path,
        &[("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf")],
    );
    apply_install(&app, &archive_path).expect("initial install");

    let unmanaged_activation_file = paths.activation_dir().join("Unmanaged.ttf");
    fs::write(&unmanaged_activation_file, b"unmanaged").expect("write unmanaged activation file");
    fs::write(paths.registry_snapshot_path(), b"registry").expect("write registry metadata");
    fs::create_dir_all(paths.provider_metadata_dir()).expect("create provider metadata dir");
    let provider_metadata_file = paths.provider_metadata_dir().join("google.json");
    fs::write(&provider_metadata_file, b"provider").expect("write provider metadata");
    let config_path = paths.config_path();
    fs::create_dir_all(config_path.parent().expect("config parent")).expect("create config dir");
    fs::write(&config_path, b"schema_version = 1\n").expect("write config");

    let remove_plan = app
        .remove_plan(RemoveRequest {
            package_id: package_id("source-code-pro"),
        })
        .expect("plan remove");
    let mut progress = NoProgress;
    let cancellation = NeverCancelled;
    app.apply_remove(
        remove_plan,
        ExecutionPolicy::SafeOnly,
        &mut progress,
        &cancellation,
    )
    .expect("remove package");

    assert_eq!(
        fs::read(&unmanaged_activation_file).expect("unmanaged activation file remains"),
        b"unmanaged"
    );
    assert_eq!(
        fs::read(paths.registry_snapshot_path()).expect("registry metadata remains"),
        b"registry"
    );
    assert_eq!(
        fs::read(provider_metadata_file).expect("provider metadata remains"),
        b"provider"
    );
    assert_eq!(
        fs::read(config_path).expect("config remains"),
        b"schema_version = 1\n"
    );
}

#[test]
fn failed_local_archive_install_does_not_update_manifest() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let app = FontbrewApp::with_paths(paths.clone());
    let archive_path = temp.path().join("broken.zip");
    write_invalid_font_archive(&archive_path);

    let error = app
        .install_plan(local_archive_request(&archive_path, false))
        .expect_err("invalid font archive should fail during planning");

    assert!(matches!(error, FontbrewError::FontParse { .. }));
    assert!(!paths.manifest_path().exists());
    assert!(app
        .list_packages()
        .expect("list after failed install")
        .packages
        .is_empty());
}

#[test]
fn activation_conflict_blocks_install_without_manifest_update() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let app = FontbrewApp::with_paths(paths.clone());
    let archive_path = temp.path().join("source-code-pro.zip");
    write_fixture_archive(
        &archive_path,
        &[("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf")],
    );
    fs::create_dir_all(paths.activation_dir()).expect("create activation dir");
    let unmanaged_activation_file = paths.activation_dir().join("SourceCodePro-Regular.ttf");
    fs::write(&unmanaged_activation_file, b"unmanaged").expect("write unmanaged conflict");

    let plan = app
        .install_plan(local_archive_request(&archive_path, false))
        .expect("plan install with activation conflict");

    assert_eq!(plan.risks.len(), 1);

    let mut progress = NoProgress;
    let cancellation = NeverCancelled;
    let error = app
        .apply_install(
            plan,
            ExecutionPolicy::SafeOnly,
            &mut progress,
            &cancellation,
        )
        .expect_err("safe policy should reject activation conflict");

    assert!(matches!(
        error,
        FontbrewError::ExecutionPolicyRequired { .. }
    ));
    assert_eq!(
        fs::read(&unmanaged_activation_file).expect("unmanaged conflict remains"),
        b"unmanaged"
    );
    assert!(!paths.manifest_path().exists());
}

#[test]
fn install_plan_reports_unmanaged_same_family_overlap_without_mutating_under_safe_policy() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let app = FontbrewApp::with_paths(paths.clone());
    let archive_path = temp.path().join("source-code-pro.zip");
    write_fixture_archive(
        &archive_path,
        &[("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf")],
    );

    let user_fonts_dir = paths
        .activation_dir()
        .parent()
        .expect("activation dir has parent")
        .to_path_buf();
    fs::create_dir_all(&user_fonts_dir).expect("create user font dir");
    let unmanaged_font = user_fonts_dir.join("ManualSourceCodePro.ttf");
    fs::copy(
        fixture_font_path("SourceCodePro-Regular.ttf"),
        &unmanaged_font,
    )
    .expect("write unmanaged same-family font");

    let plan = app
        .install_plan(local_archive_request(&archive_path, false))
        .expect("plan install with unmanaged family overlap");

    assert!(plan.risks.iter().any(|risk| matches!(
        risk,
        PlanRisk::UnmanagedFontOverlap {
            family_name,
            description
        } if family_name.as_str() == "Source Code Pro"
            && description.contains("ManualSourceCodePro.ttf")
    )));

    let error = app
        .apply_install(
            plan,
            ExecutionPolicy::SafeOnly,
            &mut NoProgress,
            &NeverCancelled,
        )
        .expect_err("safe policy should reject same-family unmanaged overlap");

    assert!(matches!(
        error,
        FontbrewError::ExecutionPolicyRequired { .. }
    ));
    assert!(unmanaged_font.exists());
    assert!(!paths.manifest_path().exists());
    assert!(!paths
        .package_store_dir(
            &package_id("source-code-pro"),
            &PackageVersion::new("local"),
        )
        .exists());
}

#[cfg(unix)]
#[test]
fn install_plan_reports_activation_artifact_managed_by_another_package() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let app = FontbrewApp::with_paths(paths.clone());
    let archive_path = temp.path().join("source-code-pro.zip");
    write_fixture_archive(
        &archive_path,
        &[("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf")],
    );

    let other_package_id = package_id("other-package");
    let other_version = PackageVersion::new("1.0.0");
    let other_source_path = paths
        .package_store_dir(&other_package_id, &other_version)
        .join("files/SourceCodePro-Regular.ttf");
    fs::create_dir_all(other_source_path.parent().expect("other source parent"))
        .expect("create other package store");
    fs::copy(
        fixture_font_path("SourceCodePro-Regular.ttf"),
        &other_source_path,
    )
    .expect("write other managed font");

    fs::create_dir_all(paths.activation_dir()).expect("create activation dir");
    let shared_activation_path = paths.activation_dir().join("SourceCodePro-Regular.ttf");
    std::os::unix::fs::symlink(&other_source_path, &shared_activation_path)
        .expect("create other package activation");

    let mut manifest = ManifestV1::empty();
    manifest
        .insert_package(ManifestPackageRecord {
            package_id: other_package_id.clone(),
            version: other_version.clone(),
            source: ManifestSource::LocalArchive {
                path: temp.path().join("other.zip"),
            },
            update_source: None,
            families: vec![FamilyName::new("Source Code Pro")],
            font_files: Vec::new(),
            activation_artifacts: vec![fontbrew_core::manifest::ManifestActivationArtifactRecord {
                path: shared_activation_path.clone(),
                source_path: other_source_path.clone(),
                strategy: ActivationStrategy::Symlink,
            }],
            installed_at: "unix:1".to_string(),
            active_version: Some(other_version),
        })
        .expect("insert other package record");
    ManifestStore::write(&paths.manifest_path(), &manifest).expect("write manifest");

    let plan = app
        .install_plan(local_archive_request(&archive_path, false))
        .expect("plan install with other managed activation conflict");

    assert!(plan.risks.iter().any(|risk| matches!(
        risk,
        PlanRisk::Conflict {
            package_id: risk_package_id,
            description
        } if risk_package_id == &package_id("source-code-pro")
            && description.contains("other-package")
            && description.contains("SourceCodePro-Regular.ttf")
    )));
}

#[test]
fn install_plan_reports_already_managed_package_from_different_source() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let app = FontbrewApp::with_paths(paths.clone());
    let first_archive = temp.path().join("source-code-pro-first.zip");
    let second_archive = temp.path().join("source-code-pro-second.zip");
    write_fixture_archive(
        &first_archive,
        &[("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf")],
    );
    write_fixture_archive(
        &second_archive,
        &[("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf")],
    );
    apply_install(&app, &first_archive).expect("initial install");

    let plan = app
        .install_plan(local_archive_request(&second_archive, false))
        .expect("plan same package from different source");

    assert!(plan.risks.iter().any(|risk| matches!(
        risk,
        PlanRisk::Conflict {
            package_id: risk_package_id,
            description
        } if risk_package_id == &package_id("source-code-pro")
            && description.contains("different source")
            && description.contains("source-code-pro-first.zip")
            && description.contains("source-code-pro-second.zip")
    )));

    let error = app
        .apply_install(
            plan,
            ExecutionPolicy::SafeOnly,
            &mut NoProgress,
            &NeverCancelled,
        )
        .expect_err("safe policy should reject source conflict");

    assert!(matches!(
        error,
        FontbrewError::ExecutionPolicyRequired { .. }
    ));
}

#[test]
fn reinstall_from_different_source_does_not_adopt_source_even_with_approved_risk() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let app = FontbrewApp::with_paths(paths.clone());
    let first_archive = temp.path().join("source-code-pro-first.zip");
    let second_archive = temp.path().join("source-code-pro-second.zip");
    write_fixture_archive(
        &first_archive,
        &[("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf")],
    );
    write_fixture_archive(
        &second_archive,
        &[("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf")],
    );
    apply_install(&app, &first_archive).expect("initial install");

    let plan = app
        .install_plan(local_archive_request(&second_archive, true))
        .expect("plan reinstall from different source");
    assert!(!plan.risks.is_empty());

    let error = app
        .apply_install(
            plan,
            ExecutionPolicy::AssumeYes,
            &mut NoProgress,
            &NeverCancelled,
        )
        .expect_err("approved risk should not silently adopt a different source");

    assert!(matches!(error, FontbrewError::Conflict { .. }));
    let info = app
        .package_info(InfoRequest {
            package_id: package_id("source-code-pro"),
        })
        .expect("package info after rejected reinstall");
    assert!(info.package.source.contains("source-code-pro-first.zip"));
    assert!(!info.package.source.contains("source-code-pro-second.zip"));
}

#[cfg(unix)]
#[test]
fn install_plan_reports_unmanaged_same_family_symlink_overlap() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let app = FontbrewApp::with_paths(paths.clone());
    let archive_path = temp.path().join("source-code-pro.zip");
    write_fixture_archive(
        &archive_path,
        &[("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf")],
    );

    let user_fonts_dir = paths
        .activation_dir()
        .parent()
        .expect("activation dir has parent")
        .to_path_buf();
    let symlink_target_dir = temp.path().join("manual-font-targets");
    let symlink_target = symlink_target_dir.join("SourceCodePro-Regular.ttf");
    fs::create_dir_all(&user_fonts_dir).expect("create user font dir");
    fs::create_dir_all(&symlink_target_dir).expect("create symlink target dir");
    fs::copy(
        fixture_font_path("SourceCodePro-Regular.ttf"),
        &symlink_target,
    )
    .expect("write symlink target font");
    let unmanaged_symlink = user_fonts_dir.join("ManualSourceCodePro.ttf");
    std::os::unix::fs::symlink(&symlink_target, &unmanaged_symlink)
        .expect("create unmanaged font symlink");

    let plan = app
        .install_plan(local_archive_request(&archive_path, false))
        .expect("plan install with unmanaged symlink family overlap");

    assert!(plan.risks.iter().any(|risk| matches!(
        risk,
        PlanRisk::UnmanagedFontOverlap {
            family_name,
            description
        } if family_name.as_str() == "Source Code Pro"
            && description.contains("ManualSourceCodePro.ttf")
    )));
}

#[test]
fn stale_install_plan_rechecks_same_family_overlap_before_mutation() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let app = FontbrewApp::with_paths(paths.clone());
    let archive_path = temp.path().join("source-code-pro.zip");
    write_fixture_archive(
        &archive_path,
        &[("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf")],
    );

    let plan = app
        .install_plan(local_archive_request(&archive_path, false))
        .expect("plan install before unmanaged font appears");
    assert!(plan.risks.is_empty());

    let user_fonts_dir = paths
        .activation_dir()
        .parent()
        .expect("activation dir has parent")
        .to_path_buf();
    fs::create_dir_all(&user_fonts_dir).expect("create user font dir");
    let unmanaged_font = user_fonts_dir.join("ManualSourceCodePro.ttf");
    fs::copy(
        fixture_font_path("SourceCodePro-Regular.ttf"),
        &unmanaged_font,
    )
    .expect("write unmanaged same-family font after planning");

    let error = app
        .apply_install(
            plan,
            ExecutionPolicy::SafeOnly,
            &mut NoProgress,
            &NeverCancelled,
        )
        .expect_err("stale plan should recheck same-family overlap");

    assert!(matches!(
        error,
        FontbrewError::ExecutionPolicyRequired { .. }
    ));
    assert!(unmanaged_font.exists());
    assert!(!paths.manifest_path().exists());
    assert!(!paths
        .package_store_dir(
            &package_id("source-code-pro"),
            &PackageVersion::new("local"),
        )
        .exists());
}

#[cfg(unix)]
#[test]
fn stale_install_plan_rechecks_managed_activation_conflict_before_copying() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let app = FontbrewApp::with_paths(paths.clone());
    let archive_path = temp.path().join("source-code-pro.zip");
    write_fixture_archive(
        &archive_path,
        &[("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf")],
    );

    let plan = app
        .install_plan(local_archive_request(&archive_path, false))
        .expect("plan install before managed conflict appears");
    assert!(plan.risks.is_empty());

    let other_package_id = package_id("other-package");
    let other_version = PackageVersion::new("1.0.0");
    let other_source_path = paths
        .package_store_dir(&other_package_id, &other_version)
        .join("files/SourceCodePro-Regular.ttf");
    fs::create_dir_all(other_source_path.parent().expect("other source parent"))
        .expect("create other package store");
    fs::copy(
        fixture_font_path("SourceCodePro-Regular.ttf"),
        &other_source_path,
    )
    .expect("write other managed font");
    fs::create_dir_all(paths.activation_dir()).expect("create activation dir");
    let shared_activation_path = paths.activation_dir().join("SourceCodePro-Regular.ttf");
    std::os::unix::fs::symlink(&other_source_path, &shared_activation_path)
        .expect("create other package activation");

    let mut manifest = ManifestV1::empty();
    manifest
        .insert_package(ManifestPackageRecord {
            package_id: other_package_id.clone(),
            version: other_version.clone(),
            source: ManifestSource::LocalArchive {
                path: temp.path().join("other.zip"),
            },
            update_source: None,
            families: vec![FamilyName::new("Source Code Pro")],
            font_files: Vec::new(),
            activation_artifacts: vec![fontbrew_core::manifest::ManifestActivationArtifactRecord {
                path: shared_activation_path.clone(),
                source_path: other_source_path.clone(),
                strategy: ActivationStrategy::Symlink,
            }],
            installed_at: "unix:1".to_string(),
            active_version: Some(other_version.clone()),
        })
        .expect("insert other package record");
    ManifestStore::write(&paths.manifest_path(), &manifest).expect("write manifest");

    let error = app
        .apply_install(
            plan,
            ExecutionPolicy::AssumeYes,
            &mut NoProgress,
            &NeverCancelled,
        )
        .expect_err("approved stale plan should not overwrite other managed activation");

    match error {
        FontbrewError::Conflict { message, .. } => {
            assert!(message.contains("already managed by package other-package"));
        }
        other => panic!("expected conflict error, got {other:?}"),
    }
    assert_eq!(
        fs::read_link(&shared_activation_path).expect("other activation remains"),
        other_source_path
    );
    assert!(ManifestStore::read_or_empty(&paths.manifest_path())
        .expect("read manifest")
        .get_package(&other_package_id)
        .is_some());
    assert!(!paths
        .package_store_dir(
            &package_id("source-code-pro"),
            &PackageVersion::new("local"),
        )
        .exists());
}

#[cfg(unix)]
#[test]
fn install_plan_scan_error_removes_staging_directory() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let app = FontbrewApp::with_paths(paths.clone());
    let archive_path = temp.path().join("source-code-pro.zip");
    write_fixture_archive(
        &archive_path,
        &[("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf")],
    );

    let user_fonts_dir = paths
        .activation_dir()
        .parent()
        .expect("activation dir has parent")
        .to_path_buf();
    fs::create_dir_all(paths.activation_dir()).expect("create activation dir");
    let original_permissions = fs::metadata(&user_fonts_dir)
        .expect("user font dir metadata")
        .permissions();
    let mut execute_only_permissions = original_permissions.clone();
    execute_only_permissions.set_mode(0o111);
    fs::set_permissions(&user_fonts_dir, execute_only_permissions)
        .expect("make user font dir unreadable");

    let result = app.install_plan(local_archive_request(&archive_path, false));

    fs::set_permissions(&user_fonts_dir, original_permissions).expect("restore user font dir");
    let error = result.expect_err("unreadable scan dir should fail planning");
    assert!(matches!(error, FontbrewError::Io(_)));
    assert!(
        !paths.staging_dir().exists() || {
            fs::read_dir(paths.staging_dir())
                .expect("read staging root")
                .next()
                .is_none()
        }
    );
}
