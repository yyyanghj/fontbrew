use std::{
    fs::{self, File, FileTimes},
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime},
};

use fontbrew_core::{
    manifest::{
        ManifestFontFileFormat, ManifestFontFileRecord, ManifestPackageRecord, ManifestSource,
        ManifestStore, ManifestV1,
    },
    platform::FontbrewPaths,
    CancellationToken, ExecutionPolicy, FamilyName, FontbrewApp, FontbrewError, InfoRequest,
    InstallRequest, InstallSource, OutdatedRequest, PackageId, PackageVersion, ProgressEvent,
    ProgressSink, ProviderKind, SearchRequest, UpdateRequest,
};

mod support;

use support::LocalHttpServer;

static FONTSOURCE_HTTP_FIXTURE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct NoProgress;

impl ProgressSink for NoProgress {
    fn emit(&mut self, _event: ProgressEvent) {}
}

#[derive(Default)]
struct RecordingProgress {
    events: Vec<ProgressEvent>,
}

impl ProgressSink for RecordingProgress {
    fn emit(&mut self, event: ProgressEvent) {
        self.events.push(event);
    }
}

struct NeverCancelled;

impl CancellationToken for NeverCancelled {
    fn is_cancelled(&self) -> bool {
        false
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

fn fixture_font_bytes(filename: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/fonts")
        .join(filename);
    let mut file = File::open(path).expect("open fixture font");
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).expect("read fixture font");
    bytes
}

fn fontsource_list_path() -> String {
    "/fonts".to_string()
}

fn fontsource_detail_path(id: &str) -> String {
    format!("/fonts/{id}")
}

fn font_download_path(name: &str) -> String {
    format!("/cdn/{name}")
}

fn app_with_server(paths: FontbrewPaths, server: &LocalHttpServer) -> FontbrewApp {
    FontbrewApp::with_paths_and_network_client(paths, Arc::new(server.network_client()))
}

fn downloaded_font_files(paths: &FontbrewPaths) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_downloaded_font_files(&paths.staging_dir(), &mut files);
    files.sort();
    files
}

