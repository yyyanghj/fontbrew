use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Output,
};

use assert_cmd::Command;
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
    write_fixture_archive_entries(
        archive_path,
        &[("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf")],
    );
}

fn write_fixture_archive_entries(archive_path: &Path, entries: &[(&str, &str)]) {
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

fn stdout_json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).expect("stdout should be parseable JSON")
}

fn stderr_text(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).expect("stderr should be utf-8")
}

fn staging_is_empty_or_absent(home: &Path) -> bool {
    let staging_dir = home.join(".local/share/fontbrew/staging");
    match fs::read_dir(&staging_dir) {
        Ok(mut entries) => entries.next().is_none(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(error) => panic!(
            "could not read staging dir {}: {error}",
            staging_dir.display()
        ),
    }
}

fn write_fontsource_metadata(home: &Path) {
    let metadata_dir = home.join(".local/share/fontbrew/providers");
    fs::create_dir_all(&metadata_dir).expect("create provider metadata dir");
    fs::write(
        metadata_dir.join("fontsource-list-all.json"),
        r#"[{"id":"source-code-pro","family":"Source Code Pro"}]"#,
    )
    .expect("write Fontsource list metadata");
    fs::write(
        metadata_dir.join("fontsource-detail-source-code-pro.json"),
        r#"{
  "id": "source-code-pro",
  "family": "Source Code Pro",
  "version": "1.0.0",
  "variants": {
    "400": {
      "normal": {
        "latin": {
          "url": {
            "ttf": "https://example.test/source-code-pro.ttf"
          }
        }
      }
    }
  }
}"#,
    )
    .expect("write Fontsource detail metadata");
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
            predicate::str::contains("Package")
                .and(predicate::str::contains("Version"))
                .and(predicate::str::contains("Families"))
                .and(predicate::str::contains("Status"))
                .and(predicate::str::contains("source-code-pro"))
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
                .and(predicate::str::contains("Family: Source Code Pro"))
                .and(predicate::str::contains("Status: active"))
                .and(predicate::str::contains("Updates: not configured"))
                .and(predicate::str::contains("Fonts:"))
                .and(predicate::str::contains("Name"))
                .and(predicate::str::contains("Weight"))
                .and(predicate::str::contains("Italic"))
                .and(predicate::str::contains("Installed"))
                .and(predicate::str::contains("Activated"))
                .and(predicate::str::contains("SourceCodePro-Regular.ttf"))
                .and(predicate::str::contains("400"))
                .and(predicate::str::contains("yes"))
                .and(predicate::str::contains("Installed files:").not())
                .and(predicate::str::contains("Activation artifacts:").not()),
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
fn verbose_install_reports_planning_and_apply_progress_on_stderr() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let archive_path = temp.path().join("source-code-pro.zip");
    write_fixture_archive(&archive_path);

    fontbrew(&home)
        .args(["-v", "install"])
        .arg(&archive_path)
        .assert()
        .success()
        .stdout(predicate::str::contains("Installed source-code-pro"))
        .stderr(
            predicate::str::contains("Resolving")
                .and(predicate::str::contains("Finished source-code-pro")),
        );
}

#[test]
fn install_local_archive_with_package_id_override() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let archive_path = temp.path().join("source-code-pro.zip");
    write_fixture_archive(&archive_path);

    fontbrew(&home)
        .args(["--quiet", "install"])
        .arg(&archive_path)
        .args(["--id", "custom-local"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Installed custom-local"))
        .stderr(predicate::str::is_empty());

    fontbrew(&home)
        .arg("list")
        .assert()
        .success()
        .stdout(
            predicate::str::contains("custom-local")
                .and(predicate::str::contains("Source Code Pro")),
        )
        .stderr(predicate::str::is_empty());

    assert!(home
        .join(".local/share/fontbrew/packages/custom-local/local/files/SourceCodePro-Regular.ttf")
        .exists());
}

#[test]
fn install_local_archive_with_family_selection_installs_selected_package() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let archive_path = temp.path().join("mixed-families.zip");
    write_fixture_archive_entries(
        &archive_path,
        &[
            ("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf"),
            ("Inter-Variable.ttf", "Inter-Variable.ttf"),
        ],
    );

    fontbrew(&home)
        .args(["--quiet", "install"])
        .arg(&archive_path)
        .args(["--family", "Inter"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Installed inter"))
        .stderr(predicate::str::is_empty());

    fontbrew(&home)
        .arg("list")
        .assert()
        .success()
        .stdout(
            predicate::str::contains("inter")
                .and(predicate::str::contains("Inter"))
                .and(predicate::str::contains("source-code-pro").not()),
        )
        .stderr(predicate::str::is_empty());
}

#[test]
fn install_local_archive_with_all_families_installs_each_package() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let archive_path = temp.path().join("mixed-families.zip");
    write_fixture_archive_entries(
        &archive_path,
        &[
            ("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf"),
            ("Inter-Variable.ttf", "Inter-Variable.ttf"),
        ],
    );

    fontbrew(&home)
        .args(["--quiet", "install"])
        .arg(&archive_path)
        .arg("--all-families")
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Installed inter")
                .and(predicate::str::contains("Installed source-code-pro")),
        )
        .stderr(predicate::str::is_empty());

    fontbrew(&home)
        .arg("list")
        .assert()
        .success()
        .stdout(
            predicate::str::contains("inter")
                .and(predicate::str::contains("source-code-pro"))
                .and(predicate::str::contains("Inter"))
                .and(predicate::str::contains("Source Code Pro")),
        )
        .stderr(predicate::str::is_empty());
}

