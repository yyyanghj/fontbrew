use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{Cursor, Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

use fontbrew_core::{
    fetch::{HttpClient, HttpRequest, HttpResponse},
    manifest::{ManifestSource, ManifestStore},
    platform::FontbrewPaths,
    registry::OFFICIAL_REGISTRY_URL,
    CancellationToken, ExecutionPolicy, FontbrewApp, FontbrewError, InstallRequest, InstallSource,
    PackageId, ProgressEvent, ProgressSink,
};
use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

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
    download_routes: Mutex<BTreeMap<String, FakeDownloadRoute>>,
    requests: Mutex<Vec<HttpRequest>>,
    download_targets: Mutex<Vec<PathBuf>>,
}

#[derive(Clone)]
struct FakeDownloadRoute {
    status: u16,
    content_length: Option<u64>,
    body: Vec<u8>,
    cancel_after_chunks: Option<usize>,
    cancel_flag: Option<Arc<AtomicBool>>,
}

impl FakeHttpClient {
    fn with_text(&self, url: &str, body: impl Into<String>) {
        self.routes
            .lock()
            .expect("routes lock")
            .insert(url.to_string(), body.into().into_bytes());
    }

    fn with_download_bytes(&self, url: &str, body: Vec<u8>) {
        self.with_download(url, 200, Some(body.len() as u64), body);
    }

    fn with_download_content_length(&self, url: &str, content_length: u64) {
        self.with_download(url, 200, Some(content_length), Vec::new());
    }

    fn with_download(&self, url: &str, status: u16, content_length: Option<u64>, body: Vec<u8>) {
        self.download_routes.lock().expect("routes lock").insert(
            url.to_string(),
            FakeDownloadRoute {
                status,
                content_length,
                body,
                cancel_after_chunks: None,
                cancel_flag: None,
            },
        );
    }

