use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use fontbrew_core::{
    fetch::{HttpClient, HttpRequest, HttpResponse},
    manifest::{ManifestPackageRecord, ManifestSource, ManifestStore, ManifestV1},
    platform::FontbrewPaths,
    CancellationToken, FamilyName, FontbrewApp, OutdatedRequest, PackageId, PackageVersion,
    SearchRequest,
};

#[derive(Clone)]
enum FakeHttpRoute {
    Response(Vec<u8>),
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
fn unprefixed_search_fetches_fontsource_results() {
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
    "type": "fontsource"
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
        .expect("search should fetch Fontsource metadata");

    assert_eq!(report.results.len(), 1);
    assert_eq!(report.results[0].package_id, package_id("inter"));
    assert_eq!(report.results[0].source, "fontsource:inter");
    assert_eq!(
        fake_http.requested_urls(),
        vec![fontsource_list_url(), fontsource_detail_url("inter")]
    );
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
                ManifestSource::GitHub {
                    owner: "rsms".to_string(),
                    repo: "inter".to_string(),
                },
                None,
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
    assert!(report.not_updatable[0].reason.contains("no update source"));
    assert_eq!(
        fake_http.requested_urls(),
        vec![
            github_releases_url("rsms", "inter"),
            github_releases_url("adobe", "source-code-pro"),
            github_releases_url("owner", "up-to-date"),
        ]
    );
}
