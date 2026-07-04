//! Reusable application core for Fontbrew-owned frontends.

pub mod activation;
pub mod app;
pub mod archives;
pub mod config;
pub mod error;
pub mod fetch;
pub mod fonts;
pub mod fs;
mod github;
mod install;
pub mod manifest;
pub mod model;
pub mod platform;
mod providers;
pub mod registry;
pub mod sources;
pub mod tasks;
mod update;
pub mod version;

pub use app::FontbrewApp;
pub use error::{FontbrewError, Result};
pub use model::*;
pub use version::*;

#[cfg(test)]
mod tests {
    use std::{
        ffi::OsString,
        path::PathBuf,
        sync::{Mutex, MutexGuard},
    };

    use crate::{
        config::GOOGLE_FONTS_API_KEY_ENV_VAR, platform::FontbrewPaths, FamilyName, FontFormat,
        FontbrewApp, FontbrewError, InfoReport, InfoRequest, InstallPlan, InstallRequest,
        InstallSource, PackageId, PackageInfo, PackageVersion, PlannedChange, ProviderKind,
    };

    fn package_id(id: &str) -> PackageId {
        PackageId::parse(id).expect("test package id should be valid")
    }

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvVarGuard {
        key: &'static str,
        original: Option<OsString>,
        _guard: MutexGuard<'static, ()>,
    }

    impl EnvVarGuard {
        fn unset_google_fonts_api_key() -> Self {
            let guard = ENV_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let original = std::env::var_os(GOOGLE_FONTS_API_KEY_ENV_VAR);
            std::env::remove_var(GOOGLE_FONTS_API_KEY_ENV_VAR);

            Self {
                key: GOOGLE_FONTS_API_KEY_ENV_VAR,
                original,
                _guard: guard,
            }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.original {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
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
        let temp = tempfile::tempdir().expect("tempdir");
        let app = FontbrewApp::with_paths(FontbrewPaths::for_tests(
            temp.path().join("data"),
            temp.path().join("config"),
            temp.path().join("home"),
        ));
        let request = InfoRequest {
            package_id: package_id("jetbrains-mono"),
        };

        let result: crate::Result<InfoReport> = app.package_info(request);
        let error = result.expect_err("missing package should fail");

        assert!(matches!(error, FontbrewError::Manifest { .. }));
    }

    #[test]
    fn google_install_without_api_key_returns_actionable_config_error() {
        let _env = EnvVarGuard::unset_google_fonts_api_key();
        let temp = tempfile::tempdir().expect("tempdir");
        let app = FontbrewApp::with_paths(FontbrewPaths::for_tests(
            temp.path().join("data"),
            temp.path().join("config"),
            temp.path().join("home"),
        ));
        let request = InstallRequest {
            source: InstallSource::Provider {
                provider: ProviderKind::Google,
                id: "inter".to_string(),
            },
            format_preference: Vec::new(),
            asset_selector: None,
            reinstall: false,
            refresh: false,
            offline: false,
        };

        let error = app
            .install_plan(request)
            .expect_err("missing Google Fonts API key should fail");

        assert!(matches!(error, FontbrewError::Config { .. }));
        assert!(error.to_string().contains("GOOGLE_FONTS_API_KEY"));
    }
}
