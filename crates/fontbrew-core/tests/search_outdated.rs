use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
};

use fontbrew_core::{
    fetch::{HttpClient, HttpRequest, HttpResponse},
    manifest::{ManifestPackageRecord, ManifestSource, ManifestStore, ManifestV1},
    platform::FontbrewPaths,
    registry::{RegistrySnapshotStore, REGISTRY_URL_ENV_VAR},
    CancellationToken, FamilyName, FontbrewApp, FontbrewError, OutdatedRequest, PackageId,
    PackageVersion, SearchRequest,
};

const TEST_REGISTRY_URL: &str = "https://registry.example.test/registry.json";
static ENV_LOCK: Mutex<()> = Mutex::new(());

struct RegistryUrlGuard {
    original: Option<OsString>,
    _guard: MutexGuard<'static, ()>,
}

impl RegistryUrlGuard {
    fn set(url: &str) -> Self {
        let guard = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let original = std::env::var_os(REGISTRY_URL_ENV_VAR);
        std::env::set_var(REGISTRY_URL_ENV_VAR, url);

        Self {
            original,
            _guard: guard,
        }
    }

    fn unset() -> Self {
        let guard = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let original = std::env::var_os(REGISTRY_URL_ENV_VAR);
        std::env::remove_var(REGISTRY_URL_ENV_VAR);

        Self {
            original,
            _guard: guard,
        }
    }
}

impl Drop for RegistryUrlGuard {
    fn drop(&mut self) {
        match &self.original {
            Some(value) => std::env::set_var(REGISTRY_URL_ENV_VAR, value),
            None => std::env::remove_var(REGISTRY_URL_ENV_VAR),
        }
    }
}

#[derive(Clone)]
enum FakeHttpRoute {
    Response(Vec<u8>),
    NetworkError(String),
}

#[derive(Default)]
struct FakeHttpClient {
    routes: Mutex<BTreeMap<String, FakeHttpRoute>>,
    requests: Mutex<Vec<HttpRequest>>,
}

impl FakeHttpClient {
    fn with_text(&self, url: &str, body: impl Into<String>) {
        self.routes.lock().expect("routes lock").insert(
            url.to_string(),
            FakeHttpRoute::Response(body.into().into_bytes()),
        );
    }

    fn with_network_error(&self, url: &str, message: impl Into<String>) {
        self.routes
            .lock()
            .expect("routes lock")
            .insert(url.to_string(), FakeHttpRoute::NetworkError(message.into()));
    }

    fn requested_urls(&self) -> Vec<String> {
        self.requests
            .lock()
            .expect("requests lock")
            .iter()
            .map(|request| request.url.clone())
            .collect()
    }
}

impl HttpClient for FakeHttpClient {
    fn get(&self, request: HttpRequest) -> fontbrew_core::Result<HttpResponse> {
        self.requests
            .lock()
            .expect("requests lock")
            .push(request.clone());
        let body = self
            .routes
            .lock()
            .expect("routes lock")
            .get(&request.url)
            .cloned()
            .unwrap_or_else(|| panic!("unexpected HTTP request: {}", request.url));

        match body {
            FakeHttpRoute::Response(body) => Ok(HttpResponse { status: 200, body }),
            FakeHttpRoute::NetworkError(message) => Err(FontbrewError::Network { message }),
        }
    }

    fn download_to_file(
        &self,
        request: HttpRequest,
        _destination: &Path,
        _max_bytes: u64,
        _cancellation: &dyn CancellationToken,
    ) -> fontbrew_core::Result<u64> {
        panic!(
            "outdated should not download GitHub release assets: {}",
            request.url
        );
    }
}

fn test_paths(temp: &tempfile::TempDir) -> FontbrewPaths {
    FontbrewPaths::for_tests(
        temp.path().join("data"),
        temp.path().join("config"),
        temp.path().join("home"),
    )
}

fn package_id(id: &str) -> PackageId {
    PackageId::parse(id).expect("test package id should be valid")
}

fn github_releases_url(owner: &str, repo: &str) -> String {
    format!("https://api.github.com/repos/{owner}/{repo}/releases")
}

fn fontsource_list_url() -> String {
    "https://api.fontsource.org/v1/fonts".to_string()
}

fn fontsource_detail_url(id: &str) -> String {
    format!("https://api.fontsource.org/v1/fonts/{id}")
}

