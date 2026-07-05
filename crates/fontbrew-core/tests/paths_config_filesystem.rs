use std::fs;
use std::time::Duration;

use fontbrew_core::{
    config::{ActivationStrategy, FontbrewConfig},
    fs::{write_atomically, GlobalFileLock},
    platform::FontbrewPaths,
    ConfigGetRequest, ConfigSetRequest, ConfigValue, FontFormat, FontbrewApp, FontbrewError,
    PackageId, PackageVersion,
};

fn package_id(id: &str) -> PackageId {
    PackageId::parse(id).expect("test package id should be valid")
}

fn test_paths(temp: &tempfile::TempDir) -> FontbrewPaths {
    FontbrewPaths::for_tests(
        temp.path().join("data"),
        temp.path().join("config"),
        temp.path().join("home"),
    )
}

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
        paths.package_store_dir(&package_id("inter"), &PackageVersion::new("4.0")),
        temp.path().join("data/packages/inter/4.0")
    );
    assert_eq!(
        paths.manifest_path(),
        temp.path().join("data/manifest.json")
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

#[tokio::test]
async fn list_packages_returns_empty_report_from_async_app_seam() {
    let temp = tempfile::tempdir().expect("tempdir");
    let app = FontbrewApp::with_paths(test_paths(&temp));

    let report = app.list_packages().await.expect("list packages");

    assert!(report.packages.is_empty());
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
activation_strategy = "symlink"

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
    assert_eq!(config.activation_strategy, ActivationStrategy::Symlink);
    assert_eq!(config.metadata_ttl, Duration::from_secs(6 * 60 * 60));
    assert_eq!(config.update_concurrency, 2);
}

#[test]
fn config_file_rejects_reserved_copy_activation_strategy() {
    let temp = tempfile::tempdir().expect("tempdir");
    let config_path = temp.path().join("config.toml");
    fs::write(
        &config_path,
        r#"
schema_version = 1

[install]
activation_strategy = "copy"
"#,
    )
    .expect("write config");

    let error = FontbrewConfig::load(&config_path).expect_err("copy activation should be rejected");

    assert!(matches!(error, FontbrewError::Config { .. }));
    let message = error.to_string();
    assert!(message.contains("copy activation"));
    assert!(message.contains("reserved"));
    assert!(message.contains("not supported"));
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

[provider]
metadata_ttl = 12
"#,
    )
    .expect("write config");

    let error = FontbrewConfig::load(&config_path).expect_err("typo should be rejected");

    assert!(matches!(error, FontbrewError::Config { .. }));
}

#[tokio::test]
async fn config_set_persists_v1_toml_and_config_get_reads_known_keys() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let app = FontbrewApp::with_paths(paths.clone());

    let set_report = app
        .config_set(ConfigSetRequest {
            key: "install.format_preference".to_string(),
            value: "ttf,otf".to_string(),
        })
        .await
        .expect("set format preference");

    assert_eq!(set_report.key, "install.format_preference");
    assert_eq!(
        set_report.value,
        ConfigValue::List(vec!["ttf".to_string(), "otf".to_string()])
    );
    assert!(paths.config_path().exists());

    let config = FontbrewConfig::load(&paths.config_path()).expect("load persisted config");
    assert_eq!(
        config.format_preference,
        vec![FontFormat::Ttf, FontFormat::Otf]
    );
    assert_eq!(config.activation_strategy, ActivationStrategy::Symlink);
    assert_eq!(config.metadata_ttl, Duration::from_secs(24 * 60 * 60));
    assert_eq!(config.update_concurrency, 4);

    let get_report = app
        .config_get(ConfigGetRequest {
            key: "install.format_preference".to_string(),
        })
        .await
        .expect("get format preference");

    assert_eq!(get_report.key, "install.format_preference");
    assert_eq!(
        get_report.value,
        ConfigValue::List(vec!["ttf".to_string(), "otf".to_string()])
    );
}

