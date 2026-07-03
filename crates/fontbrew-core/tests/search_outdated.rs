use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use fontbrew_core::{
    fetch::{HttpClient, HttpRequest, HttpResponse},
    manifest::{ManifestPackageRecord, ManifestSource, ManifestStore, ManifestV1},
    platform::FontbrewPaths,
    registry::{RegistrySnapshotStore, RegistrySnapshotV1},
    FamilyName, FontbrewApp, FontbrewError, OutdatedRequest, PackageId, PackageVersion,
    SearchRequest,
};

#[derive(Default)]
struct FakeHttpClient {
    routes: Mutex<BTreeMap<String, Vec<u8>>>,
    requests: Mutex<Vec<HttpRequest>>,
}

impl FakeHttpClient {
    fn with_text(&self, url: &str, body: impl Into<String>) {
        self.routes
            .lock()
            .expect("routes lock")
            .insert(url.to_string(), body.into().into_bytes());
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

        Ok(HttpResponse { status: 200, body })
    }

    fn download_to_file(
        &self,
        request: HttpRequest,
        _destination: &Path,
        _max_bytes: u64,
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

fn write_registry_snapshot(paths: &FontbrewPaths) {
    let snapshot = RegistrySnapshotV1::parse(
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
}"#,
    )
    .expect("parse registry snapshot");

    RegistrySnapshotStore::new(paths.clone())
        .write_snapshot(&snapshot)
        .expect("write registry snapshot");
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
fn search_uses_local_registry_snapshot_with_case_insensitive_matching_and_limit() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    write_registry_snapshot(&paths);
    let app = FontbrewApp::with_paths(paths);

    let report = app
        .search(SearchRequest {
            query: "code".to_string(),
            limit: Some(10),
            refresh: false,
            offline: true,
        })
        .expect("search registry snapshot");

    assert_eq!(report.results.len(), 1);
    assert_eq!(report.results[0].package_id, package_id("source-code-pro"));
    assert_eq!(report.results[0].display_name, "Source Code Pro");
    assert_eq!(report.results[0].families[0].as_str(), "Source Code Pro");

    let family_report = app
        .search(SearchRequest {
            query: "MAPLE MONO".to_string(),
            limit: None,
            refresh: false,
            offline: true,
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
            refresh: false,
            offline: true,
        })
        .expect("empty query returns registry candidates");

    assert_eq!(limited_report.results.len(), 2);
}

#[test]
fn search_rejects_invalid_registry_snapshot_before_use() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    fs::create_dir_all(paths.managed_store_dir()).expect("create data root");
    fs::write(
        paths.registry_snapshot_path(),
        r#"{"schemaVersion": 1, "updatedAt": "2026-07-03T00:00:00Z", "packages": {"bad": {"name": "Bad", "source": {"type": "github", "repo": "owner//repo"}, "families": ["Bad"]}}}"#,
    )
    .expect("write invalid snapshot");
    let app = FontbrewApp::with_paths(paths);

    let error = app
        .search(SearchRequest {
            query: "bad".to_string(),
            limit: None,
            refresh: false,
            offline: true,
        })
        .expect_err("invalid registry snapshot should fail");

    assert!(matches!(
        error,
        FontbrewError::RegistryValidationFailed { .. }
    ));
}

#[test]
fn search_rejects_refresh_with_offline() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let app = FontbrewApp::with_paths(paths);

    let error = app
        .search(SearchRequest {
            query: String::new(),
            limit: None,
            refresh: true,
            offline: true,
        })
        .expect_err("refresh cannot run in offline mode");

    assert!(matches!(error, FontbrewError::Config { .. }));
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
            offline: false,
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

#[test]
fn outdated_offline_reports_github_packages_as_not_updatable_without_network() {
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
    let app = FontbrewApp::with_paths_and_http_client(paths, fake_http.clone());

    let report = app
        .outdated(OutdatedRequest {
            package_ids: Vec::new(),
            offline: true,
        })
        .expect("offline outdated should report without network");

    assert!(report.packages.is_empty());
    assert_eq!(report.not_updatable.len(), 2);
    assert_eq!(report.not_updatable[0].package_id, package_id("local-only"));
    assert_eq!(
        report.not_updatable[1].package_id,
        package_id("source-code-pro")
    );
    assert!(report.not_updatable[1].reason.contains("offline"));
    assert!(fake_http.requested_urls().is_empty());
}
