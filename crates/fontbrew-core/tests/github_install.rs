use std::{
    fs::{self, File},
    io::{Cursor, Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use fontbrew_core::{
    manifest::{ManifestSource, ManifestStore},
    platform::FontbrewPaths,
    CancellationToken, ExecutionPolicy, FamilyName, Fontbrew, FontbrewError, FontbrewOptions,
    InstallPreparation, InstallRequest, InstallSource, PackageId, ProgressEvent, ProgressSink,
};
use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

mod support;

use support::LocalHttpServer;

static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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

fn fontbrew_with_paths(paths: FontbrewPaths) -> Fontbrew {
    Fontbrew::new(FontbrewOptions {
        store_dir: Some(paths.managed_store_dir()),
        config_path: Some(paths.config_path()),
        activation_dir: Some(paths.activation_dir()),
    })
    .expect("create Fontbrew")
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
    github_request_with_selected_families(owner, repo, asset_selector, Vec::new())
}

fn github_request_with_selected_families(
    owner: &str,
    repo: &str,
    asset_selector: Option<&str>,
    families: Vec<&str>,
) -> InstallRequest {
    InstallRequest {
        source: InstallSource::GitHubRepo {
            owner: owner.to_string(),
            repo: repo.to_string(),
        },
        package_id_override: None,
        format_preference: Vec::new(),
        asset_selector: asset_selector.map(str::to_string),
        selected_families: families.into_iter().map(FamilyName::new).collect(),
        reinstall: false,
    }
}

fn github_releases_path(owner: &str, repo: &str) -> String {
    format!("/repos/{owner}/{repo}/releases")
}

fn download_path(name: &str) -> String {
    format!("/downloads/{name}")
}

fn fontbrew_with_server(paths: FontbrewPaths, server: &LocalHttpServer) -> Fontbrew {
    fontbrew_with_paths(paths).with_network_client(Arc::new(server.network_client()))
}

fn github_release_response(version: &str, asset_name: &str, download_url: &str) -> String {
    format!(
        r#"[
  {{
    "tag_name": "{version}",
    "draft": false,
    "prerelease": false,
    "assets": [
      {{
        "name": "{asset_name}",
        "browser_download_url": "{download_url}"
      }}
    ]
  }}
]"#
    )
}

fn downloaded_staging_files(paths: &FontbrewPaths) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_downloaded_staging_files(&paths.staging_dir(), &mut files);
    files.sort();
    files
}

fn collect_downloaded_staging_files(path: &Path, files: &mut Vec<PathBuf>) {
    if !path.exists() {
        return;
    }
    for entry in fs::read_dir(path).expect("read staging path") {
        let entry = entry.expect("read staging entry");
        let path = entry.path();
        if path.is_dir() {
            collect_downloaded_staging_files(&path, files);
        } else if path.file_name().is_some_and(|name| name == "download.zip") {
            files.push(path);
        }
    }
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

async fn apply_plan(
    app: &Fontbrew,
    plan: fontbrew_core::InstallPlan,
) -> fontbrew_core::InstallReport {
    let mut progress = NoProgress;
    app.apply_install_plan(
        plan,
        ExecutionPolicy::SafeOnly,
        &mut progress,
        Arc::new(NeverCancelled),
    )
    .await
    .expect("apply install")
}

#[tokio::test]
async fn direct_github_install_selects_latest_stable_release_and_records_github_source() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let server = LocalHttpServer::start();
    let source_code_pro_download = server.url(&download_path("source-code-pro.zip"));
    server.respond_text(
        &github_releases_path("adobe", "source-code-pro"),
        format!(
            r#"[
  {{
    "tag_name": "v3.0.0",
    "draft": true,
    "prerelease": false,
    "assets": [
      {{"name": "draft.zip", "browser_download_url": "https://downloads.example/draft.zip"}}
    ]
  }},
  {{
    "tag_name": "v2.0.0-beta.1",
    "draft": false,
    "prerelease": true,
    "assets": [
      {{"name": "beta.zip", "browser_download_url": "https://downloads.example/beta.zip"}}
    ]
  }},
  {{
    "tag_name": "v1.2.3",
    "draft": false,
    "prerelease": false,
    "assets": [
      {{
        "name": "source-code-pro.zip",
        "browser_download_url": "{source_code_pro_download}"
      }}
    ]
  }}
]"#
        ),
    );
    server.respond_bytes(
        &download_path("source-code-pro.zip"),
        zip_with_fixture_font("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf"),
    );
    let app = fontbrew_with_server(paths.clone(), &server);

    let plan = app
        .install_plan(github_request("adobe", "source-code-pro", None))
        .await
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
        server.request_urls(),
        vec![
            server.url(&github_releases_path("adobe", "source-code-pro")),
            source_code_pro_download,
        ]
    );
    let download_targets = downloaded_staging_files(&paths);
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

    let report = apply_plan(&app, plan).await;
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

