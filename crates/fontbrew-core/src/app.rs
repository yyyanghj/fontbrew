use std::{fmt, sync::Arc};

use crate::config::FontbrewConfig;
use crate::error::{FontbrewError, Result};
use crate::fetch::NetworkClient;
use crate::fs::GlobalFileLock;
use crate::install;
use crate::model::{
    ensure_not_cancelled, CancellationToken, ConfigGetRequest, ConfigReport, ConfigSetRequest,
    ExecutionPolicy, InfoReport, InfoRequest, InstallPlan, InstallReport, InstallRequest,
    ListReport, NoCancellation, NoProgress, OutdatedReport, OutdatedRequest, ProgressEvent,
    ProgressSink, RemovePlan, RemoveReport, RemoveRequest, SearchReport, SearchRequest, UpdatePlan,
    UpdateReport, UpdateRequest,
};
use crate::platform::FontbrewPaths;
use crate::providers::{FontsourceProvider, ProviderSearchRequest};
use crate::sources::ProviderSource;
use crate::update;
use crate::{FamilyName, PackageId};

#[derive(Clone)]
pub struct FontbrewApp {
    paths: Option<FontbrewPaths>,
    network_client: Option<Arc<NetworkClient>>,
}

pub enum InstallPreparation {
    Plan(InstallPlan),
    FamilySelection(PendingFamilySelection),
}

pub struct PendingFamilySelection {
    families: Vec<FamilyName>,
    package_id_override: Option<PackageId>,
    parsed_archive: Option<install::ParsedFontArchive>,
}

impl PendingFamilySelection {
    fn new(
        parsed_archive: install::ParsedFontArchive,
        package_id_override: Option<PackageId>,
    ) -> Self {
        Self {
            families: parsed_archive.archive_families.clone(),
            package_id_override,
            parsed_archive: Some(parsed_archive),
        }
    }

    pub fn families(&self) -> &[FamilyName] {
        &self.families
    }

    fn take_parsed_archive(&mut self) -> Result<install::ParsedFontArchive> {
        self.parsed_archive
            .take()
            .ok_or_else(|| FontbrewError::Config {
                message: "pending family selection has already been consumed".to_string(),
            })
    }
}

impl Drop for PendingFamilySelection {
    fn drop(&mut self) {
        if let Some(parsed_archive) = &self.parsed_archive {
            install::cleanup_staging(&parsed_archive.staging_dir);
        }
    }
}

impl From<install::InstallPlanCandidate> for InstallPreparation {
    fn from(candidate: install::InstallPlanCandidate) -> Self {
        match candidate {
            install::InstallPlanCandidate::Plan(plan) => Self::Plan(plan),
            install::InstallPlanCandidate::FamilySelection {
                parsed_archive,
                package_id_override,
            } => Self::FamilySelection(PendingFamilySelection::new(
                parsed_archive,
                package_id_override,
            )),
        }
    }
}

impl FontbrewApp {
    pub fn new() -> Self {
        Self {
            paths: None,
            network_client: None,
        }
    }

    pub fn with_paths(paths: FontbrewPaths) -> Self {
        Self {
            paths: Some(paths),
            network_client: None,
        }
    }

    pub fn with_paths_and_network_client(
        paths: FontbrewPaths,
        network_client: Arc<NetworkClient>,
    ) -> Self {
        Self {
            paths: Some(paths),
            network_client: Some(network_client),
        }
    }

    pub async fn install_plan(&self, request: InstallRequest) -> Result<InstallPlan> {
        self.install_plan_with_cancellation(request, Arc::new(NoCancellation))
            .await
    }

    pub async fn install_plan_with_cancellation(
        &self,
        request: InstallRequest,
        cancellation: Arc<dyn CancellationToken>,
    ) -> Result<InstallPlan> {
        let mut progress = NoProgress;
        self.install_plan_with_progress_and_cancellation(request, &mut progress, cancellation)
            .await
    }

