//! Reusable application core for Fontbrew-owned frontends.

pub mod activation;
pub mod archives;
pub mod config;
pub mod error;
pub mod fetch;
pub mod fontbrew;
pub mod fonts;
pub mod fs;
mod install;
pub mod manifest;
pub mod model;
pub mod platform;
mod providers;
mod search;
pub mod sources;
pub mod tasks;
mod update;
pub mod version;

pub use error::{FontbrewError, Result};
pub use fontbrew::{
    ExtractArchiveRequest, ExtractedArchive, FetchInstallMetadataRequest, FontFileInput, Fontbrew,
    FontbrewOptions, InstallMetadata, InstallPlanSet, InstallPreparation, InstallSourcePreparation,
    InstallTarget, ParseFontsRequest, ParsedFontFaceInfo, ParsedFontFileInfo, ParsedFonts,
    PendingAssetSelection, PendingFamilySelection, PlanInstallRequest, PrepareInstallAssetRequest,
    PrepareInstallSourceRequest,
};
pub use model::*;
pub use version::*;

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::{
        platform::FontbrewPaths, FamilyName, FontFormat, Fontbrew, FontbrewError, FontbrewOptions,
        InfoReport, InfoRequest, InstallPlan, InstallRequest, InstallSource, PackageId,
        PackageInfo, PackageVersion, PlannedChange, ProviderKind,
    };

    fn package_id(id: &str) -> PackageId {
        PackageId::parse(id).expect("test package id should be valid")
    }

    #[test]
    fn report_shells_serialize_for_frontends() {
        let plan = InstallPlan {
            package_id: package_id("jetbrains-mono"),
            target_version: Some(PackageVersion::new("2.304")),
            changes: vec![PlannedChange {
                package_id: package_id("jetbrains-mono"),
                description: "install managed package".to_string(),
            }],
            risks: Vec::new(),
            already_installed: false,
            prepared: None,
        };

        let json = serde_json::to_value(&plan).expect("install plan should serialize");

        assert_eq!(json["package_id"], "jetbrains-mono");
        assert_eq!(json["target_version"], "2.304");
        assert_eq!(json["changes"][0]["description"], "install managed package");
    }

    #[test]
    fn install_requests_serialize_from_sources_for_frontends() {
        let request = InstallRequest {
            source: InstallSource::Provider {
                provider: ProviderKind::Fontsource,
                id: "inter".to_string(),
            },
            package_id_override: None,
            format_preference: vec![FontFormat::Otf, FontFormat::Ttf],
            asset_selector: Some("*desktop*".to_string()),
            selected_families: Vec::new(),
            reinstall: true,
        };

        let json = serde_json::to_value(&request).expect("install request should serialize");

        assert_eq!(json["source"]["Provider"]["provider"], "Fontsource");
        assert_eq!(json["source"]["Provider"]["id"], "inter");
        assert_eq!(json["format_preference"][0], "Otf");
        assert_eq!(json["asset_selector"], "*desktop*");
        assert_eq!(json["reinstall"], true);
        assert!(json.get("package_id_override").is_none());
        assert!(json.get("refresh").is_none());
        assert!(json.get("offline").is_none());

        let local_request = InstallRequest {
            source: InstallSource::LocalPath(PathBuf::from("/tmp/fonts.zip")),
            package_id_override: Some(package_id("custom-local")),
            format_preference: Vec::new(),
            asset_selector: None,
            selected_families: Vec::new(),
            reinstall: false,
        };

        let local_json =
            serde_json::to_value(&local_request).expect("local install request should serialize");

        assert_eq!(local_json["source"]["LocalPath"], "/tmp/fonts.zip");
        assert_eq!(local_json["package_id_override"], "custom-local");
    }

    #[test]
    fn info_report_serializes_as_a_frontend_report_shell() {
        let report = InfoReport {
            package: PackageInfo {
                package_id: package_id("jetbrains-mono"),
                version: PackageVersion::new("2.304"),
                families: vec![FamilyName::new("JetBrains Mono")],
                source: "fontsource:jetbrains-mono".to_string(),
                activated: true,
                update_source: Some("github:JetBrains/JetBrainsMono".to_string()),
                managed: true,
                update_available: None,
                font_files: Vec::new(),
                activation_artifacts: Vec::new(),
            },
        };

        let json = serde_json::to_value(&report).expect("info report should serialize");

        assert_eq!(json["package"]["package_id"], "jetbrains-mono");
        assert_eq!(json["package"]["families"][0], "JetBrains Mono");
        assert_eq!(json["package"]["activated"], true);
        assert_eq!(json["package"]["managed"], true);
        assert_eq!(json["package"]["update_available"], serde_json::Value::Null);
        assert!(json["package"]["font_files"].is_array());
        assert!(json["package"]["activation_artifacts"].is_array());
    }

    #[tokio::test]
    async fn package_info_returns_an_info_report_shell() {
        let temp = tempfile::tempdir().expect("tempdir");
        let paths = FontbrewPaths::for_tests(
            temp.path().join("data"),
            temp.path().join("config"),
            temp.path().join("home"),
        );
        let fontbrew = Fontbrew::new(FontbrewOptions {
            store_dir: Some(paths.managed_store_dir()),
            config_path: Some(paths.config_path()),
            activation_dir: Some(paths.activation_dir()),
        })
        .expect("create Fontbrew");
        let request = InfoRequest {
            package_id: package_id("jetbrains-mono"),
        };

        let result: crate::Result<InfoReport> = fontbrew.package_info(request).await;
        let error = result.expect_err("missing package should fail");

        assert!(matches!(error, FontbrewError::Manifest { .. }));
    }
}
