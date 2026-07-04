use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{Cursor, Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Condvar, Mutex,
    },
};

use fontbrew_core::{
    fetch::{HttpClient, HttpRequest, HttpResponse},
    fs::{debug_fail_next_atomic_write, DebugAtomicWriteFailure},
    manifest::{ManifestPackageRecord, ManifestSource, ManifestStore, ManifestV1},
    platform::FontbrewPaths,
    registry::{RegistrySnapshotStore, RegistrySnapshotV1},
    tasks, CancellationToken, ExecutionPolicy, FamilyName, FontbrewApp, InstallRequest,
    InstallSource, PackageId, PackageVersion, PlanRisk, ProgressEvent, ProgressSink, UpdatePlan,
    UpdateRequest,
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

#[derive(Default)]
struct FakeHttpClient {
    routes: Mutex<BTreeMap<String, Vec<u8>>>,
    download_routes: Mutex<BTreeMap<String, Vec<u8>>>,
    requests: Mutex<Vec<String>>,
    probe: Option<Arc<ConcurrencyProbe>>,
}

impl FakeHttpClient {
    fn with_probe(probe: Arc<ConcurrencyProbe>) -> Self {
        Self {
            probe: Some(probe),
            ..Self::default()
        }
    }

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
}

impl HttpClient for FakeHttpClient {
    fn get(&self, request: HttpRequest) -> fontbrew_core::Result<HttpResponse> {
        self.requests
            .lock()
            .expect("requests lock")
            .push(request.url.clone());
        if let Some(probe) = &self.probe {
            probe.enter_release_request();
        }
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
        _max_bytes: u64,
        _cancellation: &dyn CancellationToken,
    ) -> fontbrew_core::Result<u64> {
        self.requests
            .lock()
            .expect("requests lock")
            .push(request.url.clone());
        let body = self
            .download_routes
            .lock()
            .expect("download routes lock")
            .get(&request.url)
            .cloned()
            .unwrap_or_else(|| panic!("unexpected HTTP download request: {}", request.url));

        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).expect("create download parent");
        }
        fs::write(destination, &body).expect("write fake download");

        Ok(body.len() as u64)
    }
}

struct ConcurrencyProbe {
    active: AtomicUsize,
    max_active: AtomicUsize,
    release_entries: Mutex<usize>,
    release_gate: Condvar,
    wait_for_first_entries: usize,
}

impl ConcurrencyProbe {
    fn new(wait_for_first_entries: usize) -> Self {
        Self {
            active: AtomicUsize::new(0),
            max_active: AtomicUsize::new(0),
            release_entries: Mutex::new(0),
            release_gate: Condvar::new(),
            wait_for_first_entries,
        }
    }

    fn enter_release_request(&self) {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(active, Ordering::SeqCst);

        if self.wait_for_first_entries > 1 {
            let mut entries = self.release_entries.lock().expect("release entries lock");
            if *entries < self.wait_for_first_entries {
                *entries += 1;
                self.release_gate.notify_all();
                while *entries < self.wait_for_first_entries {
                    entries = self.release_gate.wait(entries).expect("release gate wait");
                }
            }
        }

        self.active.fetch_sub(1, Ordering::SeqCst);
    }

    fn max_active(&self) -> usize {
        self.max_active.load(Ordering::SeqCst)
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

fn github_releases_url(owner: &str, repo: &str) -> String {
    format!("https://api.github.com/repos/{owner}/{repo}/releases")
}

fn zip_with_fixture_font(entry_name: &str, fixture_name: &str) -> Vec<u8> {
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .unix_permissions(0o100644);

    zip.start_file(entry_name, options)
        .expect("start archive entry");

    let mut fixture = File::open(fixture_font_path(fixture_name)).expect("open fixture font");
    let mut bytes = Vec::new();
    fixture.read_to_end(&mut bytes).expect("read fixture font");
    zip.write_all(&bytes).expect("write archive entry");

    zip.finish().expect("finish zip").into_inner()
}

fn github_release_json(version: &str, asset_name: &str, download_url: &str) -> String {
    format!(
        r#"[{{
  "tag_name": "{version}",
  "draft": false,
  "prerelease": false,
  "assets": [
    {{"name": "{asset_name}", "browser_download_url": "{download_url}"}}
  ]
}}]"#
    )
}