    fn with_cancelling_download_bytes(
        &self,
        url: &str,
        body: Vec<u8>,
        cancel_after_chunks: usize,
        cancel_flag: Arc<AtomicBool>,
    ) {
        self.download_routes.lock().expect("routes lock").insert(
            url.to_string(),
            FakeDownloadRoute {
                status: 200,
                content_length: Some(body.len() as u64),
                body,
                cancel_after_chunks: Some(cancel_after_chunks),
                cancel_flag: Some(cancel_flag),
            },
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

    fn requested_requests(&self) -> Vec<HttpRequest> {
        self.requests.lock().expect("requests lock").clone()
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
            .or_else(|| {
                if self
                    .download_routes
                    .lock()
                    .expect("download routes lock")
                    .contains_key(&request.url)
                {
                    panic!(
                        "download asset should be streamed to a file: {}",
                        request.url
                    );
                }

                None
            })
            .unwrap_or_else(|| panic!("unexpected HTTP request: {}", request.url));

        Ok(HttpResponse { status: 200, body })
    }

    fn download_to_file(
        &self,
        request: HttpRequest,
        destination: &Path,
        max_bytes: u64,
        cancellation: &dyn CancellationToken,
    ) -> fontbrew_core::Result<u64> {
        self.requests
            .lock()
            .expect("requests lock")
            .push(request.clone());
        let route = self
            .download_routes
            .lock()
            .expect("download routes lock")
            .get(&request.url)
            .cloned()
            .unwrap_or_else(|| panic!("unexpected HTTP download request: {}", request.url));

        if !(200..300).contains(&route.status) {
            return Err(FontbrewError::Network {
                message: format!(
                    "HTTP request failed with status {} for {}",
                    route.status, request.url
                ),
            });
        }

        if route
            .content_length
            .is_some_and(|length| length > max_bytes)
        {
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
        let mut written = 0_u64;
        for (chunk_index, chunk) in route.body.chunks(7).enumerate() {
            if cancellation.is_cancelled() {
                let _ = fs::remove_file(destination);
                return Err(FontbrewError::Cancelled);
            }

            let next = written + chunk.len() as u64;
            if next > max_bytes {
                let _ = fs::remove_file(destination);
                return Err(FontbrewError::ArchiveRejected {
                    reason: format!(
                        "download exceeds maximum size of {max_bytes} bytes: {}",
                        request.url
                    ),
                });
            }

            file.write_all(chunk).expect("write fake download chunk");
            written = next;
            if route.cancel_after_chunks == Some(chunk_index + 1) {
                if let Some(cancel_flag) = &route.cancel_flag {
                    cancel_flag.store(true, Ordering::SeqCst);
                }
            }
        }

        self.download_targets
            .lock()
            .expect("download targets lock")
            .push(destination.to_path_buf());
        Ok(written)
    }
}

struct AtomicCancellation {
    flag: Arc<AtomicBool>,
}

impl CancellationToken for AtomicCancellation {
    fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }
}

struct CancelWhenInstallStagingExists {
    paths: FontbrewPaths,
}

impl CancellationToken for CancelWhenInstallStagingExists {
    fn is_cancelled(&self) -> bool {
        staging_entries(&self.paths)
            .iter()
            .any(|entry| entry.starts_with("install-"))
    }
}

fn package_id(id: &str) -> PackageId {
    PackageId::parse(id).expect("test package id should be valid")
}

fn fixture_font_path(filename: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/fonts")
        .join(filename)
}

fn test_paths(temp: &tempfile::TempDir) -> FontbrewPaths {
    FontbrewPaths::for_tests(
        temp.path().join("data"),
        temp.path().join("config"),
        temp.path().join("home"),
    )
}

fn staging_entries(paths: &FontbrewPaths) -> Vec<String> {
    if !paths.staging_dir().exists() {
        return Vec::new();
    }

    let mut entries = fs::read_dir(paths.staging_dir())
        .expect("read staging root")
        .map(|entry| {
            entry
                .expect("read staging entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect::<Vec<_>>();
    entries.sort();
    entries
}

fn github_request(owner: &str, repo: &str, asset_selector: Option<&str>) -> InstallRequest {
    InstallRequest {
        source: InstallSource::GitHubRepo {
            owner: owner.to_string(),
            repo: repo.to_string(),
        },
        package_id_override: None,
        format_preference: Vec::new(),
        asset_selector: asset_selector.map(str::to_string),
        reinstall: false,
    }
}

fn registry_request(short_name: &str) -> InstallRequest {
    InstallRequest {
        source: InstallSource::RegistryName(short_name.to_string()),
        package_id_override: None,
        format_preference: Vec::new(),
        asset_selector: None,
        reinstall: false,
    }
}

fn github_releases_url(owner: &str, repo: &str) -> String {
    format!("https://api.github.com/repos/{owner}/{repo}/releases")
}

fn zip_with_fixture_font(entry_name: &str, fixture_name: &str) -> Vec<u8> {
    zip_with_fixture_fonts(&[(entry_name, fixture_name)])
}

fn zip_with_fixture_fonts(entries: &[(&str, &str)]) -> Vec<u8> {
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));

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

    zip.finish().expect("finish zip").into_inner()
}

fn apply_plan(app: &FontbrewApp, plan: fontbrew_core::InstallPlan) -> fontbrew_core::InstallReport {
    let mut progress = NoProgress;
    app.apply_install(
        plan,
        ExecutionPolicy::SafeOnly,
        &mut progress,
        &NeverCancelled,
    )
    .expect("apply install")
}

#[test]
fn direct_github_install_selects_latest_stable_release_and_records_github_source() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let fake_http = Arc::new(FakeHttpClient::default());
    fake_http.with_text(
        &github_releases_url("adobe", "source-code-pro"),
        r#"[
  {
    "tag_name": "v3.0.0",
    "draft": true,
    "prerelease": false,
    "assets": [
      {"name": "draft.zip", "browser_download_url": "https://downloads.example/draft.zip"}
    ]
  },
  {
    "tag_name": "v2.0.0-beta.1",
    "draft": false,
    "prerelease": true,
    "assets": [
      {"name": "beta.zip", "browser_download_url": "https://downloads.example/beta.zip"}
    ]
  },
  {
    "tag_name": "v1.2.3",
    "draft": false,
    "prerelease": false,
    "assets": [
      {
        "name": "source-code-pro.zip",
        "browser_download_url": "https://downloads.example/source-code-pro.zip"
      }
    ]
  }
]"#,
    );
    fake_http.with_download_bytes(
        "https://downloads.example/source-code-pro.zip",
        zip_with_fixture_font("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf"),
    );
    let app = FontbrewApp::with_paths_and_http_client(paths.clone(), fake_http.clone());

    let plan = app
        .install_plan(github_request("adobe", "source-code-pro", None))
        .expect("plan GitHub install");
    assert_eq!(plan.package_id, package_id("source-code-pro"));
    assert_eq!(
        plan.target_version
            .as_ref()
            .expect("target version")
            .as_str(),
        "v1.2.3"
    );
    assert_eq!(
        fake_http.requested_urls(),
        vec![
            github_releases_url("adobe", "source-code-pro"),
            "https://downloads.example/source-code-pro.zip".to_string(),
        ]
    );
    let download_targets = fake_http.download_targets();
    assert_eq!(download_targets.len(), 1);
    assert!(download_targets[0].starts_with(paths.staging_dir()));
    assert_eq!(
        download_targets[0]
            .file_name()
            .expect("download filename")
            .to_string_lossy(),
        "download.zip"
    );
    assert!(download_targets[0].exists());

    let report = apply_plan(&app, plan);
    assert_eq!(report.installed_version.as_str(), "v1.2.3");
    assert_eq!(report.families[0].as_str(), "Source Code Pro");

    let manifest = ManifestStore::read_or_empty(&paths.manifest_path()).expect("read manifest");
    let record = manifest
        .get_package(&package_id("source-code-pro"))
        .expect("manifest record");
    assert_eq!(
        record.source,
        ManifestSource::GitHub {
            owner: "adobe".to_string(),
            repo: "source-code-pro".to_string(),
        }
    );
    assert_eq!(
        record.update_source,
        Some(ManifestSource::GitHub {
            owner: "adobe".to_string(),
            repo: "source-code-pro".to_string(),
        })
    );
}

#[test]
fn direct_github_install_rejects_multiple_families_without_boundary() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let fake_http = Arc::new(FakeHttpClient::default());
    fake_http.with_text(
        &github_releases_url("adobe", "source-code-pro"),
        r#"[
  {
    "tag_name": "v1.2.3",
    "draft": false,
    "prerelease": false,
    "assets": [
      {
        "name": "source-code-pro.zip",
        "browser_download_url": "https://downloads.example/source-code-pro.zip"
      }
    ]
  }
]"#,
    );
    fake_http.with_download_bytes(
        "https://downloads.example/source-code-pro.zip",
        zip_with_fixture_fonts(&[
            ("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf"),
            ("Inter-Variable.ttf", "Inter-Variable.ttf"),
        ]),
    );
    let app = FontbrewApp::with_paths_and_http_client(paths.clone(), fake_http);

    let error = app
        .install_plan(github_request("adobe", "source-code-pro", None))
        .expect_err("multi-family direct GitHub archive should require a boundary");

    assert!(matches!(error, FontbrewError::ArchiveRejected { .. }));
    let message = error.to_string();
    assert!(message.contains("multiple font families"));
    assert!(message.contains("Source Code Pro"));
    assert!(message.contains("Inter"));
    assert!(!paths.manifest_path().exists());
    assert!(staging_entries(&paths).is_empty());
}