#[test]
fn install_local_archive_with_all_families_reports_source_resolution_once() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let archive_path = temp.path().join("mixed-families.zip");
    write_fixture_archive_entries(
        &archive_path,
        &[
            ("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf"),
            ("Inter-Variable.ttf", "Inter-Variable.ttf"),
        ],
    );

    let output = fontbrew(&home)
        .args(["-v", "install"])
        .arg(&archive_path)
        .arg("--all-families")
        .assert()
        .success()
        .get_output()
        .clone();
    let stderr = stderr_text(&output);

    assert_eq!(
        stderr.matches("Resolving ").count(),
        1,
        "stderr should not repeat source resolution progress:\n{stderr}"
    );
}

#[test]
fn json_install_reports_family_selection_required_for_multi_family_source() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let archive_path = temp.path().join("mixed-families.zip");
    write_fixture_archive_entries(
        &archive_path,
        &[
            ("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf"),
            ("Inter-Variable.ttf", "Inter-Variable.ttf"),
        ],
    );

    let output = fontbrew(&home)
        .args(["--json", "install"])
        .arg(&archive_path)
        .assert()
        .failure()
        .stdout(predicate::str::contains("family_selection_required"))
        .stderr(predicate::str::is_empty())
        .get_output()
        .clone();
    let json = stdout_json(&output);

    assert_eq!(json["error"]["kind"], "family_selection_required");
    assert_eq!(json["error"]["families"][0], "Inter");
    assert_eq!(json["error"]["families"][1], "Source Code Pro");
    assert!(staging_is_empty_or_absent(&home));
}

#[test]
fn install_rejects_invalid_package_id_override() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let archive_path = temp.path().join("source-code-pro.zip");
    write_fixture_archive(&archive_path);

    fontbrew(&home)
        .args(["install"])
        .arg(&archive_path)
        .args(["--id", "Source Code Pro"])
        .assert()
        .failure()
        .stdout(predicate::str::is_empty())
        .stderr(
            predicate::str::contains("invalid package id")
                .and(predicate::str::contains("lowercase")),
        );

    assert!(staging_is_empty_or_absent(&home));
}