fn update_request(package_ids: Vec<PackageId>, jobs: Option<usize>) -> UpdateRequest {
    UpdateRequest { package_ids, jobs }
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

fn prepare_source_code_pro_update(paths: &FontbrewPaths) -> (FontbrewApp, UpdatePlan) {
    install_github_source_code_pro(paths, "v1.0.0");
    let update_http = Arc::new(FakeHttpClient::default());
    update_http.with_text(
        &github_releases_url("adobe", "source-code-pro"),
        github_release_json(
            "v2.0.0",
            "source-code-pro.zip",
            "https://downloads.example/source-code-pro-v2.zip",
        ),
    );
    update_http.with_download_bytes(
        "https://downloads.example/source-code-pro-v2.zip",
        zip_with_fixture_font("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf"),
    );
    let app = FontbrewApp::with_paths_and_http_client(paths.clone(), update_http);
    let mut progress = NoProgress;
    let plan = app
        .update_plan(
            update_request(vec![package_id("source-code-pro")], Some(1)),
            &mut progress,
            &NeverCancelled,
        )
        .expect("plan update");

    (app, plan)
}

fn manifest_record(
    package_id_text: &str,
    version: &str,
    family: &str,
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
        families: vec![FamilyName::new(family)],
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

fn write_source_code_pro_registry_snapshot(paths: &FontbrewPaths) {
    let snapshot = RegistrySnapshotV1::parse(
        r#"{
  "schemaVersion": 1,
  "updatedAt": "2026-07-03T00:00:00Z",
  "packages": {
    "source-code-pro": {
      "name": "Source Code Pro",
      "source": { "type": "github", "repo": "adobe/source-code-pro" },
      "families": ["Source Code Pro"],
      "asset": {
        "include": ["*desktop*.zip"],
        "exclude": ["*web*"]
      }
    }
  }
}"#,
    )
    .expect("parse registry snapshot");

    RegistrySnapshotStore::new(paths.clone())
        .write_snapshot(&snapshot)
        .expect("write registry snapshot");
}

fn install_github_source_code_pro(
    paths: &FontbrewPaths,
    version: &str,
) -> (FontbrewApp, Arc<FakeHttpClient>) {
    install_github_source_code_pro_with_entries(
        paths,
        version,
        &[("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf")],
    )
}

fn install_github_source_code_pro_with_entries(
    paths: &FontbrewPaths,
    version: &str,
    entries: &[(&str, &str)],
) -> (FontbrewApp, Arc<FakeHttpClient>) {
    let fake_http = Arc::new(FakeHttpClient::default());
    fake_http.with_text(
        &github_releases_url("adobe", "source-code-pro"),
        github_release_json(
            version,
            "source-code-pro.zip",
            "https://downloads.example/source-code-pro.zip",
        ),
    );
    fake_http.with_download_bytes(
        "https://downloads.example/source-code-pro.zip",
        zip_with_fixture_fonts(entries),
    );
    let app = FontbrewApp::with_paths_and_http_client(paths.clone(), fake_http.clone());
    let plan = app
        .install_plan(InstallRequest {
            source: InstallSource::GitHubRepo {
                owner: "adobe".to_string(),
                repo: "source-code-pro".to_string(),
            },
            package_id_override: None,
            format_preference: Vec::new(),
            asset_selector: None,
            selected_families: Vec::new(),
            reinstall: false,
        })
        .expect("plan GitHub install");
    let mut progress = NoProgress;
    app.apply_install(
        plan,
        ExecutionPolicy::SafeOnly,
        &mut progress,
        &NeverCancelled,
    )
    .expect("apply GitHub install");

    (app, fake_http)
}

fn zip_with_fixture_fonts(entries: &[(&str, &str)]) -> Vec<u8> {
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .unix_permissions(0o100644);

    for (entry_name, fixture_name) in entries {
        zip.start_file(entry_name, options)
            .expect("start archive entry");

        let mut fixture = File::open(fixture_font_path(fixture_name)).expect("open fixture font");
        let mut bytes = Vec::new();
        fixture.read_to_end(&mut bytes).expect("read fixture font");
        zip.write_all(&bytes).expect("write archive entry");
    }

    zip.finish().expect("finish zip").into_inner()
}

#[test]
fn task_runner_respects_bounded_limit_without_tokio() {
    let serial_probe = Arc::new(ConcurrencyProbe::new(1));
    tasks::map_bounded(vec![0, 1, 2], 1, {
        let probe = serial_probe.clone();
        move |_| probe.enter_release_request()
    });
    assert_eq!(serial_probe.max_active(), 1);

    let parallel_probe = Arc::new(ConcurrencyProbe::new(2));
    tasks::map_bounded(vec![0, 1, 2], 2, {
        let probe = parallel_probe.clone();
        move |_| probe.enter_release_request()
    });
    assert_eq!(parallel_probe.max_active(), 2);
}

#[test]
fn update_apply_policy_failure_cleans_prepared_staging() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let (app, mut plan) = prepare_source_code_pro_update(&paths);
    assert!(
        staging_entries(&paths)
            .iter()
            .any(|entry| entry.starts_with("install-")),
        "update planning should create staging"
    );
    plan.risks.push(PlanRisk::Conflict {
        package_id: package_id("source-code-pro"),
        description: "forced update risk for policy cleanup test".to_string(),
    });

    let error = app
        .apply_update(
            plan,
            ExecutionPolicy::SafeOnly,
            &mut NoProgress,
            &NeverCancelled,
        )
        .expect_err("safe policy should reject risky update");

    assert!(matches!(
        error,
        fontbrew_core::FontbrewError::ExecutionPolicyRequired { .. }
    ));
    assert!(staging_entries(&paths).is_empty());
}

#[test]
fn discard_update_plan_cleans_prepared_staging() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let (app, plan) = prepare_source_code_pro_update(&paths);
    assert!(
        staging_entries(&paths)
            .iter()
            .any(|entry| entry.starts_with("install-")),
        "update planning should create staging"
    );

    app.discard_update_plan(plan);

    assert!(staging_entries(&paths).is_empty());
}

#[test]
fn update_apply_manifest_read_failure_cleans_prepared_staging() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let (app, plan) = prepare_source_code_pro_update(&paths);
    assert!(
        staging_entries(&paths)
            .iter()
            .any(|entry| entry.starts_with("install-")),
        "update planning should create staging"
    );
    fs::write(paths.manifest_path(), b"{not valid json").expect("corrupt manifest");

    let error = app
        .apply_update(
            plan,
            ExecutionPolicy::SafeOnly,
            &mut NoProgress,
            &NeverCancelled,
        )
        .expect_err("manifest read should fail");

    assert!(matches!(
        error,
        fontbrew_core::FontbrewError::Manifest { .. }
    ));
    assert!(staging_entries(&paths).is_empty());
}