#[test]
fn github_install_plan_cancellation_after_staging_creation_cleans_staging() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let fake_http = Arc::new(FakeHttpClient::default());
    let app = FontbrewApp::with_paths_and_http_client(paths.clone(), fake_http);

    let error = app
        .install_plan_with_cancellation(
            github_request("adobe", "source-code-pro", None),
            &CancelWhenInstallStagingExists {
                paths: paths.clone(),
            },
        )
        .expect_err("cancellation after staging creation should fail");

    assert!(matches!(error, FontbrewError::Cancelled));
    assert!(staging_entries(&paths).is_empty());
}

#[test]
fn github_install_plan_cleans_staging_when_download_is_cancelled() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let fake_http = Arc::new(FakeHttpClient::default());
    fake_http.with_text(
        &github_releases_url("adobe", "source-code-pro"),
        r#"[
  {
    "tag_name": "v1.2.3",
    "draft": false,
    "prerelease": false,
    "assets": [
      {
        "name": "source-code-pro.zip",
        "browser_download_url": "https://downloads.example/source-code-pro.zip"
      }
    ]
  }
]"#,
    );
    let cancel_flag = Arc::new(AtomicBool::new(false));
    fake_http.with_cancelling_download_bytes(
        "https://downloads.example/source-code-pro.zip",
        zip_with_fixture_font("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf"),
        1,
        cancel_flag.clone(),
    );
    let app = FontbrewApp::with_paths_and_http_client(paths.clone(), fake_http.clone());
    let cancellation = AtomicCancellation { flag: cancel_flag };

    let error = app
        .install_plan_with_cancellation(
            github_request("adobe", "source-code-pro", None),
            &cancellation,
        )
        .expect_err("cancelled download should fail planning");

    assert!(matches!(error, FontbrewError::Cancelled));
    assert_eq!(
        fake_http.requested_urls(),
        vec![
            github_releases_url("adobe", "source-code-pro"),
            "https://downloads.example/source-code-pro.zip".to_string(),
        ]
    );
    assert!(
        !paths.staging_dir().exists()
            || fs::read_dir(paths.staging_dir())
                .expect("read staging root")
                .next()
                .is_none()
    );
}

