use std::fs;
use std::time::Duration;

use fontbrew_core::{
    config::{ActivationStrategy, FontbrewConfig},
    fs::{write_atomically, GlobalFileLock},
    platform::FontbrewPaths,
    FontFormat, FontbrewError, PackageId, PackageVersion,
};

#[test]
fn injected_paths_resolve_all_fontbrew_locations_without_home_access() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = FontbrewPaths::for_tests(
        temp.path().join("data"),
        temp.path().join("config"),
        temp.path().join("home"),
    );

    assert_eq!(paths.managed_store_dir(), temp.path().join("data"));
    assert_eq!(
        paths.package_store_dir(&PackageId::new("inter"), &PackageVersion::new("4.0")),
        temp.path().join("data/packages/inter/4.0")
    );
    assert_eq!(
        paths.manifest_path(),
        temp.path().join("data/manifest.json")
    );
    assert_eq!(
        paths.registry_snapshot_path(),
        temp.path().join("data/registry.json")
    );
    assert_eq!(
        paths.provider_metadata_dir(),
        temp.path().join("data/providers")
    );
    assert_eq!(
        paths.config_path(),
        temp.path().join("config/fontbrew/config.toml")
    );
    assert_eq!(paths.staging_dir(), temp.path().join("data/staging"));
    assert_eq!(
        paths.activation_dir(),
        temp.path().join("home/Library/Fonts/Fontbrew")
    );
}

#[test]
fn missing_config_file_uses_deterministic_v1_defaults() {
    let temp = tempfile::tempdir().expect("tempdir");
    let config_path = temp.path().join("config.toml");

    let config = FontbrewConfig::load(&config_path).expect("missing config should use defaults");

    assert_eq!(config.schema_version, 1);
    assert_eq!(
        config.format_preference,
        vec![
            FontFormat::Otf,
            FontFormat::Ttf,
            FontFormat::Ttc,
            FontFormat::Otc
        ]
    );
    assert_eq!(config.activation_strategy, ActivationStrategy::Symlink);
    assert!(config.registry_auto_update);
    assert_eq!(config.metadata_ttl, Duration::from_secs(24 * 60 * 60));
    assert_eq!(config.update_concurrency, 4);
}

#[test]
fn config_file_parses_v1_toml_shape() {
    let temp = tempfile::tempdir().expect("tempdir");
    let config_path = temp.path().join("config.toml");
    fs::write(
        &config_path,
        r#"
schema_version = 1

[install]
format_preference = ["ttf", "otf"]
activation_strategy = "copy"

[registry]
auto_update = false

[network]
metadata_ttl_hours = 6
update_concurrency = 2
"#,
    )
    .expect("write config");

    let config = FontbrewConfig::load(&config_path).expect("config should parse");

    assert_eq!(
        config.format_preference,
        vec![FontFormat::Ttf, FontFormat::Otf]
    );
    assert_eq!(config.activation_strategy, ActivationStrategy::Copy);
    assert!(!config.registry_auto_update);
    assert_eq!(config.metadata_ttl, Duration::from_secs(6 * 60 * 60));
    assert_eq!(config.update_concurrency, 2);
}

#[test]
fn missing_and_newer_schema_versions_are_structured_config_errors() {
    let temp = tempfile::tempdir().expect("tempdir");
    let missing_schema_path = temp.path().join("missing-schema.toml");
    let newer_schema_path = temp.path().join("newer-schema.toml");

    fs::write(&missing_schema_path, "format_preference = [\"otf\"]\n").expect("write config");
    fs::write(&newer_schema_path, "schema_version = 2\n").expect("write config");

    let missing_error = FontbrewConfig::load(&missing_schema_path).expect_err("schema is required");
    let newer_error = FontbrewConfig::load(&newer_schema_path).expect_err("new schema is rejected");

    assert!(matches!(missing_error, FontbrewError::Config { .. }));
    assert!(matches!(newer_error, FontbrewError::Config { .. }));
}

#[test]
fn unknown_grouped_config_fields_are_structured_config_errors() {
    let temp = tempfile::tempdir().expect("tempdir");
    let config_path = temp.path().join("config.toml");
    fs::write(
        &config_path,
        r#"
schema_version = 1

[registry]
auto_udpate = true
"#,
    )
    .expect("write config");

    let error = FontbrewConfig::load(&config_path).expect_err("typo should be rejected");

    assert!(matches!(error, FontbrewError::Config { .. }));
}

#[test]
fn atomic_write_replaces_final_file_without_partial_content() {
    let temp = tempfile::tempdir().expect("tempdir");
    let target = temp.path().join("nested/manifest.json");

    write_atomically(&target, br#"{"packages":[]}"#).expect("initial atomic write");
    write_atomically(&target, br#"{"packages":[{"id":"inter"}]}"#)
        .expect("replacement atomic write");

    let final_content = fs::read_to_string(&target).expect("read target");

    assert_eq!(final_content, r#"{"packages":[{"id":"inter"}]}"#);
}

#[test]
fn second_global_write_lock_attempt_fails_while_first_lock_is_held() {
    let temp = tempfile::tempdir().expect("tempdir");
    let lock_path = temp.path().join("fontbrew.lock");

    let first_lock = GlobalFileLock::try_exclusive(&lock_path).expect("first lock");
    let second_lock = GlobalFileLock::try_exclusive(&lock_path);

    assert!(matches!(second_lock, Err(FontbrewError::Lock { .. })));

    drop(first_lock);

    GlobalFileLock::try_exclusive(&lock_path).expect("lock should be available after drop");
}