#[test]
fn update_plan_cancellation_after_resolved_github_staging_creation_cleans_staging() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    install_github_source_code_pro(&paths, "v1.0.0");
    assert!(staging_entries(&paths).is_empty());

    let update_http = Arc::new(FakeHttpClient::default());
    update_http.with_text(
        &github_releases_url("adobe", "source-code-pro"),
        github_release_json(
            "v2.0.0",
            "source-code-pro.zip",
            "https://downloads.example/source-code-pro-v2.zip",
        ),
    );
    let app = FontbrewApp::with_paths_and_http_client(paths.clone(), update_http);
    let mut progress = NoProgress;

    let plan = app
        .update_plan(
            update_request(vec![package_id("source-code-pro")], Some(1)),
            &mut progress,
            &CancelWhenInstallStagingExists {
                paths: paths.clone(),
            },
        )
        .expect("update plan should record per-package cancellation failure");

    assert!(plan.prepared.is_empty());
    assert_eq!(plan.failed.len(), 1);
    assert_eq!(plan.failed[0].package_id, package_id("source-code-pro"));
    assert!(plan.failed[0].reason.contains("operation cancelled"));
    assert!(staging_entries(&paths).is_empty());
}

#[test]
fn update_prepare_partial_failure_does_not_block_other_prepared_packages() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    write_manifest(
        &paths,
        vec![
            manifest_record(
                "source-code-pro",
                "v1.0.0",
                "Source Code Pro",
                ManifestSource::GitHub {
                    owner: "adobe".to_string(),
                    repo: "source-code-pro".to_string(),
                },
                None,
            ),
            manifest_record(
                "inter",
                "v1.0.0",
                "Inter",
                ManifestSource::GitHub {
                    owner: "rsms".to_string(),
                    repo: "inter".to_string(),
                },
                None,
            ),
        ],
    );
    let fake_http = Arc::new(FakeHttpClient::default());
    fake_http.with_text(
        &github_releases_url("adobe", "source-code-pro"),
        github_release_json(
            "v2.0.0",
            "source-code-pro.zip",
            "https://downloads.example/source-code-pro.zip",
        ),
    );
    fake_http.with_download_bytes(
        "https://downloads.example/source-code-pro.zip",
        zip_with_fixture_font("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf"),
    );
    fake_http.with_text(
        &github_releases_url("rsms", "inter"),
        r#"[{"tag_name":"v2.0.0","draft":false,"prerelease":false,"assets":[]}]"#,
    );
    let app = FontbrewApp::with_paths_and_http_client(paths.clone(), fake_http);

    let mut progress = NoProgress;
    let plan = app
        .update_plan(
            update_request(Vec::new(), Some(2)),
            &mut progress,
            &NeverCancelled,
        )
        .expect("plan updates with partial prepare failure");

    assert_eq!(plan.prepared.len(), 1);
    assert_eq!(plan.prepared[0].package_id, package_id("source-code-pro"));
    assert_eq!(plan.prepared[0].target_version.as_str(), "v2.0.0");
    assert_eq!(plan.failed.len(), 1);
    assert_eq!(plan.failed[0].package_id, package_id("inter"));
    assert!(plan.failed[0].reason.contains("no matching installable"));
    assert_eq!(
        ManifestStore::read_or_empty(&paths.manifest_path())
            .expect("read manifest")
            .get_package(&package_id("source-code-pro"))
            .expect("manifest record")
            .version
            .as_str(),
        "v1.0.0"
    );
}

#[test]
fn update_prepare_identity_mismatch_fails_that_package_only() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    write_manifest(
        &paths,
        vec![manifest_record(
            "source-code-pro",
            "v1.0.0",
            "Source Code Pro",
            ManifestSource::GitHub {
                owner: "adobe".to_string(),
                repo: "source-code-pro".to_string(),
            },
            None,
        )],
    );
    let fake_http = Arc::new(FakeHttpClient::default());
    fake_http.with_text(
        &github_releases_url("adobe", "source-code-pro"),
        github_release_json(
            "v2.0.0",
            "source-code-pro.zip",
            "https://downloads.example/source-code-pro.zip",
        ),
    );
    fake_http.with_download_bytes(
        "https://downloads.example/source-code-pro.zip",
        zip_with_fixture_font("Inter-Variable.ttf", "Inter-Variable.ttf"),
    );
    let app = FontbrewApp::with_paths_and_http_client(paths, fake_http);

    let mut progress = NoProgress;
    let plan = app
        .update_plan(
            update_request(Vec::new(), Some(1)),
            &mut progress,
            &NeverCancelled,
        )
        .expect("identity mismatch is reported in plan");

    assert!(plan.prepared.is_empty());
    assert_eq!(plan.failed.len(), 1);
    assert_eq!(plan.failed[0].package_id, package_id("source-code-pro"));
    assert!(plan.failed[0].reason.contains("identity mismatch"));
}