fn collect_downloaded_font_files(path: &Path, files: &mut Vec<PathBuf>) {
    if !path.exists() {
        return;
    }
    for entry in fs::read_dir(path).expect("read staging path") {
        let entry = entry.expect("read staging entry");
        let path = entry.path();
        if path.is_dir() {
            collect_downloaded_font_files(&path, files);
        } else if path.extension().and_then(|extension| extension.to_str()) == Some("ttf") {
            files.push(path);
        }
    }
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

fn fontsource_list_snapshot_path(paths: &FontbrewPaths) -> PathBuf {
    paths
        .provider_metadata_dir()
        .join("fontsource-list-all.json")
}

fn fontsource_detail_snapshot_path(paths: &FontbrewPaths, provider_id: &str) -> PathBuf {
    paths
        .provider_metadata_dir()
        .join(format!("fontsource-detail-{provider_id}.json"))
}

fn set_file_modified_time(path: &Path, modified_at: SystemTime) {
    let file = File::options()
        .write(true)
        .open(path)
        .expect("open cached metadata file");
    file.set_times(FileTimes::new().set_modified(modified_at))
        .expect("set cached metadata modified time");
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

#[tokio::test]
async fn fontsource_search_returns_only_results_with_desktop_urls_and_writes_metadata_snapshots() {
    let _guard = FONTSOURCE_HTTP_FIXTURE_LOCK.lock().await;
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let server = LocalHttpServer::start();
    server.respond_text(
        &fontsource_list_path(),
        r#"[
  {
    "id": "abel",
    "family": "Abel",
    "subsets": ["latin"],
    "weights": [400],
    "styles": ["normal"],
    "lastModified": "2025-05-30",
    "license": "OFL-1.1",
    "type": "fontsource"
  },
  {
    "id": "abel-web-only",
    "family": "Abel Web Only",
    "subsets": ["latin"],
    "weights": [400],
    "styles": ["normal"],
    "lastModified": "2025-05-30",
    "license": "OFL-1.1",
    "type": "fontsource"
  }
]"#,
    );
    server.respond_text(
        &fontsource_detail_path("abel"),
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
    server.respond_text(
        &fontsource_detail_path("abel-web-only"),
        r#"{
  "id": "abel-web-only",
  "family": "Abel Web Only",
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
    let app = app_with_server(paths.clone(), &server);

    let report = app
        .search(SearchRequest {
            query: "fontsource:Abl".to_string(),
            limit: None,
        })
        .await
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
        server.request_urls(),
        vec![
            server.url(&fontsource_list_path()),
            server.url(&fontsource_detail_path("abel")),
            server.url(&fontsource_detail_path("abel-web-only")),
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

#[tokio::test]
async fn fontsource_search_uses_fresh_metadata_snapshot_without_network() {
    let _guard = FONTSOURCE_HTTP_FIXTURE_LOCK.lock().await;
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let server = LocalHttpServer::start();
    server.respond_text(
        &fontsource_list_path(),
        r#"[{
  "id": "abel",
  "family": "Abel",
  "subsets": ["latin"],
  "weights": [400],
  "styles": ["normal"],
  "lastModified": "2025-05-30",
  "license": "OFL-1.1",
  "type": "fontsource"
}]"#,
    );
    server.respond_text(
        &fontsource_detail_path("abel"),
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
    let app = app_with_server(paths.clone(), &server);
    let first_report = app
        .search(SearchRequest {
            query: "fontsource:Abel".to_string(),
            limit: None,
        })
        .await
        .expect("first search should write Fontsource metadata snapshot");

    assert_eq!(first_report.results.len(), 1);
    assert_eq!(
        first_report.results[0]
            .version
            .as_ref()
            .expect("version")
            .as_str(),
        "v18"
    );

    let second_report = app
        .search(SearchRequest {
            query: "fontsource:Abel".to_string(),
            limit: None,
        })
        .await
        .expect("second search should use fresh Fontsource metadata snapshot");

    assert_eq!(second_report.results.len(), 1);
    assert_eq!(second_report.results[0].source, "fontsource:abel");
    assert_eq!(
        second_report.results[0]
            .version
            .as_ref()
            .expect("version")
            .as_str(),
        "v18"
    );
    assert_eq!(
        server.request_urls(),
        vec![
            server.url(&fontsource_list_path()),
            server.url(&fontsource_detail_path("abel"))
        ]
    );
    assert_provider_metadata_has_no_font_binaries(&paths);
}

#[tokio::test]
async fn fontsource_search_falls_back_to_stale_metadata_snapshot_when_refresh_fails() {
    let _guard = FONTSOURCE_HTTP_FIXTURE_LOCK.lock().await;
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let server = LocalHttpServer::start();
    server.respond_text(
        &fontsource_list_path(),
        r#"[{
  "id": "abel",
  "family": "Abel",
  "subsets": ["latin"],
  "weights": [400],
  "styles": ["normal"],
  "lastModified": "2025-05-30",
  "license": "OFL-1.1",
  "type": "fontsource"
}]"#,
    );
    server.respond_text(
        &fontsource_detail_path("abel"),
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
    let app = app_with_server(paths.clone(), &server);

    app.search(SearchRequest {
        query: "fontsource:Abel".to_string(),
        limit: None,
    })
    .await
    .expect("first search should write Fontsource metadata snapshot");

    let stale_time = SystemTime::now() - Duration::from_secs(48 * 60 * 60);
    set_file_modified_time(&fontsource_list_snapshot_path(&paths), stale_time);
    set_file_modified_time(&fontsource_detail_snapshot_path(&paths, "abel"), stale_time);
    server.respond_status(&fontsource_list_path(), 500, "server error");
    server.respond_status(&fontsource_detail_path("abel"), 500, "server error");

    let stale_report = app
        .search(SearchRequest {
            query: "fontsource:Abel".to_string(),
            limit: None,
        })
        .await
        .expect("stale Fontsource metadata should be used when refresh fails");

    assert_eq!(stale_report.results.len(), 1);
    assert_eq!(stale_report.results[0].source, "fontsource:abel");
    assert_eq!(
        stale_report.results[0]
            .version
            .as_ref()
            .expect("version")
            .as_str(),
        "v18"
    );
    assert_eq!(
        server.request_urls(),
        vec![
            server.url(&fontsource_list_path()),
            server.url(&fontsource_detail_path("abel")),
            server.url(&fontsource_list_path()),
            server.url(&fontsource_detail_path("abel")),
        ]
    );
    assert_provider_metadata_has_no_font_binaries(&paths);
}

#[tokio::test]
async fn fontsource_install_downloads_desktop_font_and_records_provider_manifest_source() {
    let _guard = FONTSOURCE_HTTP_FIXTURE_LOCK.lock().await;
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let server = LocalHttpServer::start();
    let source_code_pro_download = server.url(&font_download_path("source-code-pro.ttf"));
    server.respond_text(
        &fontsource_detail_path("source-code-pro"),
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
            "ttf": "__DOWNLOAD_URL__"
          }
        }
      }
    }
  }
}"#
        .replace("__DOWNLOAD_URL__", &source_code_pro_download),
    );
    server.respond_bytes(
        &font_download_path("source-code-pro.ttf"),
        fixture_font_bytes("SourceCodePro-Regular.ttf"),
    );
    let app = app_with_server(paths.clone(), &server);

    let plan = app
        .install_plan(InstallRequest {
            source: InstallSource::Provider {
                provider: ProviderKind::Fontsource,
                id: "source-code-pro".to_string(),
            },
            package_id_override: None,
            format_preference: Vec::new(),
            asset_selector: None,
            selected_families: Vec::new(),
            reinstall: false,
        })
        .await
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
        server.request_urls(),
        vec![
            server.url(&fontsource_detail_path("source-code-pro")),
            source_code_pro_download,
        ]
    );
    let download_targets = downloaded_font_files(&paths);
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
            Arc::new(NeverCancelled),
        )
        .await
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
        .await
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

