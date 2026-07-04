use std::cell::RefCell;
use std::fs;

use fontbrew_core::{
    platform::FontbrewPaths,
    registry::{
        registry_url_from_env, RegistryHttpClient, RegistrySnapshotStore, REGISTRY_URL_ENV_VAR,
    },
    FontbrewApp, FontbrewError, InstallRequest, InstallSource,
};

fn paths() -> (tempfile::TempDir, FontbrewPaths) {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = FontbrewPaths::for_tests(
        temp.path().join("data"),
        temp.path().join("config"),
        temp.path().join("home"),
    );

    (temp, paths)
}

fn valid_registry_json() -> String {
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
      "release": {
        "channel": "stable"
      },
      "asset": {
        "include": ["*Inter*.zip"],
        "exclude": ["*web*", "*.woff2"]
      },
      "install": {
        "formatPreference": ["otf", "ttf"]
      }
    }
  }
}"#
    .to_string()
}

#[test]
fn registry_snapshot_rejects_invalid_entries_before_use() {
    for (name, json) in [
        (
            "wrong schema",
            r#"{"schemaVersion": 2, "updatedAt": "2026-07-03T00:00:00Z", "packages": {}}"#,
        ),
        (
            "unsafe package id",
            r#"{"schemaVersion": 1, "updatedAt": "2026-07-03T00:00:00Z", "packages": {"Inter": {"name": "Inter", "source": {"type": "github", "repo": "rsms/inter"}, "families": ["Inter"]}}}"#,
        ),
        (
            "duplicate package id",
            r#"{"schemaVersion": 1, "updatedAt": "2026-07-03T00:00:00Z", "packages": {"inter": {"name": "Inter", "source": {"type": "github", "repo": "rsms/inter"}, "families": ["Inter"]}, "inter": {"name": "Inter 2", "source": {"type": "github", "repo": "rsms/inter"}, "families": ["Inter"]}}}"#,
        ),
        (
            "unknown source type",
            r#"{"schemaVersion": 1, "updatedAt": "2026-07-03T00:00:00Z", "packages": {"inter": {"name": "Inter", "source": {"type": "tarball", "url": "https://example.test/inter.zip"}, "families": ["Inter"]}}}"#,
        ),
        (
            "invalid github repo",
            r#"{"schemaVersion": 1, "updatedAt": "2026-07-03T00:00:00Z", "packages": {"inter": {"name": "Inter", "source": {"type": "github", "repo": "rsms//inter"}, "families": ["Inter"]}}}"#,
        ),
        (
            "invalid glob",
            r#"{"schemaVersion": 1, "updatedAt": "2026-07-03T00:00:00Z", "packages": {"inter": {"name": "Inter", "source": {"type": "github", "repo": "rsms/inter"}, "families": ["Inter"], "asset": {"include": ["["]}}}}"#,
        ),
        (
            "unknown required behavior",
            r#"{"schemaVersion": 1, "updatedAt": "2026-07-03T00:00:00Z", "required": ["future-registry-v99"], "packages": {}}"#,
        ),
        (
            "empty include family",
            r#"{"schemaVersion": 1, "updatedAt": "2026-07-03T00:00:00Z", "packages": {"inter": {"name": "Inter", "source": {"type": "github", "repo": "rsms/inter"}, "families": ["Inter"], "install": {"includeFamilies": [" "]}}}}"#,
        ),
        (
            "empty exclude family",
            r#"{"schemaVersion": 1, "updatedAt": "2026-07-03T00:00:00Z", "packages": {"inter": {"name": "Inter", "source": {"type": "github", "repo": "rsms/inter"}, "families": ["Inter"], "install": {"excludeFamilies": [" "]}}}}"#,
        ),
        (
            "duplicate normalized include family",
            r#"{"schemaVersion": 1, "updatedAt": "2026-07-03T00:00:00Z", "packages": {"source-code-pro": {"name": "Source Code Pro", "source": {"type": "github", "repo": "adobe/source-code-pro"}, "families": ["Source Code Pro"], "install": {"includeFamilies": ["Source Code Pro", " source   code pro "]}}}}"#,
        ),
        (
            "duplicate normalized exclude family",
            r#"{"schemaVersion": 1, "updatedAt": "2026-07-03T00:00:00Z", "packages": {"source-code-pro": {"name": "Source Code Pro", "source": {"type": "github", "repo": "adobe/source-code-pro"}, "families": ["Source Code Pro"], "install": {"excludeFamilies": ["Inter", " inter "]}}}}"#,
        ),
        (
            "include exclude overlap",
            r#"{"schemaVersion": 1, "updatedAt": "2026-07-03T00:00:00Z", "packages": {"source-code-pro": {"name": "Source Code Pro", "source": {"type": "github", "repo": "adobe/source-code-pro"}, "families": ["Source Code Pro"], "install": {"includeFamilies": ["Source Code Pro"], "excludeFamilies": [" source code pro "]}}}}"#,
        ),
        (
            "exclude overlaps default include family",
            r#"{"schemaVersion": 1, "updatedAt": "2026-07-03T00:00:00Z", "packages": {"source-code-pro": {"name": "Source Code Pro", "source": {"type": "github", "repo": "adobe/source-code-pro"}, "families": ["Source Code Pro"], "install": {"excludeFamilies": [" source code pro "]}}}}"#,
        ),
    ] {
        let error = RegistrySnapshotStore::parse(json)
            .expect_err(&format!("{name} registry should be rejected"));

        assert!(
            matches!(error, FontbrewError::RegistryValidationFailed { .. }),
            "{name} produced {error:?}"
        );
    }
}