    pub async fn install_plan_with_progress_and_cancellation(
        &self,
        request: InstallRequest,
        progress: &mut dyn ProgressSink,
        cancellation: Arc<dyn CancellationToken>,
    ) -> Result<InstallPlan> {
        ensure_not_cancelled(cancellation.as_ref())?;
        let paths = self.paths()?;
        install::ensure_package_id_override_allowed_for_source(&request)?;
        match request.source.clone() {
            crate::InstallSource::LocalPath(_) => {
                let (result, events) = spawn_blocking_result(move || {
                    let mut recording = RecordingProgressSink::default();
                    let result = install::install_plan_with_progress(
                        &paths,
                        request,
                        &mut recording,
                        cancellation.as_ref(),
                    );
                    Ok((result, recording.events))
                })
                .await?;
                replay_progress(progress, events);
                result
            }
            crate::InstallSource::GitHubRepo { owner, repo } => {
                let github_repo = crate::sources::GitHubRepo::parse(format!("{owner}/{repo}"))?;
                install::github_repo_install_plan(
                    &paths,
                    github_repo,
                    request,
                    progress,
                    self.network_client()?.as_ref(),
                    cancellation.clone(),
                )
                .await
            }
            crate::InstallSource::Provider {
                provider: crate::ProviderKind::Fontsource,
                id,
            } => {
                install::fontsource_install_plan(
                    &paths,
                    id,
                    request,
                    progress,
                    self.network_client()?.as_ref(),
                    cancellation.clone(),
                )
                .await
            }
        }
    }

    pub async fn prepare_install(
        &self,
        request: InstallRequest,
        progress: &mut dyn ProgressSink,
        cancellation: Arc<dyn CancellationToken>,
    ) -> Result<InstallPreparation> {
        ensure_not_cancelled(cancellation.as_ref())?;
        let paths = self.paths()?;
        install::ensure_package_id_override_allowed_for_source(&request)?;
        match request.source.clone() {
            crate::InstallSource::LocalPath(_) => {
                let (result, events) = spawn_blocking_result(move || {
                    let mut recording = RecordingProgressSink::default();
                    let InstallRequest {
                        source,
                        package_id_override,
                        format_preference,
                        selected_families,
                        reinstall,
                        ..
                    } = request;
                    let crate::InstallSource::LocalPath(path) = source else {
                        unreachable!("local install branch should receive a local path");
                    };
                    let result = install::local_archive_install_plan_candidate(
                        &paths,
                        path,
                        package_id_override,
                        reinstall,
                        format_preference,
                        selected_families,
                        &mut recording,
                        cancellation.as_ref(),
                    )
                    .map(InstallPreparation::from);
                    Ok((result, recording.events))
                })
                .await?;
                replay_progress(progress, events);
                result
            }
            crate::InstallSource::GitHubRepo { owner, repo } => {
                let github_repo = crate::sources::GitHubRepo::parse(format!("{owner}/{repo}"))?;
                install::github_repo_install_plan_candidate(
                    &paths,
                    github_repo,
                    request,
                    progress,
                    self.network_client()?.as_ref(),
                    cancellation.clone(),
                )
                .await
                .map(InstallPreparation::from)
            }
            crate::InstallSource::Provider { .. } => {
                let plan = self
                    .install_plan_with_progress_and_cancellation(request, progress, cancellation)
                    .await?;
                Ok(InstallPreparation::Plan(plan))
            }
        }
    }