#[tokio::test]
async fn fontsource_install_records_provider_variant_weight_for_downloaded_font() {
    let _guard = FONTSOURCE_HTTP_FIXTURE_LOCK.lock().await;
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let server = LocalHttpServer::start();
    let source_code_pro_download = server.url(&font_download_path("source-code-pro-200.ttf"));
    server.respond_text(
        &fontsource_detail_path("source-code-pro"),
        r#"{
  "id": "source-code-pro",
  "family": "Source Code Pro",
  "subsets": ["latin"],
  "weights": [200],
  "styles": ["normal"],
  "lastModified": "2025-05-30",
  "version": "v2",
  "license": "OFL-1.1",
  "variants": {
    "200": {
      "normal": {
        "latin": {
          "url": {
            "ttf": "__DOWNLOAD_URL__"
          }
        }
      }
    }
  }
}"#
        .replace("__DOWNLOAD_URL__", &source_code_pro_download),
    );
    server.respond_bytes(
        &font_download_path("source-code-pro-200.ttf"),
        fixture_font_bytes("SourceCodePro-Regular.ttf"),
    );
    let app = app_with_server(paths.clone(), &server);

    let plan = app
        .install_plan(InstallRequest {
            source: InstallSource::Provider {
                provider: ProviderKind::Fontsource,
                id: "source-code-pro".to_string(),
            },
            package_id_override: None,
            format_preference: Vec::new(),
            asset_selector: None,
            selected_families: Vec::new(),
            reinstall: false,
        })
        .await
        .expect("plan Fontsource install");

    app.apply_install(
        plan,
        ExecutionPolicy::SafeOnly,
        &mut NoProgress,
        Arc::new(NeverCancelled),
    )
    .await
    .expect("apply Fontsource install");

    let info = app
        .package_info(InfoRequest {
            package_id: package_id("source-code-pro"),
        })
        .await
        .expect("read Fontsource package info");

    assert_eq!(info.package.font_files.len(), 1);
    assert_eq!(info.package.font_files[0].weight, 200);
}

