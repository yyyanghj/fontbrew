use std::{path::PathBuf, sync::Arc};

use fontbrew_core::{
    manifest::{ManifestPackageRecord, ManifestSource, ManifestStore, ManifestV1},
    platform::FontbrewPaths,
    FamilyName, Fontbrew, FontbrewError, FontbrewOptions, OutdatedRequest, PackageId,
    PackageVersion, SearchRequest,
};

mod support;

use support::LocalHttpServer;

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

fn package_id(id: &str) -> PackageId {
    PackageId::parse(id).expect("test package id should be valid")
}

fn github_releases_path(owner: &str, repo: &str) -> String {
    format!("/repos/{owner}/{repo}/releases")
}

fn fontsource_list_path() -> String {
    "/fonts".to_string()
}

fn fontsource_detail_path(id: &str) -> String {
    format!("/fonts/{id}")
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

#[tokio::test]
async fn unprefixed_search_fetches_fontsource_results() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let server = LocalHttpServer::start();
    server.respond_text(
        &fontsource_list_path(),
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
    server.respond_text(
        &fontsource_detail_path("inter"),
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
    let app =
        fontbrew_with_paths(paths.clone()).with_network_client(Arc::new(server.network_client()));

    let report = app
        .search(SearchRequest {
            query: "iner".to_string(),
            limit: Some(1),
        })
        .await
        .expect("search should fetch Fontsource metadata");

    assert_eq!(report.len(), 1);
    assert_eq!(report[0].package_id, package_id("inter"));
    assert_eq!(report[0].source, "fontsource:inter");
    assert_eq!(
        server.request_urls(),
        vec![
            server.url(&fontsource_list_path()),
            server.url(&fontsource_detail_path("inter"))
        ]
    );
}

#[tokio::test]
async fn search_rejects_zero_limit_without_network_requests() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = test_paths(&temp);
    let server = LocalHttpServer::start();
    let app = fontbrew_with_paths(paths).with_network_client(Arc::new(server.network_client()));

    let error = app
        .search(SearchRequest {
            query: "inter".to_string(),
            limit: Some(0),
        })
        .await
        .expect_err("zero search limit should be rejected");

    assert!(matches!(error, FontbrewError::Config { .. }));
    assert!(error.to_string().contains("greater than 0"));
    assert!(server.request_urls().is_empty());
}

#[tokio::test]
async fn outdated_reports_newer_github_releases_and_local_packages_without_update_sources() {
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
    let server = LocalHttpServer::start();
    server.respond_text(
        &github_releases_path("adobe", "source-code-pro"),
        r#"[{"tag_name":"v1.2.0","draft":false,"prerelease":false,"assets":[]}]"#,
    );
    server.respond_text(
        &github_releases_path("rsms", "inter"),
        r#"[{"tag_name":"v4.1.0","draft":false,"prerelease":false,"assets":[]}]"#,
    );
    server.respond_text(
        &github_releases_path("owner", "up-to-date"),
        r#"[{"tag_name":"v2.0.0","draft":false,"prerelease":false,"assets":[]}]"#,
    );
    let app = fontbrew_with_paths(paths).with_network_client(Arc::new(server.network_client()));

    let report = app
        .outdated(OutdatedRequest {
            package_ids: Vec::new(),
        })
        .await
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
        server.request_urls(),
        vec![
            server.url(&github_releases_path("rsms", "inter")),
            server.url(&github_releases_path("adobe", "source-code-pro")),
            server.url(&github_releases_path("owner", "up-to-date")),
        ]
    );
}