#[test]
fn install_rejects_package_id_override_for_non_local_sources() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");

    for source in [
        "source-code-pro",
        "adobe/source-code-pro",
        "fontsource:source-code-pro",
    ] {
        fontbrew(&home)
            .args(["install", source, "--id", "custom-local"])
            .assert()
            .failure()
            .stdout(predicate::str::is_empty())
            .stderr(
                predicate::str::contains("--id").and(predicate::str::contains("local archive")),
            );
    }

    assert!(staging_is_empty_or_absent(&home));
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

    let info_output = fontbrew(&home)
        .args(["--json", "info", "source-code-pro"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .clone();
    let info_json = stdout_json(&info_output);

    assert_eq!(info_json["schemaVersion"], 1);
    assert_eq!(info_json["command"], "info");
    assert_eq!(info_json["report"]["package"]["managed"], true);
    assert!(info_json["report"]["package"]["font_files"]
        .as_array()
        .expect("font_files should be an array")
        .iter()
        .any(|font_file| font_file["path"]
            .as_str()
            .expect("font file path should be a string")
            .contains("SourceCodePro-Regular.ttf")));
    assert!(info_json["report"]["package"]["activation_artifacts"]
        .as_array()
        .expect("activation_artifacts should be an array")
        .iter()
        .any(|artifact| artifact["path"]
            .as_str()
            .expect("activation artifact path should be a string")
            .contains("SourceCodePro-Regular.ttf")));
}

#[test]
fn config_set_and_get_report_human_and_json_values() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");

    fontbrew(&home)
        .args(["config", "set", "install.format_preference", "ttf,otf"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "install.format_preference = [\"ttf\", \"otf\"]",
        ))
        .stderr(predicate::str::is_empty());

    fontbrew(&home)
        .args(["config", "get", "install.format_preference"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "install.format_preference = [\"ttf\", \"otf\"]",
        ))
        .stderr(predicate::str::is_empty());

    let output = fontbrew(&home)
        .args(["--json", "config", "get", "install.format_preference"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .clone();
    let json = stdout_json(&output);

    assert_eq!(json["schemaVersion"], 1);
    assert_eq!(json["command"], "config_get");
    assert_eq!(json["report"]["key"], "install.format_preference");
    assert_eq!(json["report"]["value"][0], "ttf");
    assert_eq!(json["report"]["value"][1], "otf");
}

#[test]
fn config_set_rejects_reserved_copy_activation_strategy() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");

    fontbrew(&home)
        .args(["config", "set", "install.activation_strategy", "copy"])
        .assert()
        .failure()
        .stdout(predicate::str::is_empty())
        .stderr(
            predicate::str::contains("copy activation")
                .and(predicate::str::contains("reserved"))
                .and(predicate::str::contains("not supported")),
        );

    let output = fontbrew(&home)
        .args([
            "--json",
            "config",
            "set",
            "install.activation_strategy",
            "copy",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::is_empty())
        .get_output()
        .clone();
    let json = stdout_json(&output);
    let message = json["error"]["message"]
        .as_str()
        .expect("error message should be a string");

    assert_eq!(json["schemaVersion"], 1);
    assert_eq!(json["error"]["kind"], "config");
    assert!(message.contains("copy activation"));
    assert!(message.contains("reserved"));
    assert!(message.contains("not supported"));
}

#[test]
fn install_otf_flag_overrides_global_ttf_format_preference() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let archive_path = temp.path().join("source-code-pro.zip");
    write_fixture_archive_entries(
        &archive_path,
        &[
            ("SourceCodePro-Regular.otf", "SourceCodePro-Regular.otf"),
            ("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf"),
        ],
    );

    fontbrew(&home)
        .args(["config", "set", "install.format_preference", "ttf,otf"])
        .assert()
        .success();

    fontbrew(&home)
        .args(["install", "--otf"])
        .arg(&archive_path)
        .assert()
        .success()
        .stdout(predicate::str::contains("Installed source-code-pro"))
        .stderr(predicate::str::is_empty());

    let package_dir = home.join(".local/share/fontbrew/packages/source-code-pro/local/files");
    assert!(package_dir.join("SourceCodePro-Regular.otf").exists());
    assert!(!package_dir.join("SourceCodePro-Regular.ttf").exists());
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
    assert_eq!(
        json["error"]["risks"][0]["Conflict"]["package_id"],
        "source-code-pro"
    );
    assert!(json["error"]["risks"][0]["Conflict"]["description"]
        .as_str()
        .expect("risk description")
        .contains("SourceCodePro-Regular.ttf"));
    assert!(!stdout_text.contains("Continue?"));
    assert!(stderr_text(&output).is_empty());
}

#[test]
fn json_install_with_same_family_overlap_requires_yes_and_reports_structured_risk() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let archive_path = temp.path().join("source-code-pro.zip");
    write_fixture_archive(&archive_path);

    let user_fonts_dir = home.join("Library/Fonts");
    fs::create_dir_all(&user_fonts_dir).expect("create user fonts dir");
    fs::copy(
        fixture_font_path("SourceCodePro-Regular.ttf"),
        user_fonts_dir.join("ManualSourceCodePro.ttf"),
    )
    .expect("write unmanaged same-family font");

    let output = fontbrew(&home)
        .args(["--json", "install"])
        .arg(&archive_path)
        .assert()
        .failure()
        .stderr(predicate::str::is_empty())
        .get_output()
        .clone();
    let json = stdout_json(&output);

    assert_eq!(json["schemaVersion"], 1);
    assert_eq!(json["error"]["kind"], "approval_required");
    assert_eq!(
        json["error"]["risks"][0]["UnmanagedFontOverlap"]["family_name"],
        "Source Code Pro"
    );
    assert!(
        json["error"]["risks"][0]["UnmanagedFontOverlap"]["description"]
            .as_str()
            .expect("risk description")
            .contains("ManualSourceCodePro.ttf")
    );
    assert!(stderr_text(&output).is_empty());
    assert!(staging_is_empty_or_absent(&home));
}

