use std::{fmt, sync::Arc};

use crate::error::{FontbrewError, Result};
use crate::fetch::{HttpClient, ReqwestHttpClient};
use crate::install;
use crate::model::{
    CancellationToken, ExecutionPolicy, InfoReport, InfoRequest, InstallPlan, InstallReport,
    InstallRequest, ListReport, OutdatedReport, OutdatedRequest, ProgressSink,
    RegistryStatusReport, RegistryUpdateReport, RemovePlan, RemoveReport, RemoveRequest,
    SearchReport, SearchRequest, UpdatePlan, UpdateReport, UpdateRequest,
};
use crate::platform::FontbrewPaths;
use crate::registry::{registry_url_from_env, RegistrySnapshotStore, ReqwestRegistryHttpClient};

#[derive(Clone)]
pub struct FontbrewApp {
    paths: Option<FontbrewPaths>,
    http_client: Option<Arc<dyn HttpClient>>,
}

impl FontbrewApp {
    pub fn new() -> Self {
        Self {
            paths: None,
            http_client: None,
        }
    }

    pub fn with_paths(paths: FontbrewPaths) -> Self {
        Self {
            paths: Some(paths),
            http_client: None,
        }
    }

    pub fn with_paths_and_http_client(
        paths: FontbrewPaths,
        http_client: Arc<dyn HttpClient>,
    ) -> Self {
        Self {
            paths: Some(paths),
            http_client: Some(http_client),
        }
    }

    pub fn install_plan(&self, request: InstallRequest) -> Result<InstallPlan> {
        match request.source.clone() {
            crate::InstallSource::LocalPath(_) => install::install_plan(&self.paths()?, request),
            crate::InstallSource::RegistryName(short_name) => {
                let recipe =
                    RegistrySnapshotStore::new(self.paths()?).resolve_short_name(&short_name)?;
                install::registry_recipe_install_plan(
                    &self.paths()?,
                    recipe,
                    request,
                    self.http_client()?.as_ref(),
                )
            }
            crate::InstallSource::GitHubRepo { owner, repo } => {
                let github_repo = crate::sources::GitHubRepo::parse(format!("{owner}/{repo}"))?;
                install::github_repo_install_plan(
                    &self.paths()?,
                    github_repo,
                    request,
                    self.http_client()?.as_ref(),
                )
            }
            crate::InstallSource::Provider { .. } => not_implemented("install_plan"),
        }
    }

    pub fn apply_install(
        &self,
        plan: InstallPlan,
        policy: ExecutionPolicy,
        progress: &mut dyn ProgressSink,
        cancellation: &dyn CancellationToken,
    ) -> Result<InstallReport> {
        install::apply_install(&self.paths()?, plan, policy, progress, cancellation)
    }

    pub fn list_packages(&self) -> Result<ListReport> {
        install::list_packages(&self.paths()?)
    }

    pub fn package_info(&self, request: InfoRequest) -> Result<InfoReport> {
        install::package_info(&self.paths()?, request)
    }

    pub fn remove_plan(&self, request: RemoveRequest) -> Result<RemovePlan> {
        install::remove_plan(&self.paths()?, request)
    }

    pub fn apply_remove(
        &self,
        plan: RemovePlan,
        policy: ExecutionPolicy,
        progress: &mut dyn ProgressSink,
        cancellation: &dyn CancellationToken,
    ) -> Result<RemoveReport> {
        install::apply_remove(&self.paths()?, plan, policy, progress, cancellation)
    }

    pub fn outdated(&self, _request: OutdatedRequest) -> Result<OutdatedReport> {
        not_implemented("outdated")
    }

    pub fn update_plan(
        &self,
        _request: UpdateRequest,
        _progress: &mut dyn ProgressSink,
        _cancellation: &dyn CancellationToken,
    ) -> Result<UpdatePlan> {
        not_implemented("update_plan")
    }

    pub fn apply_update(
        &self,
        _plan: UpdatePlan,
        _policy: ExecutionPolicy,
        _progress: &mut dyn ProgressSink,
        _cancellation: &dyn CancellationToken,
    ) -> Result<UpdateReport> {
        not_implemented("apply_update")
    }

    pub fn search(&self, _request: SearchRequest) -> Result<SearchReport> {
        not_implemented("search")
    }

    pub fn registry_update(&self) -> Result<RegistryUpdateReport> {
        let paths = self.paths()?;
        let store = RegistrySnapshotStore::new(paths);
        let client = ReqwestRegistryHttpClient::default();
        let registry_url = registry_url_from_env();

        store.update_from_client(&client, &registry_url)
    }

    pub fn registry_status(&self) -> Result<RegistryStatusReport> {
        RegistrySnapshotStore::new(self.paths()?).status()
    }

    fn paths(&self) -> Result<FontbrewPaths> {
        match &self.paths {
            Some(paths) => Ok(paths.clone()),
            None => FontbrewPaths::resolve(),
        }
    }

    fn http_client(&self) -> Result<Arc<dyn HttpClient>> {
        if let Some(http_client) = &self.http_client {
            return Ok(http_client.clone());
        }

        Ok(Arc::new(ReqwestHttpClient::try_new()?))
    }
}

fn not_implemented<T>(operation: &'static str) -> Result<T> {
    Err(FontbrewError::NotImplemented { operation })
}

impl Default for FontbrewApp {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for FontbrewApp {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FontbrewApp")
            .field("paths", &self.paths)
            .field(
                "http_client",
                &self.http_client.as_ref().map(|_| "<http-client>"),
            )
            .finish()
    }
}