fn registry_snapshot_json() -> &'static str {
    r#"{
  "schemaVersion": 1,
  "updatedAt": "2026-07-03T00:00:00Z",
  "packages": {
    "inter": {
      "name": "Inter",
      "source": { "type": "github", "repo": "rsms/inter" },
      "families": ["Inter"]
    },
    "maple-mono": {
      "name": "Maple Mono NF CN",
      "source": { "type": "github", "repo": "subframe7536/maple-font" },
      "families": ["Maple Mono NF CN"]
    },
    "source-code-pro": {
      "name": "Source Code Pro",
      "source": { "type": "github", "repo": "adobe/source-code-pro" },
      "families": ["Source Code Pro"]
    }
  }
}"#
}

fn manifest_record(
    package_id_text: &str,
    version: &str,
    source: ManifestSource,
    update_source: Option<ManifestSource>,
) -> ManifestPackageRecord {
    let package_id = package_id(package_id_text);
    let version = PackageVersion::new(version);

    ManifestPackageRecord {
        package_id: package_id.clone(),
        version: version.clone(),
        source,
        update_source,
        families: vec![FamilyName::new(package_id.as_str().to_string())],
        font_files: Vec::new(),
        activation_artifacts: Vec::new(),
        installed_at: "unix:1".to_string(),
        active_version: Some(version),
    }
}

fn write_manifest(paths: &FontbrewPaths, records: Vec<ManifestPackageRecord>) {
    let mut manifest = ManifestV1::empty();
    for record in records {
        manifest.insert_package(record).expect("insert package");
    }

    ManifestStore::write(&paths.manifest_path(), &manifest).expect("write manifest");
}

#[test]
fn unprefixed_search_refreshes_registry_then_fetches_fontsource_fallback() {
    let _registry_url = RegistryUrlGuard::set(TEST_REGISTRY_URL);
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let fake_http = Arc::new(FakeHttpClient::default());
    fake_http.with_text(TEST_REGISTRY_URL, registry_snapshot_json());
    fake_http.with_text(
        &fontsource_list_url(),
        r#"[
  {
    "id": "abel",
    "family": "Abel",
    "subsets": ["latin"],
    "weights": [400],
    "styles": ["normal"],
    "lastModified": "2025-05-30",
    "license": "OFL-1.1",
    "type": "google"
  }
]"#,
    );
    fake_http.with_text(
        &fontsource_detail_url("abel"),
        r#"{
  "id": "abel",
  "family": "Abel",
  "subsets": ["latin"],
  "weights": [400],
  "styles": ["normal"],
  "lastModified": "2025-05-30",
  "version": "v18",
  "license": "OFL-1.1",
  "variants": {
    "400": {
      "normal": {
        "latin": {
          "url": {
            "ttf": "https://cdn.example/abel.ttf"
          }
        }
      }
    }
  }
}"#,
    );
    let app = FontbrewApp::with_paths_and_http_client(paths.clone(), fake_http.clone());

    let report = app
        .search(SearchRequest {
            query: "Abel".to_string(),
            limit: Some(1),
        })
        .expect("unprefixed search should refresh registry and fetch Fontsource fallback");

    assert_eq!(report.results.len(), 1);
    assert_eq!(report.results[0].package_id, package_id("abel"));
    assert_eq!(report.results[0].source, "fontsource:abel");
    assert_eq!(
        fake_http.requested_urls(),
        vec![
            TEST_REGISTRY_URL.to_string(),
            fontsource_list_url(),
            fontsource_detail_url("abel"),
        ]
    );
}

#[test]
fn unprefixed_search_skips_registry_when_registry_url_is_not_configured() {
    let _registry_url = RegistryUrlGuard::unset();
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let fake_http = Arc::new(FakeHttpClient::default());
    fake_http.with_text(
        &fontsource_list_url(),
        r#"[
  {
    "id": "inter",
    "family": "Inter",
    "subsets": ["latin"],
    "weights": [400],
    "styles": ["normal"],
    "lastModified": "2025-05-30",
    "license": "OFL-1.1",
    "type": "google"
  }
]"#,
    );
    fake_http.with_text(
        &fontsource_detail_url("inter"),
        r#"{
  "id": "inter",
  "family": "Inter",
  "subsets": ["latin"],
  "weights": [400],
  "styles": ["normal"],
  "lastModified": "2025-05-30",
  "version": "v4",
  "license": "OFL-1.1",
  "variants": {
    "400": {
      "normal": {
        "latin": {
          "url": {
            "ttf": "https://cdn.example/inter.ttf"
          }
        }
      }
    }
  }
}"#,
    );
    let app = FontbrewApp::with_paths_and_http_client(paths.clone(), fake_http.clone());

    let report = app
        .search(SearchRequest {
            query: "iner".to_string(),
            limit: Some(1),
        })
        .expect("search should skip registry when registry URL is not configured");

    assert_eq!(report.results.len(), 1);
    assert_eq!(report.results[0].package_id, package_id("inter"));
    assert_eq!(report.results[0].source, "fontsource:inter");
    assert_eq!(
        fake_http.requested_urls(),
        vec![fontsource_list_url(), fontsource_detail_url("inter")]
    );
    assert!(paths.registry_snapshot_path().exists());
}

