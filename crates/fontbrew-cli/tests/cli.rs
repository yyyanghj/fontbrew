use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Output,
};

use assert_cmd::Command;
use fontbrew_core::registry::REGISTRY_URL_ENV_VAR;
use predicates::prelude::*;
use serde_json::Value;
use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

fn fontbrew(home: &Path) -> Command {
    let mut command = Command::cargo_bin("fontbrew").expect("fontbrew binary");
    command.env("HOME", home);
    command
}

fn fixture_font_path(filename: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/fonts")
        .join(filename)
}

fn write_fixture_archive(archive_path: &Path) {
    let file = File::create(archive_path).expect("create archive");
    let mut zip = ZipWriter::new(file);
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .unix_permissions(0o100644);

    zip.start_file("SourceCodePro-Regular.ttf", options)
        .expect("start archive entry");

    let mut fixture =
        File::open(fixture_font_path("SourceCodePro-Regular.ttf")).expect("open fixture font");
    let mut bytes = Vec::new();
    fixture.read_to_end(&mut bytes).expect("read fixture font");
    zip.write_all(&bytes).expect("write archive entry");
    zip.finish().expect("finish archive");
}

fn stdout_json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).expect("stdout should be parseable JSON")
}

fn stderr_text(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).expect("stderr should be utf-8")
}

fn write_registry_snapshot(path: &Path) {
    fs::write(
        path,
        r#"{
  "schemaVersion": 1,
  "updatedAt": "2026-07-03T00:00:00Z",
  "packages": {
    "inter": {
      "name": "Inter",
      "source": {
        "type": "github",
        "repo": "rsms/inter"
      },
      "families": ["Inter"],
      "asset": {
        "include": ["*Inter*.zip"],
        "exclude": ["*web*", "*.woff2"]
      }
    }
  }
}"#,
    )
    .expect("write registry snapshot fixture");
}

fn write_search_registry_snapshot(path: &Path) {
    fs::write(
        path,
        r#"{
  "schemaVersion": 1,
  "updatedAt": "2026-07-03T00:00:00Z",
  "packages": {
    "inter": {
      "name": "Inter",
      "source": {
        "type": "github",
        "repo": "rsms/inter"
      },
      "families": ["Inter"]
    },
    "source-code-pro": {
      "name": "Source Code Pro",
      "source": {
        "type": "github",
        "repo": "adobe/source-code-pro"
      },
      "families": ["Source Code Pro"]
    }
  }
}"#,
    )
    .expect("write search registry snapshot fixture");
}

#[test]
fn list_on_empty_home_prints_human_empty_state_on_stdout_only() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");

    fontbrew(&home)
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains("No managed packages installed."))
        .stderr(predicate::str::is_empty());
}