#[test]
fn registry_snapshot_reads_writes_and_resolves_short_names() {
    let (_temp, paths) = paths();
    let store = RegistrySnapshotStore::new(paths.clone());
    let snapshot =
        RegistrySnapshotStore::parse(&valid_registry_json()).expect("valid registry should parse");

    store
        .write_snapshot(&snapshot)
        .expect("snapshot should write atomically");
    let read_back = store.read_snapshot().expect("snapshot should read");
    let recipe = read_back
        .resolve_short_name("inter")
        .expect("short name should resolve from snapshot");

    assert_eq!(recipe.package_id.as_str(), "inter");
    assert_eq!(recipe.name, "Inter");
    assert_eq!(recipe.github_repo.owner, "rsms");
    assert_eq!(recipe.github_repo.repo, "inter");
    assert!(read_back.resolve_short_name("missing").is_err());
    assert!(paths.registry_snapshot_path().exists());
}

#[test]
fn registry_snapshot_reads_default_empty_snapshot_when_missing() {
    let (_temp, paths) = paths();
    let store = RegistrySnapshotStore::new(paths.clone());

    assert!(!paths.registry_snapshot_path().exists());

    let snapshot = store
        .read_snapshot()
        .expect("missing registry snapshot should be seeded from default");

    assert_eq!(snapshot.schema_version, 1);
    assert_eq!(snapshot.updated_at, "1970-01-01T00:00:00Z");
    assert!(snapshot.packages.is_empty());
    assert!(paths.registry_snapshot_path().exists());
}

#[test]
fn registry_update_fetches_metadata_with_fake_client_without_caching_fonts() {
    let (_temp, paths) = paths();
    let store = RegistrySnapshotStore::new(paths.clone());
    let client = FakeRegistryClient {
        response: valid_registry_json(),
        requested_urls: RefCell::new(Vec::new()),
    };

    let report = store
        .update_from_client(&client, "https://registry.example.test/registry.json")
        .expect("registry update should write valid snapshot");

    assert_eq!(report.package_count, 1);
    assert_eq!(
        client.requested_urls.borrow().as_slice(),
        ["https://registry.example.test/registry.json"]
    );
    assert!(paths.registry_snapshot_path().exists());
    assert!(!paths.managed_store_dir().join("packages").exists());
    assert!(!paths.staging_dir().exists());
}

#[test]
fn registry_status_reports_snapshot_schema_version_when_available() {
    let (_temp, paths) = paths();
    let store = RegistrySnapshotStore::new(paths.clone());

    let default_status = store
        .status()
        .expect("missing snapshot should be seeded before status");
    assert!(default_status.available);
    assert_eq!(default_status.schema_version, Some(1));
    assert_eq!(
        default_status.registry_updated_at.as_deref(),
        Some("1970-01-01T00:00:00Z")
    );
    assert_eq!(default_status.package_count, 0);
    assert!(paths.registry_snapshot_path().exists());

    let snapshot =
        RegistrySnapshotStore::parse(&valid_registry_json()).expect("valid registry should parse");
    store
        .write_snapshot(&snapshot)
        .expect("snapshot should write atomically");

    let available = store.status().expect("available status should report");
    assert!(available.available);
    assert_eq!(available.schema_version, Some(1));
}

#[test]
fn registry_status_rejects_newer_snapshot_schema_before_reporting_status() {
    let (_temp, paths) = paths();
    let store = RegistrySnapshotStore::new(paths.clone());
    fs::create_dir_all(paths.managed_store_dir()).expect("create data root");
    fs::write(
        paths.registry_snapshot_path(),
        r#"{"schemaVersion": 2, "updatedAt": "2026-07-03T00:00:00Z", "packages": {}}"#,
    )
    .expect("write newer snapshot");

    let error = store
        .status()
        .expect_err("newer schema should fail before status is rendered");

    assert!(matches!(
        error,
        FontbrewError::RegistryValidationFailed { .. }
    ));
    assert!(error
        .to_string()
        .contains("unsupported registry schemaVersion"));
}