#[tokio::test]
async fn config_set_and_get_support_all_known_scalar_keys() {
    let temp = tempfile::tempdir().expect("tempdir");
    let app = FontbrewApp::with_paths(test_paths(&temp));

    for (key, raw_value, expected_value) in [
        (
            "install.activation_strategy",
            "symlink",
            ConfigValue::String("symlink".to_string()),
        ),
        ("network.metadata_ttl_hours", "6", ConfigValue::Integer(6)),
        ("network.update_concurrency", "2", ConfigValue::Integer(2)),
    ] {
        let set_report = app
            .config_set(ConfigSetRequest {
                key: key.to_string(),
                value: raw_value.to_string(),
            })
            .await
            .expect("set known config key");
        assert_eq!(set_report.key, key);
        assert_eq!(set_report.value, expected_value);

        let get_report = app
            .config_get(ConfigGetRequest {
                key: key.to_string(),
            })
            .await
            .expect("get known config key");
        assert_eq!(get_report.key, key);
        assert_eq!(get_report.value, expected_value);
    }
}

#[tokio::test]
async fn config_set_rejects_reserved_copy_activation_strategy() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let app = FontbrewApp::with_paths(paths.clone());

    let error = app
        .config_set(ConfigSetRequest {
            key: "install.activation_strategy".to_string(),
            value: "copy".to_string(),
        })
        .await
        .expect_err("copy activation should be rejected");

    assert!(matches!(error, FontbrewError::Config { .. }));
    let message = error.to_string();
    assert!(message.contains("copy activation"));
    assert!(message.contains("reserved"));
    assert!(message.contains("not supported"));
    assert!(!paths.config_path().exists());
}

#[tokio::test]
async fn config_set_uses_global_write_lock() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let app = FontbrewApp::with_paths(paths.clone());
    let _held_lock =
        GlobalFileLock::try_exclusive(&paths.managed_store_dir().join(".fontbrew.lock"))
            .expect("hold global write lock");

    let error = app
        .config_set(ConfigSetRequest {
            key: "network.metadata_ttl_hours".to_string(),
            value: "6".to_string(),
        })
        .await
        .expect_err("config set should fail while global lock is held");

    assert!(matches!(error, FontbrewError::Lock { .. }));
    assert!(!paths.config_path().exists());
}

#[tokio::test]
async fn config_get_and_set_reject_unknown_keys_and_malformed_values() {
    let temp = tempfile::tempdir().expect("tempdir");
    let app = FontbrewApp::with_paths(test_paths(&temp));

    let unknown_get = app
        .config_get(ConfigGetRequest {
            key: "install.unknown".to_string(),
        })
        .await
        .expect_err("unknown get key should fail");
    let unknown_set = app
        .config_set(ConfigSetRequest {
            key: "install.unknown".to_string(),
            value: "true".to_string(),
        })
        .await
        .expect_err("unknown set key should fail");
    let malformed_set = app
        .config_set(ConfigSetRequest {
            key: "network.update_concurrency".to_string(),
            value: "many".to_string(),
        })
        .await
        .expect_err("malformed set value should fail");

    assert!(matches!(unknown_get, FontbrewError::Config { .. }));
    assert!(matches!(unknown_set, FontbrewError::Config { .. }));
    assert!(matches!(malformed_set, FontbrewError::Config { .. }));
}

#[test]
fn persisted_config_rejects_empty_or_zero_values() {
    let temp = tempfile::tempdir().expect("tempdir");

    for (name, content) in [
        (
            "empty-format-preference",
            r#"
schema_version = 1

[install]
format_preference = []
"#,
        ),
        (
            "zero-metadata-ttl",
            r#"
schema_version = 1

[network]
metadata_ttl_hours = 0
"#,
        ),
        (
            "zero-update-concurrency",
            r#"
schema_version = 1

[network]
update_concurrency = 0
"#,
        ),
    ] {
        let config_path = temp.path().join(format!("{name}.toml"));
        fs::write(&config_path, content).expect("write malformed config");

        let error = FontbrewConfig::load(&config_path)
            .expect_err("malformed persisted config should be rejected");

        assert!(
            matches!(error, FontbrewError::Config { .. }),
            "{name} produced {error:?}"
        );
    }
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