#[test]
fn update_prepare_reuses_registry_asset_selection_for_registry_packages() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    write_source_code_pro_registry_snapshot(&paths);
    write_manifest(
        &paths,
        vec![manifest_record(
            "source-code-pro",
            "v1.0.0",
            "Source Code Pro",
            ManifestSource::Registry {
                id: "source-code-pro".to_string(),
            },
            Some(ManifestSource::GitHub {
                owner: "adobe".to_string(),
                repo: "source-code-pro".to_string(),
            }),
        )],
    );
    let fake_http = Arc::new(FakeHttpClient::default());
    fake_http.with_text(
        &github_releases_url("adobe", "source-code-pro"),
        r#"[
  {
    "tag_name": "v2.0.0",
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
    let app = FontbrewApp::with_paths_and_http_client(paths, fake_http);

    let mut progress = NoProgress;
    let plan = app
        .update_plan(
            update_request(vec![package_id("source-code-pro")], Some(1)),
            &mut progress,
            &NeverCancelled,
        )
        .expect("registry asset selection should resolve update asset");

    assert_eq!(plan.prepared.len(), 1);
    assert!(plan.failed.is_empty());
    assert_eq!(plan.prepared[0].target_version.as_str(), "v2.0.0");
}

#[test]
fn direct_github_update_reuses_manifest_family_boundary() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    write_manifest(
        &paths,
        vec![manifest_record(
            "source-code-pro",
            "v1.0.0",
            "Source Code Pro",
            ManifestSource::GitHub {
                owner: "adobe".to_string(),
                repo: "source-code-pro".to_string(),
            },
            Some(ManifestSource::GitHub {
                owner: "adobe".to_string(),
                repo: "source-code-pro".to_string(),
            }),
        )],
    );
    let fake_http = Arc::new(FakeHttpClient::default());
    fake_http.with_text(
        &github_releases_url("adobe", "source-code-pro"),
        github_release_json(
            "v2.0.0",
            "source-code-pro.zip",
            "https://downloads.example/source-code-pro.zip",
        ),
    );
    fake_http.with_download_bytes(
        "https://downloads.example/source-code-pro.zip",
        zip_with_fixture_fonts(&[
            ("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf"),
            ("Inter-Variable.ttf", "Inter-Variable.ttf"),
        ]),
    );
    let app = FontbrewApp::with_paths_and_http_client(paths.clone(), fake_http);

    let mut progress = NoProgress;
    let plan = app
        .update_plan(
            update_request(vec![package_id("source-code-pro")], Some(1)),
            &mut progress,
            &NeverCancelled,
        )
        .expect("direct GitHub update should reuse manifest family boundary");

    assert_eq!(plan.prepared.len(), 1);
    assert!(plan.failed.is_empty());
    assert_eq!(plan.prepared[0].package_id, package_id("source-code-pro"));
    assert_eq!(plan.prepared[0].target_version.as_str(), "v2.0.0");
    assert!(staging_entries(&paths).len() <= 1);
}

#[test]
fn update_prepare_uses_registry_recipe_family_boundary() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let snapshot = RegistrySnapshotV1::parse(
        r#"{
  "schemaVersion": 1,
  "updatedAt": "2026-07-03T00:00:00Z",
  "packages": {
    "source-code-pro": {
      "name": "Source Code Pro",
      "source": { "type": "github", "repo": "adobe/source-code-pro" },
      "families": ["Source Code Pro", "Inter"],
      "asset": {
        "include": ["*.zip"],
        "exclude": []
      }
    }
  }
}"#,
    )
    .expect("parse registry snapshot");
    RegistrySnapshotStore::new(paths.clone())
        .write_snapshot(&snapshot)
        .expect("write registry snapshot");
    write_manifest(
        &paths,
        vec![manifest_record(
            "source-code-pro",
            "v1.0.0",
            "Source Code Pro",
            ManifestSource::Registry {
                id: "source-code-pro".to_string(),
            },
            Some(ManifestSource::GitHub {
                owner: "adobe".to_string(),
                repo: "source-code-pro".to_string(),
            }),
        )],
    );
    let fake_http = Arc::new(FakeHttpClient::default());
    fake_http.with_text(
        &github_releases_url("adobe", "source-code-pro"),
        github_release_json(
            "v2.0.0",
            "source-code-pro.zip",
            "https://downloads.example/source-code-pro.zip",
        ),
    );
    fake_http.with_download_bytes(
        "https://downloads.example/source-code-pro.zip",
        zip_with_fixture_font("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf"),
    );
    let app = FontbrewApp::with_paths_and_http_client(paths.clone(), fake_http);

    let mut progress = NoProgress;
    let plan = app
        .update_plan(
            update_request(vec![package_id("source-code-pro")], Some(1)),
            &mut progress,
            &NeverCancelled,
        )
        .expect("registry boundary failure should be reported in update plan");

    assert!(plan.prepared.is_empty());
    assert_eq!(plan.failed.len(), 1);
    assert_eq!(plan.failed[0].package_id, package_id("source-code-pro"));
    assert!(plan.failed[0]
        .reason
        .contains("missing expected registry recipe font families"));
    assert!(plan.failed[0].reason.contains("Inter"));
    assert!(staging_entries(&paths).is_empty());
}

#[test]
fn update_prepare_matches_registry_expected_families_after_normalization() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let snapshot = RegistrySnapshotV1::parse(
        r#"{
  "schemaVersion": 1,
  "updatedAt": "2026-07-03T00:00:00Z",
  "packages": {
    "source-code-pro": {
      "name": "Source Code Pro",
      "source": { "type": "github", "repo": "adobe/source-code-pro" },
      "families": [" source   code pro "],
      "asset": {
        "include": ["*.zip"],
        "exclude": []
      }
    }
  }
}"#,
    )
    .expect("parse registry snapshot");
    RegistrySnapshotStore::new(paths.clone())
        .write_snapshot(&snapshot)
        .expect("write registry snapshot");
    write_manifest(
        &paths,
        vec![manifest_record(
            "source-code-pro",
            "v1.0.0",
            "Source Code Pro",
            ManifestSource::Registry {
                id: "source-code-pro".to_string(),
            },
            Some(ManifestSource::GitHub {
                owner: "adobe".to_string(),
                repo: "source-code-pro".to_string(),
            }),
        )],
    );
    let fake_http = Arc::new(FakeHttpClient::default());
    fake_http.with_text(
        &github_releases_url("adobe", "source-code-pro"),
        github_release_json(
            "v2.0.0",
            "source-code-pro.zip",
            "https://downloads.example/source-code-pro.zip",
        ),
    );
    fake_http.with_download_bytes(
        "https://downloads.example/source-code-pro.zip",
        zip_with_fixture_font("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf"),
    );
    let app = FontbrewApp::with_paths_and_http_client(paths, fake_http);

    let mut progress = NoProgress;
    let plan = app
        .update_plan(
            update_request(vec![package_id("source-code-pro")], Some(1)),
            &mut progress,
            &NeverCancelled,
        )
        .expect("normalized registry expected family should match prepared family");

    assert_eq!(plan.prepared.len(), 1);
    assert!(plan.failed.is_empty());
    assert_eq!(plan.prepared[0].package_id, package_id("source-code-pro"));
}