#[test]
fn github_token_is_sent_as_authorization_header_without_persisting_to_manifest() {
    let _guard = ENV_LOCK.lock().expect("env lock");
    let original = std::env::var_os("GITHUB_TOKEN");
    std::env::set_var("GITHUB_TOKEN", "test-token");

    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let fake_http = Arc::new(FakeHttpClient::default());
    fake_http.with_text(
        &github_releases_url("adobe", "source-code-pro"),
        r#"[
  {
    "tag_name": "v1.2.3",
    "draft": false,
    "prerelease": false,
    "assets": [
      {
        "name": "source-code-pro.zip",
        "browser_download_url": "https://downloads.example/source-code-pro.zip"
      }
    ]
  }
]"#,
    );
    fake_http.with_download_bytes(
        "https://downloads.example/source-code-pro.zip",
        zip_with_fixture_font("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf"),
    );
    let app = FontbrewApp::with_paths_and_http_client(paths.clone(), fake_http.clone());

    let plan = app
        .install_plan(github_request("adobe", "source-code-pro", None))
        .expect("plan GitHub install");
    apply_plan(&app, plan);

    let requests = fake_http.requested_requests();
    assert!(requests
        .first()
        .expect("GitHub API request")
        .headers
        .iter()
        .any(|header| header.name == "Authorization" && header.value == "Bearer test-token"));
    let manifest_text =
        std::fs::read_to_string(paths.manifest_path()).expect("manifest should exist");
    assert!(!manifest_text.contains("test-token"));

    match original {
        Some(value) => std::env::set_var("GITHUB_TOKEN", value),
        None => std::env::remove_var("GITHUB_TOKEN"),
    }
}

#[test]
fn direct_github_install_plan_is_noop_without_network_when_package_is_already_managed() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let first_http = Arc::new(FakeHttpClient::default());
    first_http.with_text(
        &github_releases_url("adobe", "source-code-pro"),
        r#"[
  {
    "tag_name": "v1.2.3",
    "draft": false,
    "prerelease": false,
    "assets": [
      {
        "name": "source-code-pro.zip",
        "browser_download_url": "https://downloads.example/source-code-pro.zip"
      }
    ]
  }
]"#,
    );
    first_http.with_download_bytes(
        "https://downloads.example/source-code-pro.zip",
        zip_with_fixture_font("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf"),
    );
    let app = FontbrewApp::with_paths_and_http_client(paths.clone(), first_http);
    let first_plan = app
        .install_plan(github_request("adobe", "source-code-pro", None))
        .expect("plan first install");
    apply_plan(&app, first_plan);

    let no_route_http = Arc::new(FakeHttpClient::default());
    let app = FontbrewApp::with_paths_and_http_client(paths, no_route_http.clone());
    let plan = app
        .install_plan(github_request("adobe", "source-code-pro", None))
        .expect("already managed direct GitHub install should plan without network");

    assert!(plan.already_installed);
    assert!(plan.changes.is_empty());
    assert!(no_route_http.requested_urls().is_empty());
}