#[test]
fn app_rejects_invalid_refreshed_registry_snapshot_before_short_name_use() {
    let (_temp, paths) = paths();
    let _guard = ENV_LOCK.lock().expect("env lock");
    let original = std::env::var_os(REGISTRY_URL_ENV_VAR);
    fs::create_dir_all(paths.managed_store_dir()).expect("create data root");
    fs::write(
        paths.registry_snapshot_path(),
        r#"{"schemaVersion": 1, "updatedAt": "2026-07-03T00:00:00Z", "packages": {"inter": {"name": "Inter", "source": {"type": "github", "repo": "rsms//inter"}, "families": ["Inter"]}}}"#,
    )
    .expect("write invalid snapshot");
    std::env::set_var(
        REGISTRY_URL_ENV_VAR,
        format!("file://{}", paths.registry_snapshot_path().display()),
    );
    let app = FontbrewApp::with_paths(paths);

    let error = app
        .install_plan(InstallRequest {
            source: InstallSource::RegistryName("inter".to_string()),
            package_id_override: None,
            format_preference: Vec::new(),
            asset_selector: None,
            selected_families: Vec::new(),
            reinstall: false,
        })
        .expect_err("invalid registry snapshot should be rejected");

    assert!(matches!(
        error,
        FontbrewError::RegistryValidationFailed { .. }
    ));

    match original {
        Some(value) => std::env::set_var(REGISTRY_URL_ENV_VAR, value),
        None => std::env::remove_var(REGISTRY_URL_ENV_VAR),
    }
}

#[test]
fn app_reads_default_registry_snapshot_without_registry_url_for_short_name_use() {
    let (_temp, paths) = paths();
    let _guard = ENV_LOCK.lock().expect("env lock");
    let original = std::env::var_os(REGISTRY_URL_ENV_VAR);
    std::env::remove_var(REGISTRY_URL_ENV_VAR);
    let app = FontbrewApp::with_paths(paths.clone());

    let error = app
        .install_plan(InstallRequest {
            source: InstallSource::RegistryName("inter".to_string()),
            package_id_override: None,
            format_preference: Vec::new(),
            asset_selector: None,
            selected_families: Vec::new(),
            reinstall: false,
        })
        .expect_err("default empty registry should not contain inter");

    assert!(matches!(
        error,
        FontbrewError::RegistryValidationFailed { .. }
    ));
    assert!(error.to_string().contains("registry package not found"));
    assert!(paths.registry_snapshot_path().exists());

    match original {
        Some(value) => std::env::set_var(REGISTRY_URL_ENV_VAR, value),
        None => std::env::remove_var(REGISTRY_URL_ENV_VAR),
    }
}

#[test]
fn registry_url_uses_environment_override() {
    let _guard = ENV_LOCK.lock().expect("env lock");
    let original = std::env::var_os(REGISTRY_URL_ENV_VAR);
    std::env::set_var(REGISTRY_URL_ENV_VAR, "https://example.test/registry.json");

    assert_eq!(
        registry_url_from_env(),
        Some("https://example.test/registry.json".to_string())
    );

    match original {
        Some(value) => std::env::set_var(REGISTRY_URL_ENV_VAR, value),
        None => std::env::remove_var(REGISTRY_URL_ENV_VAR),
    }
}

#[test]
fn registry_url_has_no_default_when_environment_is_unset_or_blank() {
    let _guard = ENV_LOCK.lock().expect("env lock");
    let original = std::env::var_os(REGISTRY_URL_ENV_VAR);
    std::env::remove_var(REGISTRY_URL_ENV_VAR);

    assert_eq!(registry_url_from_env(), None);

    std::env::set_var(REGISTRY_URL_ENV_VAR, " ");
    assert_eq!(registry_url_from_env(), None);

    match original {
        Some(value) => std::env::set_var(REGISTRY_URL_ENV_VAR, value),
        None => std::env::remove_var(REGISTRY_URL_ENV_VAR),
    }
}

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct FakeRegistryClient {
    response: String,
    requested_urls: RefCell<Vec<String>>,
}

impl RegistryHttpClient for FakeRegistryClient {
    fn get_text(&self, url: &str) -> fontbrew_core::Result<String> {
        self.requested_urls.borrow_mut().push(url.to_string());
        Ok(self.response.clone())
    }
}