#[test]
fn install_list_info_and_remove_local_archive_in_test_home() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let archive_path = temp.path().join("source-code-pro.zip");
    write_fixture_archive(&archive_path);

    fontbrew(&home)
        .args(["--quiet", "install"])
        .arg(&archive_path)
        .assert()
        .success()
        .stdout(predicate::str::contains("Installed source-code-pro"))
        .stderr(predicate::str::is_empty());

    fontbrew(&home)
        .arg("list")
        .assert()
        .success()
        .stdout(
            predicate::str::contains("source-code-pro")
                .and(predicate::str::contains("Source Code Pro")),
        )
        .stderr(predicate::str::is_empty());

    fontbrew(&home)
        .args(["info", "source-code-pro"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Package: source-code-pro")
                .and(predicate::str::contains("Version: local"))
                .and(predicate::str::contains("Source Code Pro")),
        )
        .stderr(predicate::str::is_empty());

    fontbrew(&home)
        .args(["remove", "source-code-pro"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Removed source-code-pro."))
        .stderr(predicate::str::is_empty());

    fontbrew(&home)
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains("No managed packages installed."))
        .stderr(predicate::str::is_empty());
}

#[test]
fn uninstall_alias_removes_local_archive_package() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let archive_path = temp.path().join("source-code-pro.zip");
    write_fixture_archive(&archive_path);

    fontbrew(&home)
        .arg("install")
        .arg(&archive_path)
        .assert()
        .success();

    fontbrew(&home)
        .args(["uninstall", "source-code-pro"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Removed source-code-pro."))
        .stderr(predicate::str::is_empty());
}

#[test]
fn json_install_and_list_write_parseable_stdout_only() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let archive_path = temp.path().join("source-code-pro.zip");
    write_fixture_archive(&archive_path);

    let install_output = fontbrew(&home)
        .args(["--json", "install"])
        .arg(&archive_path)
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .clone();
    let install_json = stdout_json(&install_output);

    assert_eq!(install_json["schemaVersion"], 1);
    assert_eq!(install_json["command"], "install");
    assert_eq!(install_json["report"]["package_id"], "source-code-pro");
    assert_eq!(install_json["report"]["installed"], true);
    assert!(stderr_text(&install_output).is_empty());

    let list_output = fontbrew(&home)
        .args(["--json", "list"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .clone();
    let list_json = stdout_json(&list_output);

    assert_eq!(list_json["schemaVersion"], 1);
    assert_eq!(list_json["command"], "list");
    assert_eq!(
        list_json["report"]["packages"][0]["package_id"],
        "source-code-pro"
    );
    assert!(stderr_text(&list_output).is_empty());
}

#[test]
fn json_install_with_unmanaged_activation_conflict_fails_without_prompting() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let archive_path = temp.path().join("source-code-pro.zip");
    write_fixture_archive(&archive_path);

    let activation_dir = home.join("Library/Fonts/Fontbrew");
    fs::create_dir_all(&activation_dir).expect("create activation dir");
    fs::write(
        activation_dir.join("SourceCodePro-Regular.ttf"),
        b"unmanaged",
    )
    .expect("write unmanaged activation conflict");

    let output = fontbrew(&home)
        .args(["--json", "install"])
        .arg(&archive_path)
        .assert()
        .failure()
        .stderr(predicate::str::is_empty())
        .get_output()
        .clone();
    let json = stdout_json(&output);
    let stdout_text = String::from_utf8(output.stdout.clone()).expect("stdout should be utf-8");

    assert_eq!(json["schemaVersion"], 1);
    assert_eq!(json["error"]["kind"], "approval_required");
    assert!(!stdout_text.contains("Continue?"));
    assert!(stderr_text(&output).is_empty());
}

#[test]
fn json_install_dry_run_with_unmanaged_activation_conflict_succeeds_without_prompting() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let archive_path = temp.path().join("source-code-pro.zip");
    write_fixture_archive(&archive_path);

    let activation_dir = home.join("Library/Fonts/Fontbrew");
    fs::create_dir_all(&activation_dir).expect("create activation dir");
    fs::write(
        activation_dir.join("SourceCodePro-Regular.ttf"),
        b"unmanaged",
    )
    .expect("write unmanaged activation conflict");

    let output = fontbrew(&home)
        .args(["--json", "install", "--dry-run"])
        .arg(&archive_path)
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .clone();
    let json = stdout_json(&output);

    assert_eq!(json["schemaVersion"], 1);
    assert_eq!(json["command"], "install");
    assert_eq!(json["report"]["package_id"], "source-code-pro");
    assert_eq!(json["report"]["installed"], false);
    assert!(stderr_text(&output).is_empty());
}

#[test]
fn remove_dry_run_reports_planned_removal_without_mutating_package() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let archive_path = temp.path().join("source-code-pro.zip");
    write_fixture_archive(&archive_path);

    fontbrew(&home)
        .arg("install")
        .arg(&archive_path)
        .assert()
        .success();

    fontbrew(&home)
        .args(["remove", "--dry-run", "source-code-pro"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Planned removal source-code-pro"))
        .stdout(predicate::str::contains("is not installed").not())
        .stderr(predicate::str::is_empty());

    fontbrew(&home)
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains("source-code-pro"))
        .stderr(predicate::str::is_empty());

    let output = fontbrew(&home)
        .args(["--json", "remove", "--dry-run", "source-code-pro"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .clone();
    let json = stdout_json(&output);

    assert_eq!(json["schemaVersion"], 1);
    assert_eq!(json["command"], "remove");
    assert_eq!(json["report"]["package_id"], "source-code-pro");
    assert_eq!(json["report"]["removed"], false);
    assert_eq!(json["report"]["planned"], true);
}

#[test]
fn remove_dry_run_missing_package_reports_not_installed() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");

    fontbrew(&home)
        .args(["remove", "--dry-run", "source-code-pro"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "source-code-pro is not installed.",
        ))
        .stderr(predicate::str::is_empty());

    let output = fontbrew(&home)
        .args(["--json", "remove", "--dry-run", "source-code-pro"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .clone();
    let json = stdout_json(&output);

    assert_eq!(json["schemaVersion"], 1);
    assert_eq!(json["command"], "remove");
    assert_eq!(json["report"]["removed"], false);
    assert_eq!(json["report"]["planned"], false);
}

#[test]
fn json_parse_errors_are_rendered_as_json_stdout() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");

    let output = fontbrew(&home)
        .args(["--json", "install"])
        .assert()
        .failure()
        .stderr(predicate::str::is_empty())
        .get_output()
        .clone();
    let json = stdout_json(&output);

    assert_eq!(json["schemaVersion"], 1);
    assert_eq!(json["error"]["kind"], "usage");
}

#[test]
fn registry_update_uses_env_url_and_writes_metadata_only() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let registry_path = temp.path().join("registry.json");
    write_registry_snapshot(&registry_path);

    let output = fontbrew(&home)
        .env(
            REGISTRY_URL_ENV_VAR,
            format!("file://{}", registry_path.display()),
        )
        .args(["--json", "registry", "update"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .clone();
    let json = stdout_json(&output);

    assert_eq!(json["schemaVersion"], 1);
    assert_eq!(json["command"], "registry_update");
    assert_eq!(json["report"]["package_count"], 1);
    assert!(home.join(".local/share/fontbrew/registry.json").exists());
    assert!(!home.join(".local/share/fontbrew/packages").exists());
    assert!(!home.join(".local/share/fontbrew/staging").exists());
}

#[test]
fn registry_status_reports_missing_and_present_snapshots() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let registry_path = temp.path().join("registry.json");

    fontbrew(&home)
        .args(["registry", "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Registry snapshot: missing"))
        .stderr(predicate::str::is_empty());

    write_registry_snapshot(&registry_path);
    fontbrew(&home)
        .env(
            REGISTRY_URL_ENV_VAR,
            format!("file://{}", registry_path.display()),
        )
        .args(["registry", "update"])
        .assert()
        .success();

    let output = fontbrew(&home)
        .args(["--json", "registry", "status"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .clone();
    let json = stdout_json(&output);

    assert_eq!(json["schemaVersion"], 1);
    assert_eq!(json["command"], "registry_status");
    assert_eq!(json["report"]["available"], true);
    assert_eq!(json["report"]["package_count"], 1);
    assert_eq!(
        json["report"]["registry_updated_at"],
        "2026-07-03T00:00:00Z"
    );
}

#[test]
fn json_search_refreshes_registry_snapshot_and_reports_matches_on_stdout_only() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let registry_path = temp.path().join("registry.json");
    write_search_registry_snapshot(&registry_path);

    let output = fontbrew(&home)
        .env(
            REGISTRY_URL_ENV_VAR,
            format!("file://{}", registry_path.display()),
        )
        .args(["--json", "search", "code", "--limit", "1", "--refresh"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .clone();
    let json = stdout_json(&output);

    assert_eq!(json["schemaVersion"], 1);
    assert_eq!(json["command"], "search");
    assert_eq!(
        json["report"]["results"][0]["package_id"],
        "source-code-pro"
    );
    assert_eq!(
        json["report"]["results"][0]["display_name"],
        "Source Code Pro"
    );
    assert!(stderr_text(&output).is_empty());
}

#[test]
fn human_search_reports_registry_result_fields_on_stdout_only() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let registry_path = temp.path().join("registry.json");
    write_search_registry_snapshot(&registry_path);

    fontbrew(&home)
        .env(
            REGISTRY_URL_ENV_VAR,
            format!("file://{}", registry_path.display()),
        )
        .args(["search", "code", "--limit", "1", "--refresh"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "source-code-pro\tSource Code Pro\tSource Code Pro\tregistry:source-code-pro",
        ))
        .stderr(predicate::str::is_empty());
}

#[test]
fn json_outdated_reports_local_archive_as_not_updatable_on_stdout_only() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let archive_path = temp.path().join("source-code-pro.zip");
    write_fixture_archive(&archive_path);

    fontbrew(&home)
        .args(["--quiet", "install"])
        .arg(&archive_path)
        .assert()
        .success();

    let output = fontbrew(&home)
        .args(["--json", "outdated", "--offline", "source-code-pro"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .clone();
    let json = stdout_json(&output);

    assert_eq!(json["schemaVersion"], 1);
    assert_eq!(json["command"], "outdated");
    assert_eq!(json["report"]["packages"].as_array().unwrap().len(), 0);
    assert_eq!(
        json["report"]["not_updatable"][0]["package_id"],
        "source-code-pro"
    );
    assert!(json["report"]["not_updatable"][0]["reason"]
        .as_str()
        .unwrap()
        .contains("no GitHub update source"));
    assert!(stderr_text(&output).is_empty());
}

#[test]
fn human_outdated_offline_reports_local_archive_as_not_updatable_on_stdout_only() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let archive_path = temp.path().join("source-code-pro.zip");
    write_fixture_archive(&archive_path);

    fontbrew(&home)
        .args(["--quiet", "install"])
        .arg(&archive_path)
        .assert()
        .success();

    fontbrew(&home)
        .args(["outdated", "--offline", "source-code-pro"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("source-code-pro")
                .and(predicate::str::contains("not updatable")),
        )
        .stderr(predicate::str::is_empty());
}

#[test]
fn json_update_dry_run_reports_local_package_as_failed_without_prompting() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let archive_path = temp.path().join("source-code-pro.zip");
    write_fixture_archive(&archive_path);

    fontbrew(&home)
        .args(["--quiet", "install"])
        .arg(&archive_path)
        .assert()
        .success();

    let output = fontbrew(&home)
        .args([
            "--json",
            "update",
            "--dry-run",
            "--jobs",
            "2",
            "source-code-pro",
        ])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .clone();
    let json = stdout_json(&output);

    assert_eq!(json["schemaVersion"], 1);
    assert_eq!(json["command"], "update");
    assert_eq!(json["report"]["planned"].as_array().unwrap().len(), 0);
    assert_eq!(json["report"]["updated"].as_array().unwrap().len(), 0);
    assert_eq!(
        json["report"]["skipped"][0]["package_id"],
        "source-code-pro"
    );
    assert!(json["report"]["skipped"][0]["reason"]
        .as_str()
        .unwrap()
        .contains("no GitHub update source"));
    assert!(stderr_text(&output).is_empty());
}

#[test]
fn human_update_dry_run_reports_skipped_package_on_stdout_only() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let archive_path = temp.path().join("source-code-pro.zip");
    write_fixture_archive(&archive_path);

    fontbrew(&home)
        .args(["--quiet", "install"])
        .arg(&archive_path)
        .assert()
        .success();

    fontbrew(&home)
        .args(["update", "--dry-run", "source-code-pro"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("No updates prepared.")
                .and(predicate::str::contains("source-code-pro"))
                .and(predicate::str::contains("not prepared")),
        )
        .stderr(predicate::str::is_empty());
}