#[test]
fn update_prepare_uses_bounded_parallelism_for_github_checks() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    write_manifest(
        &paths,
        vec![
            manifest_record(
                "source-code-pro",
                "v1.0.0",
                "Source Code Pro",
                ManifestSource::GitHub {
                    owner: "adobe".to_string(),
                    repo: "source-code-pro".to_string(),
                },
                None,
            ),
            manifest_record(
                "inter",
                "v1.0.0",
                "Inter",
                ManifestSource::GitHub {
                    owner: "rsms".to_string(),
                    repo: "inter".to_string(),
                },
                None,
            ),
        ],
    );

    let serial_probe = Arc::new(ConcurrencyProbe::new(1));
    let serial_http = Arc::new(FakeHttpClient::with_probe(serial_probe.clone()));
    seed_two_successful_updates(&serial_http);
    let serial_app = FontbrewApp::with_paths_and_http_client(paths.clone(), serial_http);
    let mut progress = NoProgress;
    serial_app
        .update_plan(
            update_request(Vec::new(), Some(1)),
            &mut progress,
            &NeverCancelled,
        )
        .expect("serial update plan");
    assert_eq!(serial_probe.max_active(), 1);

    let parallel_probe = Arc::new(ConcurrencyProbe::new(2));
    let parallel_http = Arc::new(FakeHttpClient::with_probe(parallel_probe.clone()));
    seed_two_successful_updates(&parallel_http);
    let parallel_app = FontbrewApp::with_paths_and_http_client(paths, parallel_http);
    parallel_app
        .update_plan(
            update_request(Vec::new(), Some(2)),
            &mut progress,
            &NeverCancelled,
        )
        .expect("parallel update plan");
    assert_eq!(parallel_probe.max_active(), 2);
}

fn seed_two_successful_updates(fake_http: &FakeHttpClient) {
    fake_http.with_text(
        &github_releases_url("adobe", "source-code-pro"),
        github_release_json(
            "v2.0.0",
            "source-code-pro.zip",
            "https://downloads.example/source-code-pro.zip",
        ),
    );
    fake_http.with_download_bytes(
        "https://downloads.example/source-code-pro.zip",
        zip_with_fixture_font("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf"),
    );
    fake_http.with_text(
        &github_releases_url("rsms", "inter"),
        github_release_json("v2.0.0", "inter.zip", "https://downloads.example/inter.zip"),
    );
    fake_http.with_download_bytes(
        "https://downloads.example/inter.zip",
        zip_with_fixture_font("Inter-Variable.ttf", "Inter-Variable.ttf"),
    );
}

#[test]
fn update_apply_failure_preserves_old_version_activation_and_manifest() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    install_github_source_code_pro(&paths, "v1.0.0");
    let old_store_path = paths
        .package_store_dir(
            &package_id("source-code-pro"),
            &PackageVersion::new("v1.0.0"),
        )
        .join("files/SourceCodePro-Regular.ttf");
    let old_activation_path = paths.activation_dir().join("SourceCodePro-Regular.ttf");
    assert_eq!(
        fs::read_link(&old_activation_path).expect("old activation symlink"),
        old_store_path
    );

    let update_http = Arc::new(FakeHttpClient::default());
    update_http.with_text(
        &github_releases_url("adobe", "source-code-pro"),
        github_release_json(
            "v2.0.0",
            "source-code-pro.zip",
            "https://downloads.example/source-code-pro-v2.zip",
        ),
    );
    update_http.with_download_bytes(
        "https://downloads.example/source-code-pro-v2.zip",
        zip_with_fixture_font("SourceCodePro-Regular-v2.ttf", "SourceCodePro-Regular.ttf"),
    );
    let app = FontbrewApp::with_paths_and_http_client(paths.clone(), update_http);
    let mut progress = NoProgress;
    let plan = app
        .update_plan(
            update_request(vec![package_id("source-code-pro")], Some(1)),
            &mut progress,
            &NeverCancelled,
        )
        .expect("plan update");
    assert_eq!(plan.prepared.len(), 1);

    let conflict_path = paths.activation_dir().join("SourceCodePro-Regular-v2.ttf");
    fs::write(&conflict_path, b"unmanaged").expect("write unmanaged activation conflict");

    let report = app
        .apply_update(
            plan,
            ExecutionPolicy::SafeOnly,
            &mut progress,
            &NeverCancelled,
        )
        .expect("apply reports per-package failure");

    assert!(report.updated.is_empty());
    assert_eq!(report.skipped.len(), 1);
    assert_eq!(report.skipped[0].package_id, package_id("source-code-pro"));
    assert_eq!(
        ManifestStore::read_or_empty(&paths.manifest_path())
            .expect("read manifest")
            .get_package(&package_id("source-code-pro"))
            .expect("manifest record")
            .version
            .as_str(),
        "v1.0.0"
    );
    assert!(old_store_path.exists());
    assert!(!paths
        .package_store_dir(
            &package_id("source-code-pro"),
            &PackageVersion::new("v2.0.0")
        )
        .exists());
    assert_eq!(
        fs::read_link(&old_activation_path).expect("old activation restored"),
        old_store_path
    );
}

