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
fn app_resolves_registry_short_name_before_github_fetch_is_implemented() {
    let (_temp, paths) = paths();
    let store = RegistrySnapshotStore::new(paths.clone());
    let snapshot =
        RegistrySnapshotStore::parse(&valid_registry_json()).expect("valid registry should parse");
    store.write_snapshot(&snapshot).expect("write snapshot");
    let app = FontbrewApp::with_paths(paths);

    let error = app
        .install_plan(InstallRequest {
            source: InstallSource::RegistryName("inter".to_string()),
            format_preference: Vec::new(),
            asset_selector: None,
            reinstall: false,
            refresh: false,
            offline: false,
        })
        .expect_err("Task 11 should resolve registry but leave GitHub fetch to Task 12");

    assert!(matches!(
        error,
        FontbrewError::NotImplemented {
            operation: "github_release_install"
        }
    ));
}

#[test]
fn app_rejects_invalid_registry_snapshot_before_short_name_use() {
    let (_temp, paths) = paths();
    fs::create_dir_all(paths.managed_store_dir()).expect("create data root");
    fs::write(
        paths.registry_snapshot_path(),
        r#"{"schemaVersion": 1, "updatedAt": "2026-07-03T00:00:00Z", "packages": {"inter": {"name": "Inter", "source": {"type": "github", "repo": "rsms//inter"}, "families": ["Inter"]}}}"#,
    )
    .expect("write invalid snapshot");
    let app = FontbrewApp::with_paths(paths);

    let error = app
        .install_plan(InstallRequest {
            source: InstallSource::RegistryName("inter".to_string()),
            format_preference: Vec::new(),
            asset_selector: None,
            reinstall: false,
            refresh: false,
            offline: false,
        })
        .expect_err("invalid registry snapshot should be rejected");

    assert!(matches!(
        error,
        FontbrewError::RegistryValidationFailed { .. }
    ));
}

#[test]
fn registry_url_uses_environment_override() {
    let _guard = ENV_LOCK.lock().expect("env lock");
    let original = std::env::var_os(REGISTRY_URL_ENV_VAR);
    std::env::set_var(REGISTRY_URL_ENV_VAR, "https://example.test/registry.json");

    assert_eq!(
        registry_url_from_env(),
        "https://example.test/registry.json"
    );

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