#[tokio::test]
async fn direct_github_install_uses_package_id_override() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let server = LocalHttpServer::start();
    let source_code_pro_download = server.url(&download_path("source-code-pro.zip"));
    server.respond_text(
        &github_releases_path("adobe", "source-code-pro"),
        github_release_response("v1.2.3", "source-code-pro.zip", &source_code_pro_download),
    );
    server.respond_bytes(
        &download_path("source-code-pro.zip"),
        zip_with_fixture_font("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf"),
    );
    let app = fontbrew_with_server(paths.clone(), &server);
    let mut request = github_request("adobe", "source-code-pro", None);
    request.package_id_override = Some(package_id("custom-remote"));

    let plan = app
        .install_plan(request)
        .await
        .expect("plan GitHub install with package id override");
    assert_eq!(plan.package_id, package_id("custom-remote"));

    let report = apply_plan(&app, plan).await;
    assert_eq!(report.package_id, package_id("custom-remote"));

    let manifest = ManifestStore::read_or_empty(&paths.manifest_path()).expect("read manifest");
    let record = manifest
        .get_package(&package_id("custom-remote"))
        .expect("manifest record");
    assert_eq!(
        record.source,
        ManifestSource::GitHub {
            owner: "adobe".to_string(),
            repo: "source-code-pro".to_string(),
        }
    );
    assert!(manifest
        .get_package(&package_id("source-code-pro"))
        .is_none());
}

#[tokio::test]
async fn direct_github_install_requires_family_selection_for_multiple_families() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let server = LocalHttpServer::start();
    let source_code_pro_download = server.url(&download_path("source-code-pro.zip"));
    server.respond_text(
        &github_releases_path("adobe", "source-code-pro"),
        github_release_response("v1.2.3", "source-code-pro.zip", &source_code_pro_download),
    );
    server.respond_bytes(
        &download_path("source-code-pro.zip"),
        zip_with_fixture_fonts(&[
            ("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf"),
            ("Inter-Variable.ttf", "Inter-Variable.ttf"),
        ]),
    );
    let app = fontbrew_with_server(paths.clone(), &server);

    let error = app
        .install_plan(github_request("adobe", "source-code-pro", None))
        .await
        .expect_err("multi-family direct GitHub archive should require a boundary");

    assert!(matches!(
        error,
        FontbrewError::FamilySelectionRequired { .. }
    ));
    let message = error.to_string();
    assert!(message.contains("multiple font families"));
    assert!(message.contains("Source Code Pro"));
    assert!(message.contains("Inter"));
    assert!(!paths.manifest_path().exists());
    assert!(staging_entries(&paths).is_empty());
}

