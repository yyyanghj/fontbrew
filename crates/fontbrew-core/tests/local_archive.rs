use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
};

use fontbrew_core::{
    manifest::{ManifestPackageRecord, ManifestSource, ManifestStore, ManifestV1},
    platform::FontbrewPaths,
    CancellationToken, ExecutionPolicy, FontbrewApp, FontbrewError, InfoRequest, InstallRequest,
    InstallSource, PackageId, PackageVersion, ProgressEvent, ProgressSink, RemovePlan,
    RemoveRequest,
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
    InstallRequest {
        source: InstallSource::LocalPath(archive_path.to_path_buf()),
        format_preference: Vec::new(),
        asset_selector: None,
        reinstall,
        refresh: false,
        offline: true,
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
fn failed_activation_rolls_back_copied_package_files() {
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
        .expect_err("copy activation is not implemented");

    assert!(matches!(error, FontbrewError::NotImplemented { .. }));
    assert!(!paths
        .package_store_dir(
            &package_id("source-code-pro"),
            &PackageVersion::new("local"),
        )
        .exists());
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
