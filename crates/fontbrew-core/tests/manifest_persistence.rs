use std::{collections::BTreeMap, fs, path::PathBuf};

use fontbrew_core::{
    config::ActivationStrategy,
    manifest::{
        ManifestActivationArtifactRecord, ManifestFontFileFormat, ManifestPackageRecord,
        ManifestSource, ManifestStore, ManifestV1,
    },
    FamilyName, FontbrewError, PackageId, PackageVersion, ProviderKind,
};

fn package_id(id: &str) -> PackageId {
    PackageId::parse(id).expect("test package id should be valid")
}

fn package_record(id: &str, version: &str) -> ManifestPackageRecord {
    ManifestPackageRecord {
        package_id: package_id(id),
        version: PackageVersion::new(version),
        source: ManifestSource::GitHub {
            owner: "font-owner".to_string(),
            repo: "font-repo".to_string(),
        },
        update_source: Some(ManifestSource::GitHub {
            owner: "font-owner".to_string(),
            repo: "font-repo".to_string(),
        }),
        families: vec![FamilyName::new("Inter")],
        font_files: vec![fontbrew_core::manifest::ManifestFontFileRecord {
            path: PathBuf::from("packages/inter/fonts/Inter-Regular.ttf"),
            family: FamilyName::new("Inter"),
            style: "Regular".to_string(),
            weight: 400,
            format: ManifestFontFileFormat::Ttf,
        }],
        activation_artifacts: vec![ManifestActivationArtifactRecord {
            path: PathBuf::from("activated/Inter-Regular.ttf"),
            source_path: PathBuf::from("packages/inter/fonts/Inter-Regular.ttf"),
            strategy: ActivationStrategy::Symlink,
        }],
        installed_at: "2026-07-04T10:20:30Z".to_string(),
        active_version: Some(PackageVersion::new(version)),
    }
}

#[tokio::test]
async fn empty_manifest_serializes_schema_version_and_packages() {
    let manifest = ManifestV1::empty();

    let json = serde_json::to_value(&manifest).expect("manifest should serialize");

    assert_eq!(json["schemaVersion"], 1);
    assert!(json["packages"]
        .as_object()
        .expect("packages is keyed object")
        .is_empty());
}

#[tokio::test]
async fn manifest_packages_serialize_as_keyed_object_by_package_id() {
    let mut manifest = ManifestV1::empty();
    manifest
        .insert_package(package_record("inter", "1.0.0"))
        .expect("insert package");

    let json = serde_json::to_value(&manifest).expect("manifest should serialize");

    assert!(json["packages"]
        .as_object()
        .expect("packages object")
        .contains_key("inter"));
    assert_eq!(json["packages"]["inter"]["packageId"], "inter");
    assert_eq!(json["packages"]["inter"]["version"], "1.0.0");
}

#[tokio::test]
async fn manifest_store_reads_and_writes_manifest_v1() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let path = temp_dir.path().join("manifest.json");
    let mut manifest = ManifestV1::empty();
    manifest
        .insert_package(package_record("inter", "1.0.0"))
        .expect("insert package");

    ManifestStore::write(&path, &manifest).expect("write manifest");
    let read = ManifestStore::read_or_empty(&path).expect("read manifest");

    let package = read
        .get_package(&package_id("inter"))
        .expect("package should round-trip");
    assert_eq!(package.version.as_str(), "1.0.0");
    assert_eq!(package.families, vec![FamilyName::new("Inter")]);
    assert_eq!(
        package.font_files[0].path,
        PathBuf::from("packages/inter/fonts/Inter-Regular.ttf")
    );
    assert_eq!(
        package.update_source,
        Some(ManifestSource::GitHub {
            owner: "font-owner".to_string(),
            repo: "font-repo".to_string()
        })
    );
    assert_eq!(
        package.activation_artifacts[0].source_path,
        PathBuf::from("packages/inter/fonts/Inter-Regular.ttf")
    );
}

#[tokio::test]
async fn manifest_store_returns_empty_manifest_when_file_is_missing() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let path = temp_dir.path().join("missing-manifest.json");

    let manifest = ManifestStore::read_or_empty(&path).expect("missing manifest should be empty");

    assert!(manifest.packages.is_empty());
}

#[tokio::test]
async fn manifest_insert_replace_remove_and_lookup_package_records() {
    let mut manifest = ManifestV1::empty();
    let id = package_id("inter");

    assert!(manifest.get_package(&id).is_none());
    assert!(manifest
        .insert_package(package_record("inter", "1.0.0"))
        .is_ok());
    assert_eq!(
        manifest.get_package(&id).expect("inserted package").version,
        PackageVersion::new("1.0.0")
    );

    assert!(manifest
        .insert_package(package_record("inter", "1.1.0"))
        .is_ok());
    assert_eq!(
        manifest.get_package(&id).expect("replaced package").version,
        PackageVersion::new("1.1.0")
    );

    let removed = manifest.remove_package(&id).expect("package should remove");

    assert_eq!(removed.version, PackageVersion::new("1.1.0"));
    assert!(manifest.get_package(&id).is_none());
}