#[test]
fn unprefixed_search_uses_provider_fallback_when_registry_refresh_fails_without_snapshot() {
    let _registry_url = RegistryUrlGuard::set(TEST_REGISTRY_URL);
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let fake_http = Arc::new(FakeHttpClient::default());
    fake_http.with_network_error(TEST_REGISTRY_URL, "registry unavailable");
    fake_http.with_text(
        &fontsource_list_url(),
        r#"[
  {
    "id": "inter",
    "family": "Inter",
    "subsets": ["latin"],
    "weights": [400],
    "styles": ["normal"],
    "lastModified": "2025-05-30",
    "license": "OFL-1.1",
    "type": "google"
  }
]"#,
    );
    fake_http.with_text(
        &fontsource_detail_url("inter"),
        r#"{
  "id": "inter",
  "family": "Inter",
  "subsets": ["latin"],
  "weights": [400],
  "styles": ["normal"],
  "lastModified": "2025-05-30",
  "version": "v4",
  "license": "OFL-1.1",
  "variants": {
    "400": {
      "normal": {
        "latin": {
          "url": {
            "ttf": "https://cdn.example/inter.ttf"
          }
        }
      }
    }
  }
}"#,
    );
    let app = FontbrewApp::with_paths_and_http_client(paths.clone(), fake_http.clone());

    let report = app
        .search(SearchRequest {
            query: "Inter".to_string(),
            limit: Some(1),
        })
        .expect("search should use provider fallback when registry refresh fails");

    assert_eq!(report.results.len(), 1);
    assert_eq!(report.results[0].package_id, package_id("inter"));
    assert_eq!(report.results[0].source, "fontsource:inter");
    assert_eq!(
        fake_http.requested_urls(),
        vec![
            TEST_REGISTRY_URL.to_string(),
            fontsource_list_url(),
            fontsource_detail_url("inter"),
        ]
    );
    assert!(paths.registry_snapshot_path().exists());
}

#[test]
fn unprefixed_search_uses_cached_registry_snapshot_when_refresh_fails() {
    let _registry_url = RegistryUrlGuard::set(TEST_REGISTRY_URL);
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let snapshot = RegistrySnapshotStore::parse(registry_snapshot_json()).expect("parse snapshot");
    RegistrySnapshotStore::new(paths.clone())
        .write_snapshot(&snapshot)
        .expect("write cached registry snapshot");
    let fake_http = Arc::new(FakeHttpClient::default());
    fake_http.with_network_error(TEST_REGISTRY_URL, "registry unavailable");
    let app = FontbrewApp::with_paths_and_http_client(paths, fake_http.clone());

    let report = app
        .search(SearchRequest {
            query: "inter".to_string(),
            limit: Some(1),
        })
        .expect("search should use cached registry snapshot when refresh fails");

    assert_eq!(report.results.len(), 1);
    assert_eq!(report.results[0].package_id, package_id("inter"));
    assert_eq!(report.results[0].source, "registry:inter");
    assert_eq!(
        fake_http.requested_urls(),
        vec![TEST_REGISTRY_URL.to_string()]
    );
}

#[test]
fn search_refreshes_registry_snapshot_with_fuzzy_matching_and_limit() {
    let _registry_url = RegistryUrlGuard::set(TEST_REGISTRY_URL);
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let fake_http = Arc::new(FakeHttpClient::default());
    fake_http.with_text(TEST_REGISTRY_URL, registry_snapshot_json());
    let app = FontbrewApp::with_paths_and_http_client(paths, fake_http.clone());

    let report = app
        .search(SearchRequest {
            query: "code".to_string(),
            limit: Some(1),
        })
        .expect("search registry snapshot");

    assert_eq!(report.results.len(), 1);
    assert_eq!(report.results[0].package_id, package_id("source-code-pro"));
    assert_eq!(report.results[0].display_name, "Source Code Pro");
    assert_eq!(report.results[0].families[0].as_str(), "Source Code Pro");

    let family_report = app
        .search(SearchRequest {
            query: "MPLE MONO".to_string(),
            limit: Some(1),
        })
        .expect("search registry families");

    assert_eq!(family_report.results.len(), 1);
    assert_eq!(
        family_report.results[0].package_id,
        package_id("maple-mono")
    );

    let limited_report = app
        .search(SearchRequest {
            query: String::new(),
            limit: Some(2),
        })
        .expect("empty query returns registry candidates");

    assert_eq!(limited_report.results.len(), 2);
    assert_eq!(
        fake_http.requested_urls(),
        vec![
            TEST_REGISTRY_URL.to_string(),
            TEST_REGISTRY_URL.to_string(),
            TEST_REGISTRY_URL.to_string(),
        ]
    );
}