#[tokio::test]
async fn github_install_plan_archive_parse_error_replays_progress_and_cleans_staging() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let server = LocalHttpServer::start();
    let source_code_pro_download = server.url(&download_path("source-code-pro.zip"));
    server.respond_text(
        &github_releases_path("adobe", "source-code-pro"),
        github_release_response("v1.2.3", "source-code-pro.zip", &source_code_pro_download),
    );
    server.respond_bytes(&download_path("source-code-pro.zip"), b"not a zip".to_vec());
    let app = fontbrew_with_server(paths.clone(), &server);
    let mut progress = RecordingProgress::default();

    let error = app
        .install_plan_with_progress_and_cancellation(
            github_request("adobe", "source-code-pro", None),
            &mut progress,
            Arc::new(NeverCancelled),
        )
        .await
        .expect_err("malformed GitHub archive should fail planning");

    assert!(matches!(error, FontbrewError::ArchiveRejected { .. }));
    assert!(progress.events.iter().any(|event| matches!(
        event,
        ProgressEvent::DownloadStarted { package_id, .. }
            if package_id.as_str() == "source-code-pro"
    )));
    assert!(progress.events.iter().any(|event| matches!(
        event,
        ProgressEvent::ExtractingArchive { package_id }
            if package_id.as_str() == "source-code-pro"
    )));
    assert!(staging_entries(&paths).is_empty());
}

#[tokio::test]
async fn direct_github_install_selected_family_installs_one_package() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let server = LocalHttpServer::start();
    let source_code_pro_download = server.url(&download_path("source-code-pro.zip"));
    server.respond_text(
        &github_releases_path("adobe", "source-code-pro"),
        github_release_response("v1.2.3", "source-code-pro.zip", &source_code_pro_download),
    );
    server.respond_bytes(
        &download_path("source-code-pro.zip"),
        zip_with_fixture_fonts(&[
            ("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf"),
            ("Inter-Variable.ttf", "Inter-Variable.ttf"),
        ]),
    );
    let app = fontbrew_with_server(paths.clone(), &server);

    let plan = app
        .install_plan(github_request_with_selected_families(
            "adobe",
            "source-code-pro",
            None,
            vec!["Inter"],
        ))
        .await
        .expect("selected family should plan");

    assert_eq!(plan.package_id, package_id("inter"));

    let report = apply_plan(&app, plan).await;

    assert_eq!(report.package_id, package_id("inter"));
    assert_eq!(report.families, vec![FamilyName::new("Inter")]);
    let manifest = ManifestStore::read_or_empty(&paths.manifest_path()).expect("read manifest");
    assert!(manifest.get_package(&package_id("inter")).is_some());
    assert!(manifest
        .get_package(&package_id("source-code-pro"))
        .is_none());
}

#[tokio::test]
async fn github_install_plan_cancellation_after_staging_creation_cleans_staging() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let server = LocalHttpServer::start();
    let app = fontbrew_with_server(paths.clone(), &server);

    let error = app
        .install_plan_with_cancellation(
            github_request("adobe", "source-code-pro", None),
            Arc::new(CancelWhenInstallStagingExists {
                paths: paths.clone(),
            }),
        )
        .await
        .expect_err("cancellation after staging creation should fail");

    assert!(matches!(error, FontbrewError::Cancelled));
    assert!(staging_entries(&paths).is_empty());
}