#[tokio::test]
async fn manifest_source_shapes_are_explicit_and_serializable() {
    let sources = vec![
        ManifestSource::GitHub {
            owner: "rsms".to_string(),
            repo: "inter".to_string(),
        },
        ManifestSource::Provider {
            provider: ProviderKind::Fontsource,
            id: "inter".to_string(),
        },
        ManifestSource::LocalArchive {
            path: PathBuf::from("/tmp/inter.zip"),
        },
    ];

    let json = serde_json::to_value(&sources).expect("sources should serialize");
    let round_trip: Vec<ManifestSource> =
        serde_json::from_value(json).expect("sources should deserialize");

    assert_eq!(round_trip, sources);
}

#[tokio::test]
async fn manifest_source_json_shape_is_stable() {
    let record = package_record("inter", "1.0.0");

    let json = serde_json::to_value(&record).expect("record should serialize");

    assert_eq!(json["source"]["GitHub"]["owner"], "font-owner");
    assert_eq!(json["source"]["GitHub"]["repo"], "font-repo");
    assert_eq!(json["updateSource"]["GitHub"]["owner"], "font-owner");
    assert_eq!(json["updateSource"]["GitHub"]["repo"], "font-repo");
    assert_eq!(
        json["activationArtifacts"][0]["path"],
        "activated/Inter-Regular.ttf"
    );
    assert_eq!(
        json["activationArtifacts"][0]["sourcePath"],
        "packages/inter/fonts/Inter-Regular.ttf"
    );
    assert_eq!(json["activationArtifacts"][0]["strategy"], "Symlink");
}

#[tokio::test]
async fn manifest_store_rejects_missing_schema_version() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let path = temp_dir.path().join("manifest.json");
    fs::write(&path, r#"{"packages":[]}"#).expect("write invalid manifest");

    let error = ManifestStore::read_or_empty(&path).expect_err("schemaVersion is required");

    assert!(matches!(
        error,
        FontbrewError::ManifestSchema {
            found: None,
            supported: 1
        }
    ));
}

#[tokio::test]
async fn manifest_store_rejects_newer_schema_version() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let path = temp_dir.path().join("manifest.json");
    fs::write(&path, r#"{"schemaVersion":2,"packages":[]}"#).expect("write invalid manifest");

    let error = ManifestStore::read_or_empty(&path).expect_err("newer schema should reject");

    assert!(matches!(
        error,
        FontbrewError::ManifestSchema {
            found: Some(2),
            supported: 1
        }
    ));
}

#[tokio::test]
async fn manifest_store_rejects_package_key_record_mismatch_on_read() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let path = temp_dir.path().join("manifest.json");
    fs::write(
        &path,
        r#"{
            "schemaVersion": 1,
            "packages": {
                "inter": {
                    "packageId": "jetbrains-mono",
                    "version": "1.0.0",
                    "source": { "GitHub": { "owner": "rsms", "repo": "inter" } },
                    "updateSource": null,
                    "families": [],
                    "fontFiles": [],
                    "activationArtifacts": [],
                    "installedAt": "2026-07-04T10:20:30Z",
                    "activeVersion": null
                }
            }
        }"#,
    )
    .expect("write invalid manifest");

    let error = ManifestStore::read_or_empty(&path).expect_err("mismatch should reject");

    assert!(matches!(error, FontbrewError::Manifest { .. }));
    assert!(error.to_string().contains("package key"));
}

#[tokio::test]
async fn manifest_store_rejects_invalid_package_map_key() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let path = temp_dir.path().join("manifest.json");
    fs::write(
        &path,
        r#"{
            "schemaVersion": 1,
            "packages": {
                "../bad": {
                    "packageId": "inter",
                    "version": "1.0.0",
                    "source": { "GitHub": { "owner": "rsms", "repo": "inter" } },
                    "updateSource": null,
                    "families": [],
                    "fontFiles": [],
                    "activationArtifacts": [],
                    "installedAt": "2026-07-04T10:20:30Z",
                    "activeVersion": null
                }
            }
        }"#,
    )
    .expect("write invalid manifest");

    let error = ManifestStore::read_or_empty(&path).expect_err("invalid key should reject");

    assert!(matches!(error, FontbrewError::Manifest { .. }));
    assert!(error.to_string().contains("invalid package id"));
}

#[tokio::test]
async fn manifest_store_rejects_package_key_record_mismatch_on_write() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let path = temp_dir.path().join("manifest.json");
    let mut packages = BTreeMap::new();
    packages.insert(
        package_id("inter"),
        package_record("jetbrains-mono", "2.304"),
    );
    let manifest = ManifestV1 {
        schema_version: 1,
        packages,
    };

    let error = ManifestStore::write(&path, &manifest).expect_err("mismatch should reject");

    assert!(matches!(error, FontbrewError::Manifest { .. }));
    assert!(error.to_string().contains("package key"));
}

#[tokio::test]
async fn manifest_writes_replace_final_file_without_partial_content() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let path = temp_dir.path().join("manifest.json");
    let mut old_manifest = ManifestV1::empty();
    old_manifest
        .insert_package(package_record("inter", "1.0.0"))
        .expect("insert old package");
    ManifestStore::write(&path, &old_manifest).expect("write old manifest");

    let mut new_manifest = ManifestV1::empty();
    new_manifest
        .insert_package(package_record("jetbrains-mono", "2.304"))
        .expect("insert new package");
    ManifestStore::write(&path, &new_manifest).expect("write replacement manifest");

    let final_content = fs::read_to_string(&path).expect("final manifest exists");
    assert!(final_content.contains("jetbrains-mono"));
    assert!(!final_content.contains(r#""packageId": "inter""#));
    assert!(serde_json::from_str::<ManifestV1>(&final_content).is_ok());
}