#[test]
fn registry_recipe_install_plan_is_noop_without_github_when_package_is_already_managed() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let registry_json = r#"{
  "schemaVersion": 1,
  "updatedAt": "2026-07-03T00:00:00Z",
  "packages": {
    "source-code-pro": {
      "name": "Source Code Pro",
      "source": {
        "type": "github",
        "repo": "adobe/source-code-pro"
      },
      "families": ["Source Code Pro"],
      "asset": {
        "include": ["*.zip"],
        "exclude": []
      }
    }
  }
}"#;

    let first_http = Arc::new(FakeHttpClient::default());
    first_http.with_text(OFFICIAL_REGISTRY_URL, registry_json);
    first_http.with_text(
        &github_releases_url("adobe", "source-code-pro"),
        r#"[
  {
    "tag_name": "v1.2.3",
    "draft": false,
    "prerelease": false,
    "assets": [
      {
        "name": "source-code-pro.zip",
        "browser_download_url": "https://downloads.example/source-code-pro.zip"
      }
    ]
  }
]"#,
    );
    first_http.with_download_bytes(
        "https://downloads.example/source-code-pro.zip",
        zip_with_fixture_font("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf"),
    );
    let app = FontbrewApp::with_paths_and_http_client(paths.clone(), first_http);
    let first_plan = app
        .install_plan(registry_request("source-code-pro"))
        .expect("plan first registry install");
    apply_plan(&app, first_plan);

    let no_route_http = Arc::new(FakeHttpClient::default());
    no_route_http.with_text(OFFICIAL_REGISTRY_URL, registry_json);
    let app = FontbrewApp::with_paths_and_http_client(paths, no_route_http.clone());
    let plan = app
        .install_plan(registry_request("source-code-pro"))
        .expect("already managed registry install should plan without GitHub");

    assert!(plan.already_installed);
    assert!(plan.changes.is_empty());
    assert_eq!(
        no_route_http.requested_urls(),
        vec![OFFICIAL_REGISTRY_URL.to_string()]
    );
}

#[test]
fn oversized_github_asset_download_is_rejected_without_manifest_or_package_store() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let fake_http = Arc::new(FakeHttpClient::default());
    fake_http.with_text(
        &github_releases_url("adobe", "source-code-pro"),
        r#"[
  {
    "tag_name": "v1.2.3",
    "draft": false,
    "prerelease": false,
    "assets": [
      {
        "name": "source-code-pro.zip",
        "browser_download_url": "https://downloads.example/source-code-pro.zip"
      }
    ]
  }
]"#,
    );
    fake_http.with_download_content_length(
        "https://downloads.example/source-code-pro.zip",
        600 * 1024 * 1024,
    );
    let app = FontbrewApp::with_paths_and_http_client(paths.clone(), fake_http);

    let error = app
        .install_plan(github_request("adobe", "source-code-pro", None))
        .expect_err("oversized download should be rejected");

    assert!(matches!(
        error,
        FontbrewError::ArchiveRejected { reason } if reason.contains("download exceeds")
    ));
    assert!(!paths.manifest_path().exists());
    assert!(!paths.managed_store_dir().join("packages").exists());
}

#[test]
fn github_install_fails_when_multiple_installable_assets_match() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let fake_http = Arc::new(FakeHttpClient::default());
    fake_http.with_text(
        &github_releases_url("adobe", "source-code-pro"),
        r#"[
  {
    "tag_name": "v1.2.3",
    "draft": false,
    "prerelease": false,
    "assets": [
      {
        "name": "source-code-pro-desktop.zip",
        "browser_download_url": "https://downloads.example/source-code-pro-desktop.zip"
      },
      {
        "name": "source-code-pro-nerd-font.zip",
        "browser_download_url": "https://downloads.example/source-code-pro-nerd-font.zip"
      }
    ]
  }
]"#,
    );
    let app = FontbrewApp::with_paths_and_http_client(paths, fake_http);

    let error = app
        .install_plan(github_request("adobe", "source-code-pro", None))
        .expect_err("ambiguous GitHub assets should fail");

    match error {
        FontbrewError::AmbiguousAssets {
            package_id: ambiguous_package_id,
            assets,
        } => {
            assert_eq!(ambiguous_package_id, package_id("source-code-pro"));
            assert_eq!(
                assets,
                vec![
                    "source-code-pro-desktop.zip".to_string(),
                    "source-code-pro-nerd-font.zip".to_string(),
                ]
            );
        }
        other => panic!("expected AmbiguousAssets, got {other:?}"),
    }
}

