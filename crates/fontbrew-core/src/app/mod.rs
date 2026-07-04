use std::{fmt, fs, sync::Arc};

use crate::config::FontbrewConfig;
use crate::error::{FontbrewError, Result};
use crate::fetch::{HttpClient, HttpHeader, HttpRequest, ReqwestHttpClient};
use crate::fs::GlobalFileLock;
use crate::install;
use crate::model::{
    ensure_not_cancelled, CancellationToken, ConfigGetRequest, ConfigReport, ConfigSetRequest,
    ExecutionPolicy, InfoReport, InfoRequest, InstallPlan, InstallReport, InstallRequest,
    ListReport, NoCancellation, OutdatedReport, OutdatedRequest, ProgressSink,
    RegistryStatusReport, RegistryUpdateReport, RemovePlan, RemoveReport, RemoveRequest,
    SearchReport, SearchRequest, UpdatePlan, UpdateReport, UpdateRequest,
};
use crate::platform::FontbrewPaths;
use crate::providers::{FontsourceProvider, GoogleProvider, ProviderSearchRequest};
use crate::registry::{
    registry_url_from_env, registry_url_not_configured_error, RegistryHttpClient,
    RegistrySnapshotStore, ReqwestRegistryHttpClient,
};
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
        ensure_not_cancelled(cancellation)?;
        let paths = self.paths()?;
        install::ensure_package_id_override_allowed_for_source(&request)?;
        match request.source.clone() {
            crate::InstallSource::LocalPath(_) => {
                install::install_plan(&paths, request, cancellation)
            }
            crate::InstallSource::RegistryName(short_name) => {
                self.refresh_registry_snapshot(&paths)?;
                ensure_not_cancelled(cancellation)?;
                let recipe =
                    RegistrySnapshotStore::new(paths.clone()).resolve_short_name(&short_name)?;
                ensure_not_cancelled(cancellation)?;
                install::registry_recipe_install_plan(
                    &paths,
                    recipe,
                    request,
                    self.http_client()?.as_ref(),
                    cancellation,
                )
            }
            crate::InstallSource::GitHubRepo { owner, repo } => {
                let github_repo = crate::sources::GitHubRepo::parse(format!("{owner}/{repo}"))?;
                install::github_repo_install_plan(
                    &paths,
                    github_repo,
                    request,
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
                self.http_client()?.as_ref(),
                cancellation,
            ),
            crate::InstallSource::Provider {
                provider: crate::ProviderKind::Google,
                id,
            } => install::google_install_plan(
                &paths,
                id,
                request,
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

        let mut results = self.search_registry_snapshot(&paths, &request)?;

        let remaining_limit = request
            .limit
            .map(|limit| limit.saturating_sub(results.len()));
        if remaining_limit != Some(0) {
            let http_client = self.http_client()?;
            let fontsource_results = FontsourceProvider::new(&paths, http_client.as_ref()).search(
                ProviderSearchRequest {
                    query: &request.query,
                    limit: remaining_limit,
                },
            )?;
            results.extend(fontsource_results);
        }

        let remaining_limit = request
            .limit
            .map(|limit| limit.saturating_sub(results.len()));
        if remaining_limit != Some(0) && GoogleProvider::api_key_is_configured() {
            let google_results = GoogleProvider::new(&paths, self.http_client()?.as_ref()).search(
                ProviderSearchRequest {
                    query: &request.query,
                    limit: remaining_limit,
                },
            )?;
            results.extend(google_results);
        }

        if let Some(limit) = request.limit {
            results.truncate(limit);
        }

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

    pub fn registry_update(&self) -> Result<RegistryUpdateReport> {
        let paths = self.paths()?;
        let store = RegistrySnapshotStore::new(paths);
        let client = ReqwestRegistryHttpClient::default();
        let registry_url = registry_url_from_env().ok_or_else(registry_url_not_configured_error)?;

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

    fn refresh_registry_snapshot(&self, paths: &FontbrewPaths) -> Result<()> {
        let store = RegistrySnapshotStore::new(paths.clone());
        store.ensure_snapshot_exists()?;

        let Some(registry_url) = registry_url_from_env() else {
            return Ok(());
        };

        if let Some(http_client) = &self.http_client {
            let client = AppRegistryHttpClient {
                http_client: http_client.as_ref(),
            };
            store.update_from_client(&client, &registry_url)?;
            return Ok(());
        }

        let client = ReqwestRegistryHttpClient::default();
        store.update_from_client(&client, &registry_url)?;
        Ok(())
    }

    fn search_registry_snapshot(
        &self,
        paths: &FontbrewPaths,
        request: &SearchRequest,
    ) -> Result<Vec<crate::SearchResult>> {
        match self.refresh_registry_snapshot(paths) {
            Ok(()) => {}
            Err(FontbrewError::Network { .. }) => {}
            Err(error) => return Err(error),
        }

        Ok(RegistrySnapshotStore::new(paths.clone())
            .search(&request.query, None)?
            .into_iter()
            .map(|recipe| crate::SearchResult {
                package_id: recipe.package_id.clone(),
                display_name: recipe.name,
                source: format!("registry:{}", recipe.package_id.as_str()),
                version: None,
                families: recipe.families,
            })
            .collect())
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
            crate::ProviderKind::Google => {
                GoogleProvider::new(paths, http_client.as_ref()).search(ProviderSearchRequest {
                    query: &provider_source.id,
                    limit: request.limit,
                })
            }
        }
    }
}

struct AppRegistryHttpClient<'a> {
    http_client: &'a dyn HttpClient,
}

impl RegistryHttpClient for AppRegistryHttpClient<'_> {
    fn get_text(&self, url: &str) -> Result<String> {
        if let Some(path) = url.strip_prefix("file://") {
            return fs::read_to_string(path).map_err(FontbrewError::from);
        }

        let response = self.http_client.get(HttpRequest {
            url: url.to_string(),
            display_url: None,
            headers: vec![
                HttpHeader {
                    name: "User-Agent".to_string(),
                    value: "fontbrew".to_string(),
                },
                HttpHeader {
                    name: "Accept".to_string(),
                    value: "application/json".to_string(),
                },
            ],
        })?;

        if !(200..300).contains(&response.status) {
            return Err(FontbrewError::Network {
                message: format!(
                    "registry request failed with HTTP {} for {url}",
                    response.status
                ),
            });
        }

        String::from_utf8(response.body).map_err(|source| FontbrewError::Network {
            message: format!("could not read registry response from {url}: {source}"),
        })
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