#[tokio::test]
async fn github_install_plan_cleans_staging_when_download_is_cancelled() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let server = LocalHttpServer::start();
    let source_code_pro_download = server.url(&download_path("source-code-pro.zip"));
    server.respond_text(
        &github_releases_path("adobe", "source-code-pro"),
        github_release_response("v1.2.3", "source-code-pro.zip", &source_code_pro_download),
    );
    let cancel_flag = Arc::new(AtomicBool::new(false));
    server.respond_bytes_with_cancellation(
        &download_path("source-code-pro.zip"),
        zip_with_fixture_font("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf"),
        7,
        1,
        cancel_flag.clone(),
    );
    let app = fontbrew_with_server(paths.clone(), &server);
    let cancellation: Arc<dyn CancellationToken> =
        Arc::new(AtomicCancellation { flag: cancel_flag });

    let error = app
        .install_plan_with_cancellation(
            github_request("adobe", "source-code-pro", None),
            cancellation.clone(),
        )
        .await
        .expect_err("cancelled download should fail planning");

    assert!(matches!(error, FontbrewError::Cancelled));
    assert_eq!(
        server.request_urls(),
        vec![
            server.url(&github_releases_path("adobe", "source-code-pro")),
            source_code_pro_download,
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

#[tokio::test]
async fn github_token_is_sent_as_authorization_header_without_persisting_to_manifest() {
    let _guard = ENV_LOCK.lock().await;
    let original = std::env::var_os("GITHUB_TOKEN");
    std::env::set_var("GITHUB_TOKEN", "test-token");

    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let server = LocalHttpServer::start();
    let source_code_pro_download = server.url(&download_path("source-code-pro.zip"));
    server.respond_text(
        &github_releases_path("adobe", "source-code-pro"),
        github_release_response("v1.2.3", "source-code-pro.zip", &source_code_pro_download),
    );
    server.respond_bytes(
        &download_path("source-code-pro.zip"),
        zip_with_fixture_font("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf"),
    );
    let app = fontbrew_with_server(paths.clone(), &server);

    let plan = app
        .install_plan(github_request("adobe", "source-code-pro", None))
        .await
        .expect("plan GitHub install");
    apply_plan(&app, plan).await;

    let requests = server.requests();
    assert!(requests
        .first()
        .expect("GitHub API request")
        .headers
        .iter()
        .any(|(name, value)| name.eq_ignore_ascii_case("authorization")
            && value == "Bearer test-token"));
    let manifest_text =
        std::fs::read_to_string(paths.manifest_path()).expect("manifest should exist");
    assert!(!manifest_text.contains("test-token"));

    match original {
        Some(value) => std::env::set_var("GITHUB_TOKEN", value),
        None => std::env::remove_var("GITHUB_TOKEN"),
    }
}

#[tokio::test]
async fn github_api_rate_limit_error_mentions_github_token() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let server = LocalHttpServer::start();
    server.respond_status(
        &github_releases_path("githubnext", "monaspace"),
        403,
        r#"{"message":"API rate limit exceeded for 127.0.0.1. Authenticated requests get a higher rate limit.","documentation_url":"https://docs.github.com/rest/overview/resources-in-the-rest-api#rate-limiting"}"#,
    );
    let app = fontbrew_with_server(paths.clone(), &server);

    let error = app
        .install_plan(github_request("githubnext", "monaspace", None))
        .await
        .expect_err("rate-limited GitHub API request should fail");

    assert!(matches!(&error, FontbrewError::Network { .. }));
    let message = error.to_string();
    assert!(message.contains("GitHub API rate limit exceeded"));
    assert!(message.contains("GITHUB_TOKEN"));
    assert!(message
        .contains("https://docs.github.com/rest/overview/resources-in-the-rest-api#rate-limiting"));
    assert!(!paths.manifest_path().exists());
    assert!(staging_entries(&paths).is_empty());
}

#[tokio::test]
async fn direct_github_install_plan_is_noop_without_network_when_package_is_already_managed() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let first_server = LocalHttpServer::start();
    let source_code_pro_download = first_server.url(&download_path("source-code-pro.zip"));
    first_server.respond_text(
        &github_releases_path("adobe", "source-code-pro"),
        github_release_response("v1.2.3", "source-code-pro.zip", &source_code_pro_download),
    );
    first_server.respond_bytes(
        &download_path("source-code-pro.zip"),
        zip_with_fixture_font("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf"),
    );
    let app = fontbrew_with_server(paths.clone(), &first_server);
    let first_plan = app
        .install_plan(github_request("adobe", "source-code-pro", None))
        .await
        .expect("plan first install");
    apply_plan(&app, first_plan).await;

    let no_route_server = LocalHttpServer::start();
    let app = fontbrew_with_server(paths, &no_route_server);
    let plan = app
        .install_plan(github_request("adobe", "source-code-pro", None))
        .await
        .expect("already managed direct GitHub install should plan without network");

    assert!(plan.already_installed);
    assert!(plan.changes.is_empty());
    assert!(no_route_server.request_urls().is_empty());
}

#[tokio::test]
async fn oversized_github_asset_download_is_rejected_without_manifest_or_package_store() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let server = LocalHttpServer::start();
    let source_code_pro_download = server.url(&download_path("source-code-pro.zip"));
    server.respond_text(
        &github_releases_path("adobe", "source-code-pro"),
        github_release_response("v1.2.3", "source-code-pro.zip", &source_code_pro_download),
    );
    server.respond_content_length(&download_path("source-code-pro.zip"), 600 * 1024 * 1024);
    let app = fontbrew_with_server(paths.clone(), &server);

    let error = app
        .install_plan(github_request("adobe", "source-code-pro", None))
        .await
        .expect_err("oversized download should be rejected");

    assert!(matches!(
        error,
        FontbrewError::ArchiveRejected { reason } if reason.contains("download exceeds")
    ));
    assert!(!paths.manifest_path().exists());
    assert!(!paths.managed_store_dir().join("packages").exists());
}