#[tokio::test]
async fn fontsource_info_recovers_provider_variant_weight_from_legacy_manifest_path() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let package_id = package_id("source-code-pro");
    let version = PackageVersion::parse("v2").expect("version");
    let font_path = paths
        .package_store_dir(&package_id, &version)
        .join("files/source-code-pro-latin-200-normal.ttf");
    let mut manifest = ManifestV1::empty();
    manifest
        .insert_package(ManifestPackageRecord {
            package_id: package_id.clone(),
            version: version.clone(),
            source: ManifestSource::Provider {
                provider: ProviderKind::Fontsource,
                id: "source-code-pro".to_string(),
            },
            update_source: None,
            families: vec![FamilyName::new("Source Code Pro")],
            font_files: vec![ManifestFontFileRecord {
                path: font_path,
                family: FamilyName::new("Source Code Pro"),
                style: "Regular".to_string(),
                weight: 400,
                format: ManifestFontFileFormat::Ttf,
            }],
            activation_artifacts: Vec::new(),
            installed_at: "2026-07-05T00:00:00Z".to_string(),
            active_version: Some(version),
        })
        .expect("insert manifest record");
    ManifestStore::write(&paths.manifest_path(), &manifest).expect("write manifest");
    let app = FontbrewApp::with_paths(paths);

    let info = app
        .package_info(InfoRequest { package_id })
        .await
        .expect("read Fontsource package info");

    assert_eq!(info.package.font_files.len(), 1);
    assert_eq!(info.package.font_files[0].weight, 200);
}

#[tokio::test]
async fn fontsource_update_uses_provider_metadata_and_replaces_managed_version() {
    let _guard = FONTSOURCE_HTTP_FIXTURE_LOCK.lock().await;
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let install_server = LocalHttpServer::start();
    let source_code_pro_v1_download =
        install_server.url(&font_download_path("source-code-pro-v1.ttf"));
    install_server.respond_text(
        &fontsource_detail_path("source-code-pro"),
        r#"{
  "id": "source-code-pro",
  "family": "Source Code Pro",
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
            "ttf": "__DOWNLOAD_URL__"
          }
        }
      }
    }
  }
}"#
        .replace("__DOWNLOAD_URL__", &source_code_pro_v1_download),
    );
    install_server.respond_bytes(
        &font_download_path("source-code-pro-v1.ttf"),
        fixture_font_bytes("SourceCodePro-Regular.ttf"),
    );
    let app = app_with_server(paths.clone(), &install_server);
    let plan = app
        .install_plan(InstallRequest {
            source: InstallSource::Provider {
                provider: ProviderKind::Fontsource,
                id: "source-code-pro".to_string(),
            },
            package_id_override: None,
            format_preference: Vec::new(),
            asset_selector: None,
            selected_families: Vec::new(),
            reinstall: false,
        })
        .await
        .expect("plan initial Fontsource install");
    app.apply_install(
        plan,
        ExecutionPolicy::SafeOnly,
        &mut NoProgress,
        Arc::new(NeverCancelled),
    )
    .await
    .expect("apply initial Fontsource install");

    let stale_time = SystemTime::now() - Duration::from_secs(48 * 60 * 60);
    set_file_modified_time(
        &fontsource_detail_snapshot_path(&paths, "source-code-pro"),
        stale_time,
    );

    let update_server = LocalHttpServer::start();
    let source_code_pro_v2_download =
        update_server.url(&font_download_path("source-code-pro-v2.ttf"));
    update_server.respond_text(
        &fontsource_detail_path("source-code-pro"),
        r#"{
  "id": "source-code-pro",
  "family": "Source Code Pro",
  "subsets": ["latin"],
  "weights": [400],
  "styles": ["normal"],
  "lastModified": "2025-06-30",
  "version": "v2",
  "license": "OFL-1.1",
  "variants": {
    "400": {
      "normal": {
        "latin": {
          "url": {
            "ttf": "__DOWNLOAD_URL__"
          }
        }
      }
    }
  }
}"#
        .replace("__DOWNLOAD_URL__", &source_code_pro_v2_download),
    );
    update_server.respond_bytes(
        &font_download_path("source-code-pro-v2.ttf"),
        fixture_font_bytes("SourceCodePro-Regular.ttf"),
    );
    let app = app_with_server(paths.clone(), &update_server);

    let outdated = app
        .outdated(OutdatedRequest {
            package_ids: vec![package_id("source-code-pro")],
        })
        .await
        .expect("check Fontsource outdated");
    assert_eq!(outdated.packages.len(), 1);
    assert_eq!(outdated.packages[0].latest_version.as_str(), "v2");
    assert!(outdated.not_updatable.is_empty());

    let plan = app
        .update_plan(
            UpdateRequest {
                package_ids: vec![package_id("source-code-pro")],
                jobs: Some(1),
            },
            &mut NoProgress,
            Arc::new(NeverCancelled),
        )
        .await
        .expect("plan Fontsource update");
    assert_eq!(plan.prepared.len(), 1);
    assert_eq!(plan.prepared[0].target_version.as_str(), "v2");

    let report = app
        .apply_update(
            plan,
            ExecutionPolicy::SafeOnly,
            &mut NoProgress,
            Arc::new(NeverCancelled),
        )
        .await
        .expect("apply Fontsource update");

    assert_eq!(report.updated.len(), 1);
    assert_eq!(report.updated[0].installed_version.as_str(), "v2");
    let manifest = ManifestStore::read_or_empty(&paths.manifest_path()).expect("read manifest");
    let record = manifest
        .get_package(&package_id("source-code-pro"))
        .expect("manifest record");
    assert_eq!(record.version.as_str(), "v2");
    assert_eq!(
        record.source,
        ManifestSource::Provider {
            provider: ProviderKind::Fontsource,
            id: "source-code-pro".to_string(),
        }
    );
    assert_eq!(
        update_server.request_urls(),
        vec![
            update_server.url(&fontsource_detail_path("source-code-pro")),
            source_code_pro_v2_download,
        ]
    );
}