#[test]
fn json_install_with_same_family_overlap_and_yes_installs_without_overwriting_unmanaged_font() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let archive_path = temp.path().join("source-code-pro.zip");
    write_fixture_archive(&archive_path);

    let user_fonts_dir = home.join("Library/Fonts");
    fs::create_dir_all(&user_fonts_dir).expect("create user fonts dir");
    let unmanaged_font = user_fonts_dir.join("ManualSourceCodePro.ttf");
    fs::copy(
        fixture_font_path("SourceCodePro-Regular.ttf"),
        &unmanaged_font,
    )
    .expect("write unmanaged same-family font");
    let unmanaged_bytes = fs::read(&unmanaged_font).expect("read unmanaged font");

    let output = fontbrew(&home)
        .args(["--json", "install", "--yes"])
        .arg(&archive_path)
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .clone();
    let json = stdout_json(&output);

    assert_eq!(json["schemaVersion"], 1);
    assert_eq!(json["command"], "install");
    assert_eq!(json["report"]["installed"], true);
    assert_eq!(
        fs::read(&unmanaged_font).expect("unmanaged font remains"),
        unmanaged_bytes
    );
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
        .stdout(
            predicate::str::contains("Planned removal source-code-pro")
                .and(predicate::str::contains("Will remove font files:"))
                .and(predicate::str::contains("SourceCodePro-Regular.ttf"))
                .and(predicate::str::contains(
                    "Will remove activation artifacts:",
                )),
        )
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
    assert!(json["report"]["font_files"]
        .as_array()
        .expect("font_files should be an array")
        .iter()
        .any(|font_file| font_file["path"]
            .as_str()
            .expect("font file path should be a string")
            .contains("SourceCodePro-Regular.ttf")));
    assert!(json["report"]["activation_artifacts"]
        .as_array()
        .expect("activation_artifacts should be an array")
        .iter()
        .any(|artifact| artifact["path"]
            .as_str()
            .expect("activation artifact path should be a string")
            .contains("SourceCodePro-Regular.ttf")));
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
fn self_update_rejects_development_build_on_stderr() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");

    fontbrew(&home)
        .arg("self-update")
        .assert()
        .failure()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains("development build"));
}

#[test]
fn json_self_update_rejects_development_build_on_stdout_only() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");

    let output = fontbrew(&home)
        .args(["--json", "self-update"])
        .assert()
        .failure()
        .stderr(predicate::str::is_empty())
        .get_output()
        .clone();
    let json = stdout_json(&output);

    assert_eq!(json["schemaVersion"], 1);
    assert_eq!(json["error"]["kind"], "self_update_unavailable");
    assert!(json["error"]["message"]
        .as_str()
        .expect("error message should be a string")
        .contains("development build"));
}

#[test]
fn json_search_uses_fontsource_metadata_and_reports_matches_on_stdout_only() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    write_fontsource_metadata(&home);

    let output = fontbrew(&home)
        .args(["--json", "search", "code", "--limit", "1"])
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
    assert_eq!(
        json["report"]["results"][0]["source"],
        "fontsource:source-code-pro"
    );
    assert!(stderr_text(&output).is_empty());
}

#[test]
fn human_search_reports_fontsource_result_fields_on_stdout_only() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    write_fontsource_metadata(&home);

    let output = fontbrew(&home)
        .args(["search", "code", "--limit", "1"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .clone();
    let stdout = String::from_utf8(output.stdout).expect("stdout should be utf-8");

    assert!(stdout.lines().next().is_some_and(|line| {
        !line.contains("Package")
            && line.contains("Name")
            && line.contains("Families")
            && line.contains("Source")
    }));
    assert!(stdout.contains("source-code-pro"));
    assert!(stdout.contains("Source Code Pro"));
    assert!(stdout.contains("fontsource:source-code-pro"));
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
        .args(["--json", "outdated", "source-code-pro"])
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
        .contains("no update source"));
    assert!(stderr_text(&output).is_empty());
}

#[test]
fn human_outdated_reports_local_archive_as_not_updatable_on_stdout_only() {
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
        .args(["outdated", "source-code-pro"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Package")
                .and(predicate::str::contains("Status"))
                .and(predicate::str::contains("Reason"))
                .and(predicate::str::contains("source-code-pro"))
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
        .contains("no update source"));
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
                .and(predicate::str::contains("Package"))
                .and(predicate::str::contains("Status"))
                .and(predicate::str::contains("Reason"))
                .and(predicate::str::contains("source-code-pro"))
                .and(predicate::str::contains("not prepared")),
        )
        .stderr(predicate::str::is_empty());
}
