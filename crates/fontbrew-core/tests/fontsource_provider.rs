use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use fontbrew_core::{
    fetch::{HttpClient, HttpRequest, HttpResponse},
    manifest::{ManifestSource, ManifestStore},
    platform::FontbrewPaths,
    registry::{RegistrySnapshotStore, RegistrySnapshotV1},
    CancellationToken, ExecutionPolicy, FontbrewApp, FontbrewError, InfoRequest, InstallRequest,
    InstallSource, PackageId, ProgressEvent, ProgressSink, ProviderKind, SearchRequest,
};

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

#[derive(Default)]
struct FakeHttpClient {
    routes: Mutex<BTreeMap<String, Vec<u8>>>,
    download_routes: Mutex<BTreeMap<String, Vec<u8>>>,
    requests: Mutex<Vec<HttpRequest>>,
    download_targets: Mutex<Vec<PathBuf>>,
}

impl FakeHttpClient {
    fn with_text(&self, url: &str, body: impl Into<String>) {
        self.routes
            .lock()
            .expect("routes lock")
            .insert(url.to_string(), body.into().into_bytes());
    }

    fn with_download_bytes(&self, url: &str, body: Vec<u8>) {
        self.download_routes
            .lock()
            .expect("download routes lock")
            .insert(url.to_string(), body);
    }

    fn requested_urls(&self) -> Vec<String> {
        self.requests
            .lock()
            .expect("requests lock")
            .iter()
            .map(|request| request.url.clone())
            .collect()
    }

    fn download_targets(&self) -> Vec<PathBuf> {
        self.download_targets
            .lock()
            .expect("download targets lock")
            .clone()
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
        destination: &Path,
        max_bytes: u64,
        _cancellation: &dyn CancellationToken,
    ) -> fontbrew_core::Result<u64> {
        self.requests
            .lock()
            .expect("requests lock")
            .push(request.clone());
        let body = self
            .download_routes
            .lock()
            .expect("download routes lock")
            .get(&request.url)
            .cloned()
            .unwrap_or_else(|| panic!("unexpected HTTP download request: {}", request.url));

        if body.len() as u64 > max_bytes {
            return Err(FontbrewError::ArchiveRejected {
                reason: format!(
                    "download exceeds maximum size of {max_bytes} bytes: {}",
                    request.url
                ),
            });
        }

        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).expect("create download parent");
        }
        let mut file = File::create(destination).expect("create download destination");
        file.write_all(&body).expect("write fake download");
        self.download_targets
            .lock()
            .expect("download targets lock")
            .push(destination.to_path_buf());

        Ok(body.len() as u64)
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

fn write_empty_registry_snapshot(paths: &FontbrewPaths) {
    let snapshot = RegistrySnapshotV1::parse(
        r#"{
  "schemaVersion": 1,
  "updatedAt": "2026-07-04T00:00:00Z",
  "packages": {}
}"#,
    )
    .expect("parse empty registry snapshot");

    RegistrySnapshotStore::new(paths.clone())
        .write_snapshot(&snapshot)
        .expect("write registry snapshot");
}

fn fixture_font_bytes(filename: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/fonts")
        .join(filename);
    let mut file = File::open(path).expect("open fixture font");
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).expect("read fixture font");
    bytes
}

fn fontsource_list_url(query: &str) -> String {
    format!("https://api.fontsource.org/v1/fonts?family={query}")
}

fn fontsource_detail_url(id: &str) -> String {
    format!("https://api.fontsource.org/v1/fonts/{id}")
}

fn provider_metadata_files(paths: &FontbrewPaths) -> Vec<PathBuf> {
    if !paths.provider_metadata_dir().exists() {
        return Vec::new();
    }

    let mut files = Vec::new();
    collect_files(&paths.provider_metadata_dir(), &mut files);
    files.sort();
    files
}

fn collect_files(dir: &Path, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("read metadata dir") {
        let entry = entry.expect("read metadata entry");
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, files);
        } else {
            files.push(path);
        }
    }
}

fn assert_provider_metadata_has_no_font_binaries(paths: &FontbrewPaths) {
    for file in provider_metadata_files(paths) {
        let extension = file
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        assert!(
            !matches!(
                extension.as_str(),
                "ttf" | "otf" | "ttc" | "otc" | "woff" | "woff2"
            ),
            "provider metadata must not cache font binaries: {}",
            file.display()
        );
    }
}