#[test]
fn search_rejects_invalid_refreshed_registry_snapshot_before_use() {
    let _registry_url = RegistryUrlGuard::set(TEST_REGISTRY_URL);
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let fake_http = Arc::new(FakeHttpClient::default());
    fake_http.with_text(
        TEST_REGISTRY_URL,
        r#"{"schemaVersion": 1, "updatedAt": "2026-07-03T00:00:00Z", "packages": {"bad": {"name": "Bad", "source": {"type": "github", "repo": "owner//repo"}, "families": ["Bad"]}}}"#,
    );
    let app = FontbrewApp::with_paths_and_http_client(paths, fake_http);

    let error = app
        .search(SearchRequest {
            query: "bad".to_string(),
            limit: None,
        })
        .expect_err("invalid registry snapshot should fail");

    assert!(matches!(
        error,
        FontbrewError::RegistryValidationFailed { .. }
    ));
}

#[test]
fn outdated_reports_newer_github_releases_and_local_packages_without_update_sources() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    write_manifest(
        &paths,
        vec![
            manifest_record(
                "source-code-pro",
                "v1.0.0",
                ManifestSource::GitHub {
                    owner: "adobe".to_string(),
                    repo: "source-code-pro".to_string(),
                },
                None,
            ),
            manifest_record(
                "inter",
                "v4.0.0",
                ManifestSource::Registry {
                    id: "inter".to_string(),
                },
                Some(ManifestSource::GitHub {
                    owner: "rsms".to_string(),
                    repo: "inter".to_string(),
                }),
            ),
            manifest_record(
                "up-to-date",
                "v2.0.0",
                ManifestSource::GitHub {
                    owner: "owner".to_string(),
                    repo: "up-to-date".to_string(),
                },
                None,
            ),
            manifest_record(
                "local-only",
                "local",
                ManifestSource::LocalArchive {
                    path: PathBuf::from("/tmp/local.zip"),
                },
                None,
            ),
        ],
    );
    let fake_http = Arc::new(FakeHttpClient::default());
    fake_http.with_text(
        &github_releases_url("adobe", "source-code-pro"),
        r#"[{"tag_name":"v1.2.0","draft":false,"prerelease":false,"assets":[]}]"#,
    );
    fake_http.with_text(
        &github_releases_url("rsms", "inter"),
        r#"[{"tag_name":"v4.1.0","draft":false,"prerelease":false,"assets":[]}]"#,
    );
    fake_http.with_text(
        &github_releases_url("owner", "up-to-date"),
        r#"[{"tag_name":"v2.0.0","draft":false,"prerelease":false,"assets":[]}]"#,
    );
    let app = FontbrewApp::with_paths_and_http_client(paths, fake_http.clone());

    let report = app
        .outdated(OutdatedRequest {
            package_ids: Vec::new(),
        })
        .expect("check outdated packages");

    assert_eq!(report.packages.len(), 2);
    assert_eq!(report.packages[0].package_id, package_id("inter"));
    assert_eq!(report.packages[0].current_version.as_str(), "v4.0.0");
    assert_eq!(report.packages[0].latest_version.as_str(), "v4.1.0");
    assert_eq!(report.packages[1].package_id, package_id("source-code-pro"));
    assert_eq!(report.packages[1].latest_version.as_str(), "v1.2.0");
    assert_eq!(report.not_updatable.len(), 1);
    assert_eq!(report.not_updatable[0].package_id, package_id("local-only"));
    assert!(report.not_updatable[0]
        .reason
        .contains("no GitHub update source"));
    assert_eq!(
        fake_http.requested_urls(),
        vec![
            github_releases_url("rsms", "inter"),
            github_releases_url("adobe", "source-code-pro"),
            github_releases_url("owner", "up-to-date"),
        ]
    );
}
