use std::{fmt, sync::Arc};

use crate::config::FontbrewConfig;
use crate::error::Result;
use crate::fetch::{HttpClient, ReqwestHttpClient};
use crate::fs::GlobalFileLock;
use crate::install;
use crate::model::{
    ensure_not_cancelled, CancellationToken, ConfigGetRequest, ConfigReport, ConfigSetRequest,
    ExecutionPolicy, InfoReport, InfoRequest, InstallPlan, InstallReport, InstallRequest,
    ListReport, NoCancellation, NoProgress, OutdatedReport, OutdatedRequest, ProgressSink,
    RemovePlan, RemoveReport, RemoveRequest, SearchReport, SearchRequest, UpdatePlan, UpdateReport,
    UpdateRequest,
};
use crate::platform::FontbrewPaths;
use crate::providers::{FontsourceProvider, ProviderSearchRequest};
use crate::sources::ProviderSource;
use crate::update;

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
        self.install_plan_with_cancellation(request, &NoCancellation)
    }

    pub fn install_plan_with_cancellation(
        &self,
        request: InstallRequest,
        cancellation: &dyn CancellationToken,
    ) -> Result<InstallPlan> {
        let mut progress = NoProgress;
        self.install_plan_with_progress_and_cancellation(request, &mut progress, cancellation)
    }

    pub fn install_plan_with_progress_and_cancellation(
        &self,
        request: InstallRequest,
        progress: &mut dyn ProgressSink,
        cancellation: &dyn CancellationToken,
    ) -> Result<InstallPlan> {
        ensure_not_cancelled(cancellation)?;
        let paths = self.paths()?;
        install::ensure_package_id_override_allowed_for_source(&request)?;
        match request.source.clone() {
            crate::InstallSource::LocalPath(_) => {
                install::install_plan_with_progress(&paths, request, progress, cancellation)
            }
            crate::InstallSource::GitHubRepo { owner, repo } => {
                let github_repo = crate::sources::GitHubRepo::parse(format!("{owner}/{repo}"))?;
                install::github_repo_install_plan(
                    &paths,
                    github_repo,
                    request,
                    progress,
                    self.http_client()?.as_ref(),
                    cancellation,
                )
            }
            crate::InstallSource::Provider {
                provider: crate::ProviderKind::Fontsource,
                id,
            } => install::fontsource_install_plan(
                &paths,
                id,
                request,
                progress,
                self.http_client()?.as_ref(),
                cancellation,
            ),
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

    pub fn discard_install_plan(&self, plan: InstallPlan) {
        install::discard_install_plan(plan);
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

    pub fn remove_plan_with_cancellation(
        &self,
        request: RemoveRequest,
        cancellation: &dyn CancellationToken,
    ) -> Result<RemovePlan> {
        install::remove_plan_with_cancellation(&self.paths()?, request, cancellation)
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

    pub fn outdated(&self, request: OutdatedRequest) -> Result<OutdatedReport> {
        update::outdated(&self.paths()?, request, self.http_client()?.as_ref())
    }

    pub fn update_plan(
        &self,
        request: UpdateRequest,
        progress: &mut dyn ProgressSink,
        cancellation: &dyn CancellationToken,
    ) -> Result<UpdatePlan> {
        update::update_plan(
            &self.paths()?,
            request,
            self.http_client()?.as_ref(),
            progress,
            cancellation,
        )
    }

    pub fn apply_update(
        &self,
        plan: UpdatePlan,
        policy: ExecutionPolicy,
        progress: &mut dyn ProgressSink,
        cancellation: &dyn CancellationToken,
    ) -> Result<UpdateReport> {
        update::apply_update(&self.paths()?, plan, policy, progress, cancellation)
    }

    pub fn discard_update_plan(&self, plan: UpdatePlan) {
        update::discard_update_plan(plan);
    }

    pub fn search(&self, request: SearchRequest) -> Result<SearchReport> {
        let paths = self.paths()?;
        if let Some(provider_source) = ProviderSource::parse_prefixed(&request.query) {
            let results = self.search_provider_source(&paths, provider_source, &request)?;
            return Ok(SearchReport { results });
        }

        let http_client = self.http_client()?;
        let results = FontsourceProvider::new(&paths, http_client.as_ref()).search(
            ProviderSearchRequest {
                query: &request.query,
                limit: request.limit,
            },
        )?;

        Ok(SearchReport { results })
    }

    pub fn config_get(&self, request: ConfigGetRequest) -> Result<ConfigReport> {
        FontbrewConfig::get(&self.paths()?.config_path(), request)
    }

    pub fn config_set(&self, request: ConfigSetRequest) -> Result<ConfigReport> {
        let paths = self.paths()?;
        let _lock = GlobalFileLock::try_exclusive(&install::write_lock_path(&paths))?;

        FontbrewConfig::set(&paths.config_path(), request)
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

    fn search_provider_source(
        &self,
        paths: &FontbrewPaths,
        provider_source: ProviderSource,
        request: &SearchRequest,
    ) -> Result<Vec<crate::SearchResult>> {
        let http_client = self.http_client()?;

        match provider_source.provider {
            crate::ProviderKind::Fontsource => FontsourceProvider::new(paths, http_client.as_ref())
                .search(ProviderSearchRequest {
                    query: &provider_source.id,
                    limit: request.limit,
                }),
        }
    }
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