    pub async fn install_plans_with_progress_and_cancellation(
        &self,
        request: InstallRequest,
        progress: &mut dyn ProgressSink,
        cancellation: Arc<dyn CancellationToken>,
    ) -> Result<Vec<InstallPlan>> {
        ensure_not_cancelled(cancellation.as_ref())?;
        let paths = self.paths()?;
        install::ensure_package_id_override_allowed_for_source(&request)?;
        match request.source.clone() {
            crate::InstallSource::LocalPath(_) => {
                let (result, events) = spawn_blocking_result(move || {
                    let mut recording = RecordingProgressSink::default();
                    let result = install::install_plans_with_progress(
                        &paths,
                        request,
                        &mut recording,
                        cancellation.as_ref(),
                    );
                    Ok((result, recording.events))
                })
                .await?;
                replay_progress(progress, events);
                result
            }
            crate::InstallSource::GitHubRepo { owner, repo } => {
                let github_repo = crate::sources::GitHubRepo::parse(format!("{owner}/{repo}"))?;
                install::github_repo_install_plans(
                    &paths,
                    github_repo,
                    request,
                    progress,
                    self.network_client()?.as_ref(),
                    cancellation.clone(),
                )
                .await
            }
            crate::InstallSource::Provider { .. } => {
                let plan = self
                    .install_plan_with_progress_and_cancellation(request, progress, cancellation)
                    .await?;
                Ok(vec![plan])
            }
        }
    }

    pub async fn prepare_selected_families(
        &self,
        mut pending: PendingFamilySelection,
        selected_families: Vec<FamilyName>,
        progress: &mut dyn ProgressSink,
        cancellation: Arc<dyn CancellationToken>,
    ) -> Result<Vec<InstallPlan>> {
        ensure_not_cancelled(cancellation.as_ref())?;
        if pending.package_id_override.is_some() && !selected_families.is_empty() {
            return Err(FontbrewError::Config {
                message: "--id cannot be combined with --family".to_string(),
            });
        }

        let paths = self.paths()?;
        let parsed_archive = pending.take_parsed_archive()?;
        let (result, events) = spawn_blocking_result(move || {
            let mut recording = RecordingProgressSink::default();
            let result = install::family_install_plans_from_parsed_archive(
                &paths,
                parsed_archive,
                selected_families,
                &mut recording,
                cancellation.as_ref(),
            );
            Ok((result, recording.events))
        })
        .await?;
        replay_progress(progress, events);
        result
    }

    pub async fn apply_install(
        &self,
        plan: InstallPlan,
        policy: ExecutionPolicy,
        progress: &mut dyn ProgressSink,
        cancellation: Arc<dyn CancellationToken>,
    ) -> Result<InstallReport> {
        let paths = self.paths()?;
        let (result, events) = spawn_blocking_result(move || {
            let mut recording = RecordingProgressSink::default();
            let result =
                install::apply_install(&paths, plan, policy, &mut recording, cancellation.as_ref());
            Ok((result, recording.events))
        })
        .await?;
        replay_progress(progress, events);
        result
    }

    pub fn discard_install_plan(&self, plan: InstallPlan) {
        install::discard_install_plan(plan);
    }

    pub async fn list_packages(&self) -> Result<ListReport> {
        install::list_packages(&self.paths()?)
    }

    pub async fn package_info(&self, request: InfoRequest) -> Result<InfoReport> {
        install::package_info(&self.paths()?, request)
    }

    pub async fn remove_plan(&self, request: RemoveRequest) -> Result<RemovePlan> {
        install::remove_plan(&self.paths()?, request)
    }

    pub async fn remove_plan_with_cancellation(
        &self,
        request: RemoveRequest,
        cancellation: Arc<dyn CancellationToken>,
    ) -> Result<RemovePlan> {
        install::remove_plan_with_cancellation(&self.paths()?, request, cancellation.as_ref())
    }

    pub async fn apply_remove(
        &self,
        plan: RemovePlan,
        policy: ExecutionPolicy,
        progress: &mut dyn ProgressSink,
        cancellation: Arc<dyn CancellationToken>,
    ) -> Result<RemoveReport> {
        let paths = self.paths()?;
        let (result, events) = spawn_blocking_result(move || {
            let mut recording = RecordingProgressSink::default();
            let result =
                install::apply_remove(&paths, plan, policy, &mut recording, cancellation.as_ref());
            Ok((result, recording.events))
        })
        .await?;
        replay_progress(progress, events);
        result
    }

    pub async fn outdated(&self, request: OutdatedRequest) -> Result<OutdatedReport> {
        update::outdated(&self.paths()?, request, self.network_client()?.as_ref()).await
    }

