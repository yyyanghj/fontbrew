//! Reusable application core for Fontbrew-owned frontends.

pub mod app;
pub mod archives;
pub mod config;
pub mod error;
pub mod fonts;
pub mod fs;
pub mod model;
pub mod platform;
pub mod version;

pub use app::FontbrewApp;
pub use error::{FontbrewError, Result};
pub use model::*;
pub use version::*;

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::{
        FamilyName, FontFormat, FontbrewApp, FontbrewError, InfoReport, InfoRequest, InstallPlan,
        InstallRequest, InstallSource, PackageId, PackageInfo, PackageVersion, PlannedChange,
        ProviderKind,
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
                provider: ProviderKind::Google,
                id: "Inter".to_string(),
            },
            format_preference: vec![FontFormat::Otf, FontFormat::Ttf],
            asset_selector: Some("*desktop*".to_string()),
            reinstall: true,
            refresh: true,
            offline: false,
        };

        let json = serde_json::to_value(&request).expect("install request should serialize");

        assert_eq!(json["source"]["Provider"]["provider"], "Google");
        assert_eq!(json["source"]["Provider"]["id"], "Inter");
        assert_eq!(json["format_preference"][0], "Otf");
        assert_eq!(json["asset_selector"], "*desktop*");
        assert_eq!(json["reinstall"], true);
        assert_eq!(json["refresh"], true);
        assert_eq!(json["offline"], false);

        let local_request = InstallRequest {
            source: InstallSource::LocalPath(PathBuf::from("/tmp/fonts.zip")),
            format_preference: Vec::new(),
            asset_selector: None,
            reinstall: false,
            refresh: false,
            offline: true,
        };

        let local_json =
            serde_json::to_value(&local_request).expect("local install request should serialize");

        assert_eq!(local_json["source"]["LocalPath"], "/tmp/fonts.zip");
    }

    #[test]
    fn info_report_serializes_as_a_frontend_report_shell() {
        let report = InfoReport {
            package: PackageInfo {
                package_id: package_id("jetbrains-mono"),
                version: PackageVersion::new("2.304"),
                families: vec![FamilyName::new("JetBrains Mono")],
                source: "registry:jetbrains-mono".to_string(),
                activated: true,
                update_source: Some("github:JetBrains/JetBrainsMono".to_string()),
            },
        };

        let json = serde_json::to_value(&report).expect("info report should serialize");

        assert_eq!(json["package"]["package_id"], "jetbrains-mono");
        assert_eq!(json["package"]["families"][0], "JetBrains Mono");
        assert_eq!(json["package"]["activated"], true);
    }

    #[test]
    fn package_info_returns_an_info_report_shell() {
        let app = FontbrewApp::new();
        let request = InfoRequest {
            package_id: package_id("jetbrains-mono"),
        };

        let result: crate::Result<InfoReport> = app.package_info(request);
        let error = result.expect_err("stub should fail");

        assert!(matches!(
            error,
            FontbrewError::NotImplemented {
                operation: "package_info"
            }
        ));
    }

    #[test]
    fn app_methods_return_structured_not_implemented_errors() {
        let app = FontbrewApp::new();
        let request = InstallRequest {
            source: InstallSource::RegistryName("jetbrains-mono".to_string()),
            format_preference: Vec::new(),
            asset_selector: None,
            reinstall: false,
            refresh: false,
            offline: false,
        };

        let error = app.install_plan(request).expect_err("stub should fail");

        assert!(matches!(
            error,
            FontbrewError::NotImplemented {
                operation: "install_plan"
            }
        ));
    }
}