#[test]
fn update_apply_new_activation_mid_failure_removes_partial_new_activation_and_restores_old() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    install_github_source_code_pro_with_entries(
        &paths,
        "v1.0.0",
        &[
            ("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf"),
            ("SourceCodePro-Bold.ttf", "SourceCodePro-Bold.ttf"),
        ],
    );
    let old_regular_store_path = paths
        .package_store_dir(
            &package_id("source-code-pro"),
            &PackageVersion::new("v1.0.0"),
        )
        .join("files/SourceCodePro-Regular.ttf");
    let old_bold_store_path = paths
        .package_store_dir(
            &package_id("source-code-pro"),
            &PackageVersion::new("v1.0.0"),
        )
        .join("files/SourceCodePro-Bold.ttf");
    let old_regular_activation_path = paths.activation_dir().join("SourceCodePro-Regular.ttf");
    let old_bold_activation_path = paths.activation_dir().join("SourceCodePro-Bold.ttf");

    let update_http = Arc::new(FakeHttpClient::default());
    update_http.with_text(
        &github_releases_url("adobe", "source-code-pro"),
        github_release_json(
            "v2.0.0",
            "source-code-pro.zip",
            "https://downloads.example/source-code-pro-v2.zip",
        ),
    );
    update_http.with_download_bytes(
        "https://downloads.example/source-code-pro-v2.zip",
        zip_with_fixture_fonts(&[
            ("SourceCodePro-NewA.ttf", "SourceCodePro-Regular.ttf"),
            ("SourceCodePro-NewB.ttf", "SourceCodePro-Bold.ttf"),
        ]),
    );
    let app = FontbrewApp::with_paths_and_http_client(paths.clone(), update_http);
    let mut progress = NoProgress;
    let plan = app
        .update_plan(
            update_request(vec![package_id("source-code-pro")], Some(1)),
            &mut progress,
            &NeverCancelled,
        )
        .expect("plan update");
    let partial_new_activation = paths.activation_dir().join("SourceCodePro-NewA.ttf");
    let conflict_path = paths.activation_dir().join("SourceCodePro-NewB.ttf");
    fs::write(&conflict_path, b"unmanaged").expect("write unmanaged activation conflict");

    let report = app
        .apply_update(
            plan,
            ExecutionPolicy::SafeOnly,
            &mut progress,
            &NeverCancelled,
        )
        .expect("apply reports per-package failure");

    assert!(report.updated.is_empty());
    assert_eq!(report.skipped.len(), 1);
    assert_eq!(
        ManifestStore::read_or_empty(&paths.manifest_path())
            .expect("read manifest")
            .get_package(&package_id("source-code-pro"))
            .expect("manifest record")
            .version
            .as_str(),
        "v1.0.0"
    );
    assert_eq!(
        fs::read_link(&old_regular_activation_path).expect("regular activation restored"),
        old_regular_store_path
    );
    assert_eq!(
        fs::read_link(&old_bold_activation_path).expect("bold activation restored"),
        old_bold_store_path
    );
    assert!(
        fs::symlink_metadata(&partial_new_activation).is_err(),
        "partial new activation should be removed after failure"
    );
    assert!(!paths
        .package_store_dir(
            &package_id("source-code-pro"),
            &PackageVersion::new("v2.0.0")
        )
        .exists());
}

#[test]
fn update_apply_old_activation_deactivation_mid_failure_restores_removed_old_activation() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    install_github_source_code_pro_with_entries(
        &paths,
        "v1.0.0",
        &[
            ("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf"),
            ("SourceCodePro-Bold.ttf", "SourceCodePro-Bold.ttf"),
        ],
    );
    let old_regular_store_path = paths
        .package_store_dir(
            &package_id("source-code-pro"),
            &PackageVersion::new("v1.0.0"),
        )
        .join("files/SourceCodePro-Regular.ttf");
    let old_bold_store_path = paths
        .package_store_dir(
            &package_id("source-code-pro"),
            &PackageVersion::new("v1.0.0"),
        )
        .join("files/SourceCodePro-Bold.ttf");
    let old_regular_activation_path = paths.activation_dir().join("SourceCodePro-Regular.ttf");
    let old_bold_activation_path = paths.activation_dir().join("SourceCodePro-Bold.ttf");

    let update_http = Arc::new(FakeHttpClient::default());
    update_http.with_text(
        &github_releases_url("adobe", "source-code-pro"),
        github_release_json(
            "v2.0.0",
            "source-code-pro.zip",
            "https://downloads.example/source-code-pro-v2.zip",
        ),
    );
    update_http.with_download_bytes(
        "https://downloads.example/source-code-pro-v2.zip",
        zip_with_fixture_fonts(&[
            ("SourceCodePro-NewA.ttf", "SourceCodePro-Regular.ttf"),
            ("SourceCodePro-NewB.ttf", "SourceCodePro-Bold.ttf"),
        ]),
    );
    let app = FontbrewApp::with_paths_and_http_client(paths.clone(), update_http);
    let mut progress = NoProgress;
    let plan = app
        .update_plan(
            update_request(vec![package_id("source-code-pro")], Some(1)),
            &mut progress,
            &NeverCancelled,
        )
        .expect("plan update");
    fs::remove_file(&old_bold_activation_path).expect("remove old bold activation");
    fs::write(&old_bold_activation_path, b"unmanaged").expect("replace old bold activation");

    let report = app
        .apply_update(
            plan,
            ExecutionPolicy::SafeOnly,
            &mut progress,
            &NeverCancelled,
        )
        .expect("apply reports per-package failure");

    assert!(report.updated.is_empty());
    assert_eq!(report.skipped.len(), 1);
    assert_eq!(
        ManifestStore::read_or_empty(&paths.manifest_path())
            .expect("read manifest")
            .get_package(&package_id("source-code-pro"))
            .expect("manifest record")
            .version
            .as_str(),
        "v1.0.0"
    );
    assert_eq!(
        fs::read_link(&old_regular_activation_path).expect("regular activation restored"),
        old_regular_store_path
    );
    assert_eq!(
        fs::read(&old_bold_activation_path).expect("bold activation conflict remains"),
        b"unmanaged"
    );
    assert!(old_bold_store_path.exists());
    assert!(!paths
        .package_store_dir(
            &package_id("source-code-pro"),
            &PackageVersion::new("v2.0.0")
        )
        .exists());
}