#[tokio::test]
async fn github_install_fails_when_multiple_installable_assets_match() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let server = LocalHttpServer::start();
    server.respond_text(
        &github_releases_path("adobe", "source-code-pro"),
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
    let app = fontbrew_with_server(paths, &server);

    let error = app
        .install_plan(github_request("adobe", "source-code-pro", None))
        .await
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
    assert_eq!(
        server.request_urls(),
        vec![server.url(&github_releases_path("adobe", "source-code-pro"))],
        "ambiguous asset selection should stop before downloading an archive"
    );
}

#[tokio::test]
async fn github_asset_selector_resolves_asset_ambiguity() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let server = LocalHttpServer::start();
    let desktop_download = server.url(&download_path("source-code-pro-desktop.zip"));
    server.respond_text(
        &github_releases_path("adobe", "source-code-pro"),
        format!(
            r#"[
  {{
    "tag_name": "v1.2.3",
    "draft": false,
    "prerelease": false,
    "assets": [
      {{
        "name": "source-code-pro-desktop.zip",
        "browser_download_url": "{desktop_download}"
      }},
      {{
        "name": "source-code-pro-nerd-font.zip",
        "browser_download_url": "https://downloads.example/source-code-pro-nerd-font.zip"
      }}
    ]
  }}
]"#
        ),
    );
    server.respond_bytes(
        &download_path("source-code-pro-desktop.zip"),
        zip_with_fixture_font("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf"),
    );
    let app = fontbrew_with_server(paths, &server);

    let plan = app
        .install_plan(github_request(
            "adobe",
            "source-code-pro",
            Some("*desktop.zip"),
        ))
        .await
        .expect("selector should resolve one asset");
    let report = apply_plan(&app, plan).await;

    assert_eq!(report.package_id, package_id("source-code-pro"));
    assert_eq!(
        server.request_urls(),
        vec![
            server.url(&github_releases_path("adobe", "source-code-pro")),
            desktop_download,
        ]
    );
}

#[tokio::test]
async fn github_asset_selection_flow_downloads_selected_asset_once() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let server = LocalHttpServer::start();
    let releases_url = server.url(&github_releases_path("adobe", "source-code-pro"));
    let desktop_download = server.url(&download_path("source-code-pro-desktop.zip"));
    server.respond_text(
        &github_releases_path("adobe", "source-code-pro"),
        format!(
            r#"[
  {{
    "tag_name": "v1.2.3",
    "draft": false,
    "prerelease": false,
    "assets": [
      {{
        "name": "source-code-pro-desktop.zip",
        "browser_download_url": "{desktop_download}"
      }},
      {{
        "name": "source-code-pro-nerd-font.zip",
        "browser_download_url": "https://downloads.example/source-code-pro-nerd-font.zip"
      }}
    ]
  }}
]"#
        ),
    );
    server.respond_bytes(
        &download_path("source-code-pro-desktop.zip"),
        zip_with_fixture_font("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf"),
    );
    let app = fontbrew_with_server(paths, &server);

    let mut ambiguous_progress = RecordingProgress::default();
    let preparation = app
        .prepare_install(
            github_request("adobe", "source-code-pro", None),
            &mut ambiguous_progress,
            Arc::new(NeverCancelled),
        )
        .await
        .expect("ambiguous GitHub assets should return pending selection");
    let pending = match preparation {
        InstallPreparation::AssetSelection(pending) => pending,
        _ => panic!("ambiguous GitHub assets should ask the caller to choose"),
    };
    assert_eq!(pending.package_id(), &package_id("source-code-pro"));
    assert_eq!(
        pending.assets(),
        &[
            "source-code-pro-desktop.zip".to_string(),
            "source-code-pro-nerd-font.zip".to_string()
        ]
    );
    assert_eq!(
        ambiguous_progress
            .events
            .iter()
            .filter(|event| matches!(event, ProgressEvent::DownloadStarted { .. }))
            .count(),
        0,
        "ambiguous asset selection should not report archive download progress"
    );
    assert_eq!(
        server.request_urls(),
        vec![releases_url.clone()],
        "ambiguous asset selection should resolve release metadata once without downloading"
    );

    let mut selected_progress = RecordingProgress::default();
    let preparation = app
        .prepare_selected_asset(
            pending,
            "source-code-pro-desktop.zip".to_string(),
            &mut selected_progress,
            Arc::new(NeverCancelled),
        )
        .await
        .expect("selected asset should prepare install");
    assert_eq!(
        selected_progress
            .events
            .iter()
            .filter(|event| matches!(event, ProgressEvent::DownloadStarted { .. }))
            .count(),
        1,
        "selected asset prepare should report one archive download"
    );
    let plan = match preparation {
        InstallPreparation::Plan(plan) => plan,
        InstallPreparation::AssetSelection(_) => panic!("selected asset should not ask again"),
        InstallPreparation::FamilySelection(_) => panic!("single-family archive should plan"),
    };
    let report = apply_plan(&app, plan).await;

    assert_eq!(report.package_id, package_id("source-code-pro"));
    assert_eq!(
        server.request_urls(),
        vec![releases_url, desktop_download],
        "interactive asset selection should reuse resolved release metadata and download the selected asset only once"
    );
}