#[test]
fn github_asset_selector_resolves_asset_ambiguity() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let fake_http = Arc::new(FakeHttpClient::default());
    fake_http.with_text(
        &github_releases_url("adobe", "source-code-pro"),
        r#"[
  {
    "tag_name": "v1.2.3",
    "draft": false,
    "prerelease": false,
    "assets": [
      {
        "name": "source-code-pro-desktop.zip",
        "browser_download_url": "https://downloads.example/source-code-pro-desktop.zip"
      },
      {
        "name": "source-code-pro-nerd-font.zip",
        "browser_download_url": "https://downloads.example/source-code-pro-nerd-font.zip"
      }
    ]
  }
]"#,
    );
    fake_http.with_download_bytes(
        "https://downloads.example/source-code-pro-desktop.zip",
        zip_with_fixture_font("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf"),
    );
    let app = FontbrewApp::with_paths_and_http_client(paths, fake_http.clone());

    let plan = app
        .install_plan(github_request(
            "adobe",
            "source-code-pro",
            Some("*desktop.zip"),
        ))
        .expect("selector should resolve one asset");
    let report = apply_plan(&app, plan);

    assert_eq!(report.package_id, package_id("source-code-pro"));
    assert_eq!(
        fake_http.requested_urls(),
        vec![
            github_releases_url("adobe", "source-code-pro"),
            "https://downloads.example/source-code-pro-desktop.zip".to_string(),
        ]
    );
}

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn registry_recipe_include_exclude_selects_github_asset_and_records_registry_source() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let registry_json = r#"{
  "schemaVersion": 1,
  "updatedAt": "2026-07-03T00:00:00Z",
  "packages": {
    "source-code-pro": {
      "name": "Source Code Pro",
      "source": {
        "type": "github",
        "repo": "adobe/source-code-pro"
      },
      "families": ["Source Code Pro"],
      "asset": {
        "include": ["*desktop*.zip"],
        "exclude": ["*web*"]
      }
    }
  }
}"#;

    let fake_http = Arc::new(FakeHttpClient::default());
    fake_http.with_text(OFFICIAL_REGISTRY_URL, registry_json);
    fake_http.with_text(
        &github_releases_url("adobe", "source-code-pro"),
        r#"[
  {
    "tag_name": "v1.2.3",
    "draft": false,
    "prerelease": false,
    "assets": [
      {
        "name": "source-code-pro-web.zip",
        "browser_download_url": "https://downloads.example/source-code-pro-web.zip"
      },
      {
        "name": "source-code-pro-desktop.zip",
        "browser_download_url": "https://downloads.example/source-code-pro-desktop.zip"
      }
    ]
  }
]"#,
    );
    fake_http.with_download_bytes(
        "https://downloads.example/source-code-pro-desktop.zip",
        zip_with_fixture_font("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf"),
    );
    let app = FontbrewApp::with_paths_and_http_client(paths.clone(), fake_http);

    let plan = app
        .install_plan(registry_request("source-code-pro"))
        .expect("registry recipe should resolve GitHub asset");
    assert_eq!(plan.package_id, package_id("source-code-pro"));
    let report = apply_plan(&app, plan);
    assert_eq!(report.package_id, package_id("source-code-pro"));

    let manifest = ManifestStore::read_or_empty(&paths.manifest_path()).expect("read manifest");
    let record = manifest
        .get_package(&package_id("source-code-pro"))
        .expect("manifest record");
    assert_eq!(
        record.source,
        ManifestSource::Registry {
            id: "source-code-pro".to_string(),
        }
    );
    assert_eq!(
        record.update_source,
        Some(ManifestSource::GitHub {
            owner: "adobe".to_string(),
            repo: "source-code-pro".to_string(),
        })
    );
}