#[test]
fn update_apply_copy_failure_removes_partial_new_package_store() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    install_github_source_code_pro(&paths, "v1.0.0");

    let update_http = Arc::new(FakeHttpClient::default());
    update_http.with_text(
        &github_releases_url("adobe", "source-code-pro"),
        github_release_json(
            "v2.0.0",
            "source-code-pro.zip",
            "https://downloads.example/source-code-pro-v2.zip",
        ),
    );
    update_http.with_download_bytes(
        "https://downloads.example/source-code-pro-v2.zip",
        zip_with_fixture_font("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf"),
    );
    let app = FontbrewApp::with_paths_and_http_client(paths.clone(), update_http);
    let mut progress = NoProgress;
    let plan = app
        .update_plan(
            update_request(vec![package_id("source-code-pro")], Some(1)),
            &mut progress,
            &NeverCancelled,
        )
        .expect("plan update");
    fs::remove_file(first_staged_font_file(&paths.staging_dir()))
        .expect("remove staged font before apply");

    let report = app
        .apply_update(
            plan,
            ExecutionPolicy::SafeOnly,
            &mut progress,
            &NeverCancelled,
        )
        .expect("apply reports copy failure");

    assert!(report.updated.is_empty());
    assert_eq!(report.skipped.len(), 1);
    assert!(!paths
        .package_store_dir(
            &package_id("source-code-pro"),
            &PackageVersion::new("v2.0.0")
        )
        .exists());
    assert_eq!(
        ManifestStore::read_or_empty(&paths.manifest_path())
            .expect("read manifest")
            .get_package(&package_id("source-code-pro"))
            .expect("manifest record")
            .version
            .as_str(),
        "v1.0.0"
    );
}

#[test]
fn update_apply_manifest_write_uncertain_failure_keeps_new_files_if_manifest_may_reference_them() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    install_github_source_code_pro(&paths, "v1.0.0");

    let update_http = Arc::new(FakeHttpClient::default());
    update_http.with_text(
        &github_releases_url("adobe", "source-code-pro"),
        github_release_json(
            "v2.0.0",
            "source-code-pro.zip",
            "https://downloads.example/source-code-pro-v2.zip",
        ),
    );
    update_http.with_download_bytes(
        "https://downloads.example/source-code-pro-v2.zip",
        zip_with_fixture_font("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf"),
    );
    let app = FontbrewApp::with_paths_and_http_client(paths.clone(), update_http);
    let mut progress = NoProgress;
    let plan = app
        .update_plan(
            update_request(vec![package_id("source-code-pro")], Some(1)),
            &mut progress,
            &NeverCancelled,
        )
        .expect("plan update");

    debug_fail_next_atomic_write(
        &paths.manifest_path(),
        DebugAtomicWriteFailure::AfterPersist,
    );

    let report = app
        .apply_update(
            plan,
            ExecutionPolicy::SafeOnly,
            &mut progress,
            &NeverCancelled,
        )
        .expect("apply reports manifest write failure");

    assert!(report.updated.is_empty());
    assert_eq!(report.skipped.len(), 1);
    assert!(report.skipped[0]
        .reason
        .contains("commit state is uncertain"));

    let manifest = ManifestStore::read_or_empty(&paths.manifest_path()).expect("read manifest");
    let record = manifest
        .get_package(&package_id("source-code-pro"))
        .expect("manifest record");
    if record.version.as_str() == "v2.0.0" {
        let new_store_path = paths
            .package_store_dir(
                &package_id("source-code-pro"),
                &PackageVersion::new("v2.0.0"),
            )
            .join("files/SourceCodePro-Regular.ttf");
        assert!(new_store_path.exists());
        assert_eq!(
            fs::read_link(paths.activation_dir().join("SourceCodePro-Regular.ttf"))
                .expect("new activation symlink"),
            new_store_path
        );
    }
}

#[test]
fn update_apply_manifest_write_not_committed_failure_restores_old_state_and_removes_new_store() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    install_github_source_code_pro(&paths, "v1.0.0");
    let old_store_path = paths
        .package_store_dir(
            &package_id("source-code-pro"),
            &PackageVersion::new("v1.0.0"),
        )
        .join("files/SourceCodePro-Regular.ttf");
    let old_activation_path = paths.activation_dir().join("SourceCodePro-Regular.ttf");

    let update_http = Arc::new(FakeHttpClient::default());
    update_http.with_text(
        &github_releases_url("adobe", "source-code-pro"),
        github_release_json(
            "v2.0.0",
            "source-code-pro.zip",
            "https://downloads.example/source-code-pro-v2.zip",
        ),
    );
    update_http.with_download_bytes(
        "https://downloads.example/source-code-pro-v2.zip",
        zip_with_fixture_font("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf"),
    );
    let app = FontbrewApp::with_paths_and_http_client(paths.clone(), update_http);
    let mut progress = NoProgress;
    let plan = app
        .update_plan(
            update_request(vec![package_id("source-code-pro")], Some(1)),
            &mut progress,
            &NeverCancelled,
        )
        .expect("plan update");
    debug_fail_next_atomic_write(
        &paths.manifest_path(),
        DebugAtomicWriteFailure::BeforePersist,
    );

    let report = app
        .apply_update(
            plan,
            ExecutionPolicy::SafeOnly,
            &mut progress,
            &NeverCancelled,
        )
        .expect("apply reports manifest write failure");

    assert!(report.updated.is_empty());
    assert_eq!(report.skipped.len(), 1);
    assert!(report.skipped[0]
        .reason
        .contains("manifest write did not commit"));
    assert_eq!(
        ManifestStore::read_or_empty(&paths.manifest_path())
            .expect("read manifest")
            .get_package(&package_id("source-code-pro"))
            .expect("manifest record")
            .version
            .as_str(),
        "v1.0.0"
    );
    assert!(old_store_path.exists());
    assert_eq!(
        fs::read_link(&old_activation_path).expect("old activation restored"),
        old_store_path
    );
    assert!(!paths
        .package_store_dir(
            &package_id("source-code-pro"),
            &PackageVersion::new("v2.0.0")
        )
        .exists());
}