#[tokio::test]
async fn fontsource_outdated_does_not_use_stale_metadata_when_refresh_fails() {
    let _guard = FONTSOURCE_HTTP_FIXTURE_LOCK.lock().await;
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let install_server = LocalHttpServer::start();
    let source_code_pro_v1_download =
        install_server.url(&font_download_path("source-code-pro-v1.ttf"));
    install_server.respond_text(
        &fontsource_detail_path("source-code-pro"),
        r#"{
  "id": "source-code-pro",
  "family": "Source Code Pro",
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
            "ttf": "__DOWNLOAD_URL__"
          }
        }
      }
    }
  }
}"#
        .replace("__DOWNLOAD_URL__", &source_code_pro_v1_download),
    );
    install_server.respond_bytes(
        &font_download_path("source-code-pro-v1.ttf"),
        fixture_font_bytes("SourceCodePro-Regular.ttf"),
    );
    let app = app_with_server(paths.clone(), &install_server);
    let plan = app
        .install_plan(InstallRequest {
            source: InstallSource::Provider {
                provider: ProviderKind::Fontsource,
                id: "source-code-pro".to_string(),
            },
            package_id_override: None,
            format_preference: Vec::new(),
            asset_selector: None,
            selected_families: Vec::new(),
            reinstall: false,
        })
        .await
        .expect("plan initial Fontsource install");
    app.apply_install(
        plan,
        ExecutionPolicy::SafeOnly,
        &mut NoProgress,
        Arc::new(NeverCancelled),
    )
    .await
    .expect("apply initial Fontsource install");

    let stale_time = SystemTime::now() - Duration::from_secs(48 * 60 * 60);
    set_file_modified_time(
        &fontsource_detail_snapshot_path(&paths, "source-code-pro"),
        stale_time,
    );

    let update_server = LocalHttpServer::start();
    update_server.respond_status(
        &fontsource_detail_path("source-code-pro"),
        500,
        "server error",
    );
    let app = app_with_server(paths, &update_server);

    let error = app
        .outdated(OutdatedRequest {
            package_ids: vec![package_id("source-code-pro")],
        })
        .await
        .expect_err("stale Fontsource metadata should not hide refresh failure");

    assert!(error
        .to_string()
        .contains("HTTP request failed with status 500"));
}