#[test]
fn registry_recipe_format_preference_does_not_override_user_config() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let registry_json = r#"{
  "schemaVersion": 1,
  "updatedAt": "2026-07-03T00:00:00Z",
  "packages": {
    "source-code-pro": {
      "name": "Source Code Pro",
      "source": {
        "type": "github",
        "repo": "adobe/source-code-pro"
      },
      "families": ["Source Code Pro"],
      "asset": {
        "include": ["*.zip"],
        "exclude": []
      },
      "install": {
        "formatPreference": ["otf", "ttf"]
      }
    }
  }
}"#;
    let config_path = paths.config_path();
    fs::create_dir_all(config_path.parent().expect("config parent")).expect("create config dir");
    fs::write(
        &config_path,
        r#"
schema_version = 1

[install]
format_preference = ["ttf", "otf"]
"#,
    )
    .expect("write config");

    let fake_http = Arc::new(FakeHttpClient::default());
    fake_http.with_text(OFFICIAL_REGISTRY_URL, registry_json);
    fake_http.with_text(
        &github_releases_url("adobe", "source-code-pro"),
        r#"[
  {
    "tag_name": "v1.2.3",
    "draft": false,
    "prerelease": false,
    "assets": [
      {
        "name": "source-code-pro.zip",
        "browser_download_url": "https://downloads.example/source-code-pro.zip"
      }
    ]
  }
]"#,
    );
    fake_http.with_download_bytes(
        "https://downloads.example/source-code-pro.zip",
        zip_with_fixture_fonts(&[
            ("SourceCodePro-Regular.otf", "SourceCodePro-Regular.otf"),
            ("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf"),
        ]),
    );
    let app = FontbrewApp::with_paths_and_http_client(paths.clone(), fake_http);

    let plan = app
        .install_plan(registry_request("source-code-pro"))
        .expect("registry recipe should plan install");
    apply_plan(&app, plan);

    let files_dir = paths
        .package_store_dir(
            &package_id("source-code-pro"),
            &fontbrew_core::PackageVersion::new("v1.2.3"),
        )
        .join("files");
    assert!(files_dir.join("SourceCodePro-Regular.ttf").exists());
    assert!(!files_dir.join("SourceCodePro-Regular.otf").exists());
}

#[test]
fn registry_recipe_include_families_filter_extra_archive_families() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let registry_json = r#"{
  "schemaVersion": 1,
  "updatedAt": "2026-07-03T00:00:00Z",
  "packages": {
    "source-code-pro": {
      "name": "Source Code Pro",
      "source": {
        "type": "github",
        "repo": "adobe/source-code-pro"
      },
      "families": ["Source Code Pro"],
      "asset": {
        "include": ["*.zip"],
        "exclude": []
      },
      "install": {
        "includeFamilies": [" source   code pro "]
      }
    }
  }
}"#;

    let fake_http = Arc::new(FakeHttpClient::default());
    fake_http.with_text(OFFICIAL_REGISTRY_URL, registry_json);
    fake_http.with_text(
        &github_releases_url("adobe", "source-code-pro"),
        r#"[
  {
    "tag_name": "v1.2.3",
    "draft": false,
    "prerelease": false,
    "assets": [
      {
        "name": "source-code-pro.zip",
        "browser_download_url": "https://downloads.example/source-code-pro.zip"
      }
    ]
  }
]"#,
    );
    fake_http.with_download_bytes(
        "https://downloads.example/source-code-pro.zip",
        zip_with_fixture_fonts(&[
            ("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf"),
            ("Inter-Variable.ttf", "Inter-Variable.ttf"),
        ]),
    );
    let app = FontbrewApp::with_paths_and_http_client(paths.clone(), fake_http);

    let plan = app
        .install_plan(registry_request("source-code-pro"))
        .expect("registry recipe includeFamilies should explain the archive boundary");
    let report = apply_plan(&app, plan);

    assert_eq!(
        report.families,
        vec![fontbrew_core::FamilyName::new("Source Code Pro")]
    );
    let manifest = ManifestStore::read_or_empty(&paths.manifest_path()).expect("read manifest");
    let record = manifest
        .get_package(&package_id("source-code-pro"))
        .expect("manifest record");
    assert_eq!(
        record.families,
        vec![fontbrew_core::FamilyName::new("Source Code Pro")]
    );
    assert!(record
        .font_files
        .iter()
        .all(|font_file| font_file.family.as_str() == "Source Code Pro"));

    let files_dir = paths
        .package_store_dir(
            &package_id("source-code-pro"),
            &fontbrew_core::PackageVersion::new("v1.2.3"),
        )
        .join("files");
    assert!(files_dir.join("SourceCodePro-Regular.ttf").exists());
    assert!(!files_dir.join("Inter-Variable.ttf").exists());
}