#[test]
fn update_apply_success_points_manifest_and_activation_to_new_version_and_removes_old_store() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    install_github_source_code_pro(&paths, "v1.0.0");
    let old_store_dir = paths.package_store_dir(
        &package_id("source-code-pro"),
        &PackageVersion::new("v1.0.0"),
    );

    let update_http = Arc::new(FakeHttpClient::default());
    update_http.with_text(
        &github_releases_url("adobe", "source-code-pro"),
        github_release_json(
            "v2.0.0",
            "source-code-pro.zip",
            "https://downloads.example/source-code-pro-v2.zip",
        ),
    );
    update_http.with_download_bytes(
        "https://downloads.example/source-code-pro-v2.zip",
        zip_with_fixture_font("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf"),
    );
    let app = FontbrewApp::with_paths_and_http_client(paths.clone(), update_http);
    let mut progress = NoProgress;
    let plan = app
        .update_plan(
            update_request(vec![package_id("source-code-pro")], Some(1)),
            &mut progress,
            &NeverCancelled,
        )
        .expect("plan update");

    let report = app
        .apply_update(
            plan,
            ExecutionPolicy::SafeOnly,
            &mut progress,
            &NeverCancelled,
        )
        .expect("apply update");

    assert_eq!(report.updated.len(), 1);
    let manifest = ManifestStore::read_or_empty(&paths.manifest_path()).expect("read manifest");
    let record = manifest
        .get_package(&package_id("source-code-pro"))
        .expect("manifest record");
    assert_eq!(record.version.as_str(), "v2.0.0");
    let new_store_path = paths
        .package_store_dir(
            &package_id("source-code-pro"),
            &PackageVersion::new("v2.0.0"),
        )
        .join("files/SourceCodePro-Regular.ttf");
    assert_eq!(
        fs::read_link(paths.activation_dir().join("SourceCodePro-Regular.ttf"))
            .expect("new activation symlink"),
        new_store_path
    );
    assert!(!old_store_dir.exists());
}

#[test]
fn update_dry_run_does_not_mutate_manifest_activation_or_package_store() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    install_github_source_code_pro(&paths, "v1.0.0");
    let old_store_path = paths
        .package_store_dir(
            &package_id("source-code-pro"),
            &PackageVersion::new("v1.0.0"),
        )
        .join("files/SourceCodePro-Regular.ttf");
    let old_activation_path = paths.activation_dir().join("SourceCodePro-Regular.ttf");

    let update_http = Arc::new(FakeHttpClient::default());
    update_http.with_text(
        &github_releases_url("adobe", "source-code-pro"),
        github_release_json(
            "v2.0.0",
            "source-code-pro.zip",
            "https://downloads.example/source-code-pro-v2.zip",
        ),
    );
    update_http.with_download_bytes(
        "https://downloads.example/source-code-pro-v2.zip",
        zip_with_fixture_font("SourceCodePro-Regular.ttf", "SourceCodePro-Regular.ttf"),
    );
    let app = FontbrewApp::with_paths_and_http_client(paths.clone(), update_http);
    let mut progress = NoProgress;
    let plan = app
        .update_plan(
            update_request(vec![package_id("source-code-pro")], Some(1)),
            &mut progress,
            &NeverCancelled,
        )
        .expect("plan update");

    let report = app
        .apply_update(
            plan,
            ExecutionPolicy::DryRun,
            &mut progress,
            &NeverCancelled,
        )
        .expect("dry-run update");

    assert!(report.updated.is_empty());
    assert_eq!(report.planned.len(), 1);
    assert_eq!(
        ManifestStore::read_or_empty(&paths.manifest_path())
            .expect("read manifest")
            .get_package(&package_id("source-code-pro"))
            .expect("manifest record")
            .version
            .as_str(),
        "v1.0.0"
    );
    assert!(old_store_path.exists());
    assert!(!paths
        .package_store_dir(
            &package_id("source-code-pro"),
            &PackageVersion::new("v2.0.0")
        )
        .exists());
    assert_eq!(
        fs::read_link(&old_activation_path).expect("old activation symlink"),
        old_store_path
    );
}

fn first_staged_font_file(staging_dir: &Path) -> PathBuf {
    let mut stack = vec![staging_dir.to_path_buf()];

    while let Some(path) = stack.pop() {
        for entry in fs::read_dir(path).expect("read staging directory") {
            let entry = entry.expect("read staging entry");
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }

            if path.extension().is_some_and(|extension| extension == "ttf") {
                return path;
            }
        }
    }

    panic!("staged font file should exist");
}