#[tokio::test]
async fn github_asset_selection_with_multiple_families_reuses_release_metadata() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let server = LocalHttpServer::start();
    let releases_url = server.url(&github_releases_path("adobe", "source-code-pro"));
    let desktop_download = server.url(&download_path("source-code-pro-desktop.zip"));
    server.respond_text(
        &github_releases_path("adobe", "source-code-pro"),
        format!(
            r#"[
  {{
    "tag_name": "v1.2.3",
    "draft": false,
    "prerelease": false,
    "assets": [
      {{
        "name": "source-code-pro-desktop.zip",
        "browser_download_url": "{desktop_download}"
      }},
      {{
        "name": "source-code-pro-nerd-font.zip",
        "browser_download_url": "https://downloads.example/source-code-pro-nerd-font.zip"
      }}
    ]
  }}
]"#
        ),
    );
    server.respond_bytes(
        &download_path("source-code-pro-desktop.zip"),
        zip_with_fixture_fonts(&[
            ("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf"),
            ("Inter-Variable.ttf", "Inter-Variable.ttf"),
        ]),
    );
    let app = fontbrew_with_server(paths, &server);

    let request = github_request_with_selected_families(
        "adobe",
        "source-code-pro",
        None,
        vec!["Source Code Pro", "Inter"],
    );
    let preparation = app
        .prepare_install(request, &mut NoProgress, Arc::new(NeverCancelled))
        .await
        .expect("ambiguous GitHub assets should return pending selection");
    let pending = match preparation {
        InstallPreparation::AssetSelection(pending) => pending,
        _ => panic!("ambiguous GitHub assets should ask the caller to choose"),
    };

    let preparation = app
        .prepare_selected_asset(
            pending,
            "source-code-pro-desktop.zip".to_string(),
            &mut NoProgress,
            Arc::new(NeverCancelled),
        )
        .await
        .expect("selected asset should prepare families");
    let pending_families = match preparation {
        InstallPreparation::FamilySelection(pending) => pending,
        _ => panic!("multiple selected families should reuse parsed archive"),
    };
    let plans = app
        .prepare_selected_families(
            pending_families,
            vec![FamilyName::new("Source Code Pro"), FamilyName::new("Inter")],
            &mut NoProgress,
            Arc::new(NeverCancelled),
        )
        .await
        .expect("selected families should plan");

    assert_eq!(plans.len(), 2);
    assert_eq!(
        server.request_urls(),
        vec![releases_url, desktop_download],
        "selected family planning should not re-request release metadata after asset selection"
    );
}