    pub async fn update_plan(
        &self,
        request: UpdateRequest,
        progress: &mut dyn ProgressSink,
        cancellation: Arc<dyn CancellationToken>,
    ) -> Result<UpdatePlan> {
        update::update_plan(
            &self.paths()?,
            request,
            self.network_client()?.as_ref(),
            progress,
            cancellation.clone(),
        )
        .await
    }

    pub async fn apply_update(
        &self,
        plan: UpdatePlan,
        policy: ExecutionPolicy,
        progress: &mut dyn ProgressSink,
        cancellation: Arc<dyn CancellationToken>,
    ) -> Result<UpdateReport> {
        let paths = self.paths()?;
        let (result, events) = spawn_blocking_result(move || {
            let mut recording = RecordingProgressSink::default();
            let result =
                update::apply_update(&paths, plan, policy, &mut recording, cancellation.as_ref());
            Ok((result, recording.events))
        })
        .await?;
        replay_progress(progress, events);
        result
    }

    pub fn discard_update_plan(&self, plan: UpdatePlan) {
        update::discard_update_plan(plan);
    }

    pub async fn search(&self, request: SearchRequest) -> Result<SearchReport> {
        let paths = self.paths()?;
        if let Some(provider_source) = ProviderSource::parse_prefixed(&request.query) {
            let results = self
                .search_provider_source(&paths, provider_source, &request)
                .await?;
            return Ok(SearchReport { results });
        }

        let network_client = self.network_client()?;
        let results = FontsourceProvider::new(&paths, network_client.as_ref())
            .search(ProviderSearchRequest {
                query: &request.query,
                limit: request.limit,
            })
            .await?;

        Ok(SearchReport { results })
    }

    pub async fn config_get(&self, request: ConfigGetRequest) -> Result<ConfigReport> {
        FontbrewConfig::get(&self.paths()?.config_path(), request)
    }

    pub async fn config_set(&self, request: ConfigSetRequest) -> Result<ConfigReport> {
        let paths = self.paths()?;
        spawn_blocking_result(move || {
            let _lock = GlobalFileLock::try_exclusive(&install::write_lock_path(&paths))?;
            FontbrewConfig::set(&paths.config_path(), request)
        })
        .await
    }

    fn paths(&self) -> Result<FontbrewPaths> {
        match &self.paths {
            Some(paths) => Ok(paths.clone()),
            None => FontbrewPaths::resolve(),
        }
    }

    fn network_client(&self) -> Result<Arc<NetworkClient>> {
        if let Some(network_client) = &self.network_client {
            return Ok(network_client.clone());
        }

        Ok(Arc::new(NetworkClient::new()?))
    }

    async fn search_provider_source(
        &self,
        paths: &FontbrewPaths,
        provider_source: ProviderSource,
        request: &SearchRequest,
    ) -> Result<Vec<crate::SearchResult>> {
        let network_client = self.network_client()?;

        match provider_source.provider {
            crate::ProviderKind::Fontsource => {
                FontsourceProvider::new(paths, network_client.as_ref())
                    .search(ProviderSearchRequest {
                        query: &provider_source.id,
                        limit: request.limit,
                    })
                    .await
            }
        }
    }
}

#[derive(Default)]
struct RecordingProgressSink {
    events: Vec<ProgressEvent>,
}

impl ProgressSink for RecordingProgressSink {
    fn emit(&mut self, event: ProgressEvent) {
        self.events.push(event);
    }
}

fn replay_progress(progress: &mut dyn ProgressSink, events: Vec<ProgressEvent>) {
    for event in events {
        progress.emit(event);
    }
}

async fn spawn_blocking_result<T>(work: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T>
where
    T: Send + 'static,
{
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| FontbrewError::Io(std::io::Error::other(error.to_string())))?
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
                "network_client",
                &self.network_client.as_ref().map(|_| "<network-client>"),
            )
            .finish()
    }
}