#[test]
fn fontsource_search_returns_only_results_with_desktop_urls_and_writes_metadata_snapshots() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    write_empty_registry_snapshot(&paths);
    let fake_http = Arc::new(FakeHttpClient::default());
    fake_http.with_text(
        &fontsource_list_url("Abel"),
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
  },
  {
    "id": "web-only",
    "family": "Web Only",
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
            "woff2": "https://cdn.example/abel.woff2",
            "woff": "https://cdn.example/abel.woff",
            "ttf": "https://cdn.example/abel.ttf"
          }
        }
      }
    }
  }
}"#,
    );
    fake_http.with_text(
        &fontsource_detail_url("web-only"),
        r#"{
  "id": "web-only",
  "family": "Web Only",
  "subsets": ["latin"],
  "weights": [400],
  "styles": ["normal"],
  "lastModified": "2025-05-30",
  "version": "v1",
  "license": "OFL-1.1",
  "variants": {
    "400": {
      "normal": {
        "latin": {
          "url": {
            "woff2": "https://cdn.example/web-only.woff2",
            "woff": "https://cdn.example/web-only.woff"
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
            limit: None,
            refresh: false,
            offline: false,
        })
        .expect("search Fontsource");

    assert_eq!(report.results.len(), 1);
    assert_eq!(report.results[0].package_id, package_id("abel"));
    assert_eq!(report.results[0].display_name, "Abel");
    assert_eq!(report.results[0].source, "fontsource:abel");
    assert_eq!(
        report.results[0]
            .version
            .as_ref()
            .expect("version")
            .as_str(),
        "v18"
    );
    assert_eq!(
        fake_http.requested_urls(),
        vec![
            fontsource_list_url("Abel"),
            fontsource_detail_url("abel"),
            fontsource_detail_url("web-only"),
        ]
    );
    assert!(!provider_metadata_files(&paths).is_empty());
    assert!(provider_metadata_files(&paths).iter().all(|file| {
        file.extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension == "json")
    }));
    assert_provider_metadata_has_no_font_binaries(&paths);
    assert!(!paths.staging_dir().exists());
    assert!(!paths.managed_store_dir().join("packages").exists());
}

#[test]
fn fontsource_offline_search_uses_metadata_snapshots_without_network() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    write_empty_registry_snapshot(&paths);
    let fake_http = Arc::new(FakeHttpClient::default());
    fake_http.with_text(
        &fontsource_list_url("Abel"),
        r#"[{
  "id": "abel",
  "family": "Abel",
  "subsets": ["latin"],
  "weights": [400],
  "styles": ["normal"],
  "lastModified": "2025-05-30",
  "license": "OFL-1.1",
  "type": "google"
}]"#,
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
    app.search(SearchRequest {
        query: "Abel".to_string(),
        limit: None,
        refresh: false,
        offline: false,
    })
    .expect("prime Fontsource metadata snapshot");

    let offline_http = Arc::new(FakeHttpClient::default());
    let app = FontbrewApp::with_paths_and_http_client(paths, offline_http.clone());
    let report = app
        .search(SearchRequest {
            query: "Abel".to_string(),
            limit: None,
            refresh: false,
            offline: true,
        })
        .expect("offline search should use Fontsource metadata snapshot");

    assert_eq!(report.results.len(), 1);
    assert_eq!(report.results[0].source, "fontsource:abel");
    assert!(offline_http.requested_urls().is_empty());
}

#[test]
fn fontsource_install_downloads_desktop_font_and_records_provider_manifest_source() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let fake_http = Arc::new(FakeHttpClient::default());
    fake_http.with_text(
        &fontsource_detail_url("source-code-pro"),
        r#"{
  "id": "source-code-pro",
  "family": "Source Code Pro",
  "subsets": ["latin"],
  "weights": [400],
  "styles": ["normal"],
  "lastModified": "2025-05-30",
  "version": "v2",
  "license": "OFL-1.1",
  "variants": {
    "400": {
      "normal": {
        "latin": {
          "url": {
            "woff2": "https://cdn.example/source-code-pro.woff2",
            "ttf": "https://cdn.example/source-code-pro.ttf"
          }
        }
      }
    }
  }
}"#,
    );
    fake_http.with_download_bytes(
        "https://cdn.example/source-code-pro.ttf",
        fixture_font_bytes("SourceCodePro-Regular.ttf"),
    );
    let app = FontbrewApp::with_paths_and_http_client(paths.clone(), fake_http.clone());

    let plan = app
        .install_plan(InstallRequest {
            source: InstallSource::Provider {
                provider: ProviderKind::Fontsource,
                id: "source-code-pro".to_string(),
            },
            format_preference: Vec::new(),
            asset_selector: None,
            reinstall: false,
            refresh: false,
            offline: false,
        })
        .expect("plan Fontsource install");

    assert_eq!(plan.package_id, package_id("source-code-pro"));
    assert_eq!(
        plan.target_version
            .as_ref()
            .expect("target version")
            .as_str(),
        "v2"
    );
    assert_eq!(
        fake_http.requested_urls(),
        vec![
            fontsource_detail_url("source-code-pro"),
            "https://cdn.example/source-code-pro.ttf".to_string(),
        ]
    );
    let download_targets = fake_http.download_targets();
    assert_eq!(download_targets.len(), 1);
    assert!(download_targets[0].starts_with(paths.staging_dir()));
    assert_eq!(
        download_targets[0]
            .extension()
            .and_then(|extension| extension.to_str()),
        Some("ttf")
    );

    let report = app
        .apply_install(
            plan,
            ExecutionPolicy::SafeOnly,
            &mut NoProgress,
            &NeverCancelled,
        )
        .expect("apply Fontsource install");

    assert_eq!(report.package_id, package_id("source-code-pro"));
    assert_eq!(report.installed_version.as_str(), "v2");
    let manifest = ManifestStore::read_or_empty(&paths.manifest_path()).expect("read manifest");
    let record = manifest
        .get_package(&package_id("source-code-pro"))
        .expect("manifest record");
    assert_eq!(
        record.source,
        ManifestSource::Provider {
            provider: ProviderKind::Fontsource,
            id: "source-code-pro".to_string(),
        }
    );
    assert_eq!(record.update_source, None);
    let info = app
        .package_info(InfoRequest {
            package_id: package_id("source-code-pro"),
        })
        .expect("read Fontsource package info");
    assert_eq!(info.package.source, "fontsource:source-code-pro");
    assert!(record.font_files.iter().all(|font_file| font_file
        .path
        .starts_with(paths.managed_store_dir().join("packages"))));
    assert_provider_metadata_has_no_font_binaries(&paths);
    assert!(
        !paths.staging_dir().exists()
            || fs::read_dir(paths.staging_dir())
                .expect("read staging dir")
                .next()
                .is_none()
    );
}