#[tokio::test]
async fn fontsource_install_plan_reports_progress_before_apply() {
    let _guard = FONTSOURCE_HTTP_FIXTURE_LOCK.lock().await;
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let server = LocalHttpServer::start();
    let source_code_pro_download = server.url(&font_download_path("source-code-pro.ttf"));
    server.respond_text(
        &fontsource_detail_path("source-code-pro"),
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
            "ttf": "__DOWNLOAD_URL__"
          }
        }
      }
    }
  }
}"#
        .replace("__DOWNLOAD_URL__", &source_code_pro_download),
    );
    let font_bytes = fixture_font_bytes("SourceCodePro-Regular.ttf");
    server.respond_bytes(
        &font_download_path("source-code-pro.ttf"),
        font_bytes.clone(),
    );
    let app = app_with_server(paths, &server);
    let mut progress = RecordingProgress::default();

    app.install_plan_with_progress_and_cancellation(
        InstallRequest {
            source: InstallSource::Provider {
                provider: ProviderKind::Fontsource,
                id: "source-code-pro".to_string(),
            },
            package_id_override: None,
            format_preference: Vec::new(),
            asset_selector: None,
            selected_families: Vec::new(),
            reinstall: false,
        },
        &mut progress,
        Arc::new(NeverCancelled),
    )
    .await
    .expect("plan Fontsource install");

    assert!(progress.events.iter().any(|event| matches!(
        event,
        ProgressEvent::ResolvingSource { source } if source == "fontsource:source-code-pro"
    )));
    assert!(progress.events.iter().any(|event| matches!(
        event,
        ProgressEvent::DownloadStarted { package_id, bytes: None }
            if package_id.as_str() == "source-code-pro"
    )));
    assert!(progress.events.iter().any(|event| matches!(
        event,
        ProgressEvent::DownloadProgress {
            package_id,
            downloaded,
            total: None,
        } if package_id.as_str() == "source-code-pro" && *downloaded == font_bytes.len() as u64
    )));
    assert!(progress.events.iter().any(|event| matches!(
        event,
        ProgressEvent::ParsingFonts { package_id } if package_id.as_str() == "source-code-pro"
    )));
}

#[tokio::test]
async fn fontsource_install_plan_parse_error_replays_provider_progress_and_cleans_staging() {
    let _guard = FONTSOURCE_HTTP_FIXTURE_LOCK.lock().await;
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let server = LocalHttpServer::start();
    let source_code_pro_download = server.url(&font_download_path("source-code-pro.ttf"));
    server.respond_text(
        &fontsource_detail_path("source-code-pro"),
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
            "ttf": "__DOWNLOAD_URL__"
          }
        }
      }
    }
  }
}"#
        .replace("__DOWNLOAD_URL__", &source_code_pro_download),
    );
    let malformed_font = b"not a parseable font".to_vec();
    server.respond_bytes(
        &font_download_path("source-code-pro.ttf"),
        malformed_font.clone(),
    );
    let app = app_with_server(paths.clone(), &server);
    let mut progress = RecordingProgress::default();

    let error = app
        .install_plan_with_progress_and_cancellation(
            InstallRequest {
                source: InstallSource::Provider {
                    provider: ProviderKind::Fontsource,
                    id: "source-code-pro".to_string(),
                },
                package_id_override: None,
                format_preference: Vec::new(),
                asset_selector: None,
                selected_families: Vec::new(),
                reinstall: false,
            },
            &mut progress,
            Arc::new(NeverCancelled),
        )
        .await
        .expect_err("malformed provider font should fail planning");

    assert!(matches!(error, FontbrewError::FontParse { .. }));
    assert!(progress.events.iter().any(|event| matches!(
        event,
        ProgressEvent::DownloadStarted { package_id, bytes: None }
            if package_id.as_str() == "source-code-pro"
    )));
    assert!(progress.events.iter().any(|event| matches!(
        event,
        ProgressEvent::DownloadProgress {
            package_id,
            downloaded,
            total: None,
        } if package_id.as_str() == "source-code-pro" && *downloaded == malformed_font.len() as u64
    )));
    assert!(progress.events.iter().any(|event| matches!(
        event,
        ProgressEvent::ParsingFonts { package_id } if package_id.as_str() == "source-code-pro"
    )));
    assert!(
        !paths.staging_dir().exists()
            || fs::read_dir(paths.staging_dir())
                .expect("read staging dir")
                .next()
                .is_none()
    );
}