#[test]
fn registry_recipe_include_families_cannot_weaken_expected_identity() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let registry_json = r#"{
  "schemaVersion": 1,
  "updatedAt": "2026-07-03T00:00:00Z",
  "packages": {
    "source-and-inter": {
      "name": "Source Code Pro + Inter",
      "source": {
        "type": "github",
        "repo": "adobe/source-code-pro"
      },
      "families": ["Source Code Pro", "Inter"],
      "asset": {
        "include": ["*.zip"],
        "exclude": []
      },
      "install": {
        "includeFamilies": ["Source Code Pro"]
      }
    }
  }
}"#;

    let fake_http = Arc::new(FakeHttpClient::default());
    fake_http.with_text(OFFICIAL_REGISTRY_URL, registry_json);
    fake_http.with_text(
        &github_releases_url("adobe", "source-code-pro"),
        r#"[
  {
    "tag_name": "v1.2.3",
    "draft": false,
    "prerelease": false,
    "assets": [
      {
        "name": "source-and-inter.zip",
        "browser_download_url": "https://downloads.example/source-and-inter.zip"
      }
    ]
  }
]"#,
    );
    fake_http.with_download_bytes(
        "https://downloads.example/source-and-inter.zip",
        zip_with_fixture_fonts(&[
            ("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf"),
            ("Inter-Variable.ttf", "Inter-Variable.ttf"),
        ]),
    );
    let app = FontbrewApp::with_paths_and_http_client(paths.clone(), fake_http);

    let error = app
        .install_plan(registry_request("source-and-inter"))
        .expect_err("includeFamilies cannot remove top-level identity families");

    assert!(matches!(error, FontbrewError::ArchiveRejected { .. }));
    let message = error.to_string();
    assert!(message.contains("selected font files"));
    assert!(message.contains("Inter"));
    assert!(!paths.manifest_path().exists());
    assert!(staging_entries(&paths).is_empty());
}

#[test]
fn registry_recipe_requires_all_expected_families() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let registry_json = r#"{
  "schemaVersion": 1,
  "updatedAt": "2026-07-03T00:00:00Z",
  "packages": {
    "source-and-inter": {
      "name": "Source Code Pro + Inter",
      "source": {
        "type": "github",
        "repo": "adobe/source-code-pro"
      },
      "families": ["Source Code Pro", "Inter"],
      "asset": {
        "include": ["*.zip"],
        "exclude": []
      }
    }
  }
}"#;

    let fake_http = Arc::new(FakeHttpClient::default());
    fake_http.with_text(OFFICIAL_REGISTRY_URL, registry_json);
    fake_http.with_text(
        &github_releases_url("adobe", "source-code-pro"),
        r#"[
  {
    "tag_name": "v1.2.3",
    "draft": false,
    "prerelease": false,
    "assets": [
      {
        "name": "source-and-inter.zip",
        "browser_download_url": "https://downloads.example/source-and-inter.zip"
      }
    ]
  }
]"#,
    );
    fake_http.with_download_bytes(
        "https://downloads.example/source-and-inter.zip",
        zip_with_fixture_font("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf"),
    );
    let app = FontbrewApp::with_paths_and_http_client(paths.clone(), fake_http);

    let error = app
        .install_plan(registry_request("source-and-inter"))
        .expect_err("registry recipe should require every expected family");

    assert!(matches!(error, FontbrewError::ArchiveRejected { .. }));
    let message = error.to_string();
    assert!(message.contains("missing expected registry recipe font families"));
    assert!(message.contains("Inter"));
    assert!(!paths.manifest_path().exists());
    assert!(staging_entries(&paths).is_empty());
}
