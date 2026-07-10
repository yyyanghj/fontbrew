use std::{
    env,
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::{
    archives::{ArchiveExtractionOptions, ZipArchiveExtractor},
    config::FontbrewConfig,
    error::{FontbrewError, Result},
    fetch::NetworkClient,
    fonts::{FontFaceMetadata, FontFileFormat, FontMetadataReader, TtfParserMetadataReader},
    fs::GlobalFileLock,
    install::{self, ParsedArchiveInstallTarget},
    model::{
        ensure_not_cancelled, ApplyOptions, CancellationToken, ConfigValue, ExecutionPolicy,
        InstallCandidate, InstallCandidateId, InstallPlan, InstallPlanSummary, InstallReport,
        InstallReportSet, InstallRequest, InstallSource, ListPackage, NoCancellation, NoProgress,
        OutdatedReport, OutdatedRequest, PackageId, PackageInfo, PlanRisk, PlannedChange,
        PreparedInstallPackage, PreparedInstallSource, ProgressEvent, ProgressSink,
        ProgressSubject, RemovePlan, RemoveReport, SearchRequest, SearchResult, UpdatePlan,
        UpdateReport, UpdateRequest,
    },
    platform::{DefaultFontbrewLocations, FontbrewPaths},
    providers::{github, FontsourceProvider, ProviderSearchRequest, ResolvedProviderPackage},
    sources::{GitHubRepo, ProviderSource},
    update, FamilyName, FontFormat, ProviderKind,
};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FontbrewOptions {
    pub store_dir: Option<PathBuf>,
    pub config_path: Option<PathBuf>,
    pub activation_dir: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct Fontbrew {
    paths: FontbrewPaths,
    network_client: Arc<NetworkClient>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchInstallMetadataRequest {
    pub source: InstallSource,
}

#[derive(Debug)]
pub struct InstallMetadata {
    source: InstallSource,
    package_id: Option<PackageId>,
    asset_selection_label: String,
    assets: Vec<String>,
    inner: InstallMetadataInner,
}

#[derive(Debug)]
enum InstallMetadataInner {
    LocalArchive {
        path: PathBuf,
    },
    GitHub {
        release: github::ResolvedGitHubRelease,
        source_label: String,
        source: PreparedInstallSource,
    },
    Provider {
        resolved: ResolvedProviderPackage,
    },
}

#[derive(Debug)]
pub struct PrepareInstallAssetRequest {
    pub metadata: InstallMetadata,
    pub asset_selector: Option<String>,
    pub format_preference: Vec<FontFormat>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrepareInstallSourceRequest {
    pub source: InstallSource,
    pub asset_selector: Option<String>,
    pub format_preference: Option<Vec<FontFormat>>,
}

#[derive(Debug)]
pub struct InstallSourcePreparation {
    candidates: Vec<InstallCandidate>,
    inner: Option<InstallSourcePreparationInner>,
}

#[derive(Debug)]
enum InstallSourcePreparationInner {
    ParsedArchive {
        parsed_archive: install::ParsedFontArchive,
    },
    PreparedPackage {
        prepared: PreparedInstallPackage,
    },
}

#[derive(Debug)]
pub struct PlanInstallRequest {
    pub preparation: InstallSourcePreparation,
    pub targets: Vec<InstallTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallTarget {
    pub candidate_id: InstallCandidateId,
    pub package_id_override: Option<PackageId>,
    pub reinstall: bool,
}

#[derive(Debug)]
pub struct InstallPlanSet {
    plans: Option<Vec<InstallPlan>>,
    summaries: Vec<InstallPlanSummary>,
    risks: Vec<PlanRisk>,
    changes: Vec<PlannedChange>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractArchiveRequest {
    pub archive_path: PathBuf,
    pub destination_dir: PathBuf,
    pub options: Option<ArchiveExtractionOptions>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedArchive {
    pub font_files: Vec<FontFileInput>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontFileInput {
    pub path: PathBuf,
    pub format: Option<FontFormat>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseFontsRequest {
    pub files: Vec<FontFileInput>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedFonts {
    pub files: Vec<ParsedFontFileInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedFontFileInfo {
    pub path: PathBuf,
    pub faces: Vec<ParsedFontFaceInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedFontFaceInfo {
    pub family: FamilyName,
    pub style: String,
    pub weight: u16,
    pub format: FontFormat,
}

impl InstallMetadata {
    pub fn source(&self) -> &InstallSource {
        &self.source
    }

    pub fn package_id(&self) -> Option<&PackageId> {
        self.package_id.as_ref()
    }

    pub fn assets(&self) -> &[String] {
        &self.assets
    }

    pub fn asset_selection_label(&self) -> &str {
        &self.asset_selection_label
    }
}

impl Fontbrew {
    pub fn new(options: FontbrewOptions) -> Result<Self> {
        let cwd = env::current_dir()?;
        let default_locations = if options.store_dir.is_none()
            || options.config_path.is_none()
            || options.activation_dir.is_none()
        {
            Some(FontbrewPaths::default_locations()?)
        } else {
            None
        };
        let store_dir = absolute_or_default(
            options.store_dir,
            default_store_dir,
            &default_locations,
            &cwd,
        );
        let config_path = absolute_or_default(
            options.config_path,
            default_config_path,
            &default_locations,
            &cwd,
        );
        let activation_dir = absolute_or_default(
            options.activation_dir,
            default_activation_dir,
            &default_locations,
            &cwd,
        );

        Ok(Self {
            paths: FontbrewPaths::from_locations(store_dir, config_path, activation_dir),
            network_client: Arc::new(NetworkClient::new()?),
        })
    }

    #[doc(hidden)]
    pub fn with_network_client(mut self, network_client: Arc<NetworkClient>) -> Self {
        self.network_client = network_client;
        self
    }

    pub async fn fetch_install_metadata(
        &self,
        request: FetchInstallMetadataRequest,
    ) -> Result<InstallMetadata> {
        match request.source {
            InstallSource::LocalPath(path) => Ok(InstallMetadata {
                source: InstallSource::LocalPath(path.clone()),
                package_id: None,
                asset_selection_label: path.display().to_string(),
                assets: Vec::new(),
                inner: InstallMetadataInner::LocalArchive { path },
            }),
            InstallSource::GitHubRepo { owner, repo } => {
                let github_repo = GitHubRepo::parse(format!("{owner}/{repo}"))?;
                let source_label = github_repo.label();
                let release = github::resolve_latest_stable_release(
                    self.network_client.as_ref(),
                    &github_repo,
                )
                .await?;
                let assets = release.installable_asset_names();

                Ok(InstallMetadata {
                    source: InstallSource::GitHubRepo {
                        owner: owner.clone(),
                        repo: repo.clone(),
                    },
                    package_id: None,
                    asset_selection_label: source_label.clone(),
                    assets,
                    inner: InstallMetadataInner::GitHub {
                        release,
                        source_label,
                        source: PreparedInstallSource::GitHub { owner, repo },
                    },
                })
            }
            InstallSource::Provider {
                provider: ProviderKind::Fontsource,
                id,
            } => {
                let resolved = FontsourceProvider::new(&self.paths, self.network_client.as_ref())
                    .resolve_install_package(&id)
                    .await?;
                let package_id = resolved.package_id.clone();

                Ok(InstallMetadata {
                    source: InstallSource::Provider {
                        provider: ProviderKind::Fontsource,
                        id: id.clone(),
                    },
                    package_id: Some(package_id),
                    asset_selection_label: format!("fontsource:{id}"),
                    assets: Vec::new(),
                    inner: InstallMetadataInner::Provider { resolved },
                })
            }
        }
    }

    pub async fn prepare_install_asset(
        &self,
        request: PrepareInstallAssetRequest,
        progress: &mut dyn ProgressSink,
        cancellation: Arc<dyn CancellationToken>,
    ) -> Result<InstallSourcePreparation> {
        ensure_not_cancelled(cancellation.as_ref())?;
        match request.metadata.inner {
            InstallMetadataInner::LocalArchive { path } => {
                let paths = self.paths.clone();
                let format_preference = request.format_preference;
                let (result, events) = spawn_blocking_result(move || {
                    let mut recording = RecordingProgressSink::default();
                    let result = install::prepare_local_archive_install_source(
                        &paths,
                        path,
                        format_preference,
                        &mut recording,
                        cancellation.as_ref(),
                    );
                    Ok((result, recording.events))
                })
                .await?;
                replay_progress(progress, events);
                let parsed_archive = result?;
                let candidates =
                    install::install_candidates_from_parsed_archive(&parsed_archive, None)?;

                Ok(InstallSourcePreparation {
                    candidates,
                    inner: Some(InstallSourcePreparationInner::ParsedArchive { parsed_archive }),
                })
            }
            InstallMetadataInner::GitHub {
                release,
                source_label,
                source,
            } => {
                let asset = github::select_resolved_release_asset(
                    &release,
                    request.asset_selector.as_deref(),
                    &source_label,
                )?;
                let options = install::RemoteInstallOptions {
                    asset_selector: None,
                    package_id: None,
                    progress_subject: Some(ProgressSubject::source(source_label)),
                    reinstall: false,
                    explicit_format_preference: crate::config::dedupe_formats(
                        request.format_preference,
                    ),
                    family_boundary: None,
                };
                let parsed_archive = install::prepare_resolved_github_release_parsed_archive(
                    &self.paths,
                    asset,
                    source,
                    options,
                    progress,
                    self.network_client.as_ref(),
                    cancellation,
                )
                .await?;
                let candidates =
                    install::install_candidates_from_parsed_archive(&parsed_archive, None)?;

                Ok(InstallSourcePreparation {
                    candidates,
                    inner: Some(InstallSourcePreparationInner::ParsedArchive { parsed_archive }),
                })
            }
            InstallMetadataInner::Provider { resolved } => {
                if request.asset_selector.is_some() {
                    return Err(FontbrewError::Config {
                        message: "--asset is not supported for Fontsource provider sources"
                            .to_string(),
                    });
                }

                let package_id = resolved.package_id.clone();
                let options = install::RemoteInstallOptions {
                    asset_selector: None,
                    package_id: Some(package_id.clone()),
                    progress_subject: Some(ProgressSubject::package(&package_id)),
                    reinstall: false,
                    explicit_format_preference: crate::config::dedupe_formats(
                        request.format_preference,
                    ),
                    family_boundary: None,
                };
                let prepared = install::prepare_provider_package(
                    &self.paths,
                    resolved,
                    options,
                    progress,
                    self.network_client.as_ref(),
                    cancellation,
                )
                .await?;
                let candidates = vec![install::install_candidate_from_prepared(&prepared)];

                Ok(InstallSourcePreparation {
                    candidates,
                    inner: Some(InstallSourcePreparationInner::PreparedPackage { prepared }),
                })
            }
        }
    }

    pub async fn prepare_install_source(
        &self,
        request: PrepareInstallSourceRequest,
    ) -> Result<InstallSourcePreparation> {
        match request.source {
            InstallSource::LocalPath(path) => {
                let paths = self.paths.clone();
                let format_preference = request.format_preference.unwrap_or_default();
                let parsed_archive = spawn_blocking_result(move || {
                    let mut progress = NoProgress;
                    install::prepare_local_archive_install_source(
                        &paths,
                        path,
                        format_preference,
                        &mut progress,
                        &NoCancellation,
                    )
                })
                .await?;
                let candidates =
                    install::install_candidates_from_parsed_archive(&parsed_archive, None)?;

                Ok(InstallSourcePreparation {
                    candidates,
                    inner: Some(InstallSourcePreparationInner::ParsedArchive { parsed_archive }),
                })
            }
            InstallSource::GitHubRepo { owner, repo } => {
                let github_repo = GitHubRepo::parse(format!("{owner}/{repo}"))?;
                let mut progress = NoProgress;
                let parsed_archive = install::prepare_github_repo_install_source(
                    &self.paths,
                    github_repo,
                    request.asset_selector,
                    request.format_preference.unwrap_or_default(),
                    &mut progress,
                    self.network_client.as_ref(),
                    Arc::new(NoCancellation),
                )
                .await?;
                let candidates =
                    install::install_candidates_from_parsed_archive(&parsed_archive, None)?;

                Ok(InstallSourcePreparation {
                    candidates,
                    inner: Some(InstallSourcePreparationInner::ParsedArchive { parsed_archive }),
                })
            }
            InstallSource::Provider {
                provider: ProviderKind::Fontsource,
                id,
            } => {
                if request.asset_selector.is_some() {
                    return Err(FontbrewError::Config {
                        message: "--asset is not supported for Fontsource provider sources"
                            .to_string(),
                    });
                }

                let mut progress = NoProgress;
                let prepared = install::prepare_fontsource_install_source(
                    &self.paths,
                    id,
                    request.format_preference.unwrap_or_default(),
                    &mut progress,
                    self.network_client.as_ref(),
                    Arc::new(NoCancellation),
                )
                .await?;
                let candidates = vec![install::install_candidate_from_prepared(&prepared)];

                Ok(InstallSourcePreparation {
                    candidates,
                    inner: Some(InstallSourcePreparationInner::PreparedPackage { prepared }),
                })
            }
        }
    }

    pub fn extract_archive(&self, request: ExtractArchiveRequest) -> Result<ExtractedArchive> {
        let extractor = ZipArchiveExtractor::new(request.options.unwrap_or_default());
        let font_files = extractor
            .extract(request.archive_path, request.destination_dir)?
            .into_iter()
            .map(|font_file| FontFileInput {
                path: font_file.path,
                format: Some(font_format_from_reader_format(font_file.format)),
            })
            .collect();

        Ok(ExtractedArchive { font_files })
    }

    pub fn parse_fonts(&self, request: ParseFontsRequest) -> Result<ParsedFonts> {
        let reader = TtfParserMetadataReader;
        let mut files = Vec::with_capacity(request.files.len());

        for file in request.files {
            let raw_faces = match file.format {
                Some(format) => {
                    reader.read_file_with_format(&file.path, font_reader_format(format))?
                }
                None => reader.read_file(&file.path)?,
            };
            let faces = raw_faces.into_iter().map(parsed_font_face_info).collect();
            files.push(ParsedFontFileInfo {
                path: file.path,
                faces,
            });
        }

        Ok(ParsedFonts { files })
    }

    pub fn plan_install(&self, request: PlanInstallRequest) -> Result<InstallPlanSet> {
        let PlanInstallRequest {
            mut preparation,
            targets,
        } = request;
        let candidates = preparation.candidates.clone();
        let Some(inner) = preparation.take_inner() else {
            return Err(FontbrewError::Config {
                message: "install source preparation has already been consumed".to_string(),
            });
        };

        match inner {
            InstallSourcePreparationInner::ParsedArchive { parsed_archive } => {
                let parsed_targets = match targets
                    .into_iter()
                    .map(|target| parsed_archive_target(target, &candidates))
                    .collect::<Result<Vec<_>>>()
                {
                    Ok(parsed_targets) => parsed_targets,
                    Err(error) => {
                        install::cleanup_staging(&parsed_archive.staging_dir);
                        return Err(error);
                    }
                };
                let mut progress = NoProgress;
                let plans = install::install_plans_from_parsed_archive_targets(
                    &self.paths,
                    parsed_archive,
                    parsed_targets,
                    &mut progress,
                    &NoCancellation,
                )?;

                InstallPlanSet::new(plans)
            }
            InstallSourcePreparationInner::PreparedPackage { mut prepared } => {
                let target = single_prepared_target(targets, &candidates, &prepared)?;
                prepared.reinstall = target.reinstall;
                let mut progress = NoProgress;
                let plan =
                    install::install_plan_from_prepared(&self.paths, prepared, &mut progress)?;

                InstallPlanSet::new(vec![plan])
            }
        }
    }

    pub async fn apply_install(
        &self,
        plans: InstallPlanSet,
        options: ApplyOptions,
    ) -> Result<InstallReportSet> {
        let mut progress = NoProgress;
        self.apply_install_with_progress_and_cancellation(
            plans,
            options,
            &mut progress,
            Arc::new(NoCancellation),
        )
        .await
    }

    pub async fn apply_install_with_progress_and_cancellation(
        &self,
        plans: InstallPlanSet,
        options: ApplyOptions,
        progress: &mut dyn ProgressSink,
        cancellation: Arc<dyn CancellationToken>,
    ) -> Result<InstallReportSet> {
        let mut pending_plans = plans.into_plans();
        pending_plans.reverse();
        let mut reports = Vec::new();

        while let Some(plan) = pending_plans.pop() {
            let paths = self.paths.clone();
            let policy = options.policy.clone();
            let cancellation = cancellation.clone();
            let (result, events) = spawn_blocking_result(move || {
                let mut recording = RecordingProgressSink::default();
                let result = install::apply_install(
                    &paths,
                    plan,
                    policy,
                    &mut recording,
                    cancellation.as_ref(),
                );
                Ok((result, recording.events))
            })
            .await?;
            replay_progress(progress, events);

            match result {
                Ok(report) => reports.push(report),
                Err(error) => {
                    for plan in pending_plans {
                        install::discard_install_plan(plan);
                    }
                    return Err(error);
                }
            }
        }

        Ok(InstallReportSet { packages: reports })
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
        let paths = self.paths.clone();
        install::ensure_package_id_override_allowed_for_source(&request)?;
        match request.source.clone() {
            InstallSource::LocalPath(_) => {
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
            InstallSource::GitHubRepo { owner, repo } => {
                let github_repo = GitHubRepo::parse(format!("{owner}/{repo}"))?;
                install::github_repo_install_plan(
                    &paths,
                    github_repo,
                    request,
                    progress,
                    self.network_client.as_ref(),
                    cancellation.clone(),
                )
                .await
            }
            InstallSource::Provider {
                provider: ProviderKind::Fontsource,
                id,
            } => {
                install::fontsource_install_plan(
                    &paths,
                    id,
                    request,
                    progress,
                    self.network_client.as_ref(),
                    cancellation.clone(),
                )
                .await
            }
        }
    }

    pub async fn apply_install_plan(
        &self,
        plan: InstallPlan,
        policy: ExecutionPolicy,
        progress: &mut dyn ProgressSink,
        cancellation: Arc<dyn CancellationToken>,
    ) -> Result<InstallReport> {
        let paths = self.paths.clone();
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

    pub fn list_packages(&self) -> Result<Vec<ListPackage>> {
        install::list_packages(&self.paths)
    }

    pub fn package_info(&self, package_id: &PackageId) -> Result<PackageInfo> {
        install::package_info(&self.paths, package_id)
    }

    pub fn plan_remove(&self, package_id: PackageId) -> Result<RemovePlan> {
        install::remove_plan(&self.paths, package_id)
    }

    pub fn plan_remove_with_cancellation(
        &self,
        package_id: PackageId,
        cancellation: Arc<dyn CancellationToken>,
    ) -> Result<RemovePlan> {
        install::remove_plan_with_cancellation(&self.paths, package_id, cancellation.as_ref())
    }

    pub async fn apply_remove(
        &self,
        plan: RemovePlan,
        policy: ExecutionPolicy,
        progress: &mut dyn ProgressSink,
        cancellation: Arc<dyn CancellationToken>,
    ) -> Result<RemoveReport> {
        let paths = self.paths.clone();
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
        update::outdated(&self.paths, request, self.network_client.as_ref()).await
    }

    pub async fn update_plan(
        &self,
        request: UpdateRequest,
        progress: &mut dyn ProgressSink,
        cancellation: Arc<dyn CancellationToken>,
    ) -> Result<UpdatePlan> {
        update::update_plan(
            &self.paths,
            request,
            self.network_client.as_ref(),
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
        let paths = self.paths.clone();
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

    pub async fn search(&self, request: SearchRequest) -> Result<Vec<SearchResult>> {
        if request.limit == Some(0) {
            return Err(FontbrewError::Config {
                message: "search limit must be greater than 0".to_string(),
            });
        }

        if let Some(provider_source) = ProviderSource::parse_prefixed(&request.query) {
            return self.search_provider_source(provider_source, &request).await;
        }

        FontsourceProvider::new(&self.paths, self.network_client.as_ref())
            .search(ProviderSearchRequest {
                query: &request.query,
                limit: request.limit,
            })
            .await
    }

    pub fn config_get(&self, key: &str) -> Result<ConfigValue> {
        FontbrewConfig::get(&self.paths.config_path(), key)
    }

    pub async fn config_set(&self, key: String, value: String) -> Result<ConfigValue> {
        let paths = self.paths.clone();
        spawn_blocking_result(move || {
            let _lock = GlobalFileLock::try_exclusive(&install::write_lock_path(&paths))?;
            FontbrewConfig::set(&paths.config_path(), &key, &value)
        })
        .await
    }

    async fn search_provider_source(
        &self,
        provider_source: ProviderSource,
        request: &SearchRequest,
    ) -> Result<Vec<crate::SearchResult>> {
        match provider_source.provider {
            ProviderKind::Fontsource => {
                FontsourceProvider::new(&self.paths, self.network_client.as_ref())
                    .search(ProviderSearchRequest {
                        query: &provider_source.id,
                        limit: request.limit,
                    })
                    .await
            }
        }
    }
}

impl InstallSourcePreparation {
    pub fn candidates(&self) -> &[InstallCandidate] {
        &self.candidates
    }

    fn take_inner(&mut self) -> Option<InstallSourcePreparationInner> {
        self.inner.take()
    }
}

impl Drop for InstallSourcePreparation {
    fn drop(&mut self) {
        let Some(inner) = self.inner.take() else {
            return;
        };

        match inner {
            InstallSourcePreparationInner::ParsedArchive { parsed_archive } => {
                install::cleanup_staging(&parsed_archive.staging_dir);
            }
            InstallSourcePreparationInner::PreparedPackage { prepared } => {
                install::cleanup_staging(&prepared.staging_dir);
            }
        }
    }
}

impl InstallPlanSet {
    fn new(plans: Vec<InstallPlan>) -> Result<Self> {
        if plans.is_empty() {
            return Err(FontbrewError::Config {
                message: "install requires at least one plan".to_string(),
            });
        }

        let summaries = plans
            .iter()
            .map(InstallPlanSummary::from)
            .collect::<Vec<_>>();
        let risks = plans
            .iter()
            .flat_map(|plan| plan.risks.iter().cloned())
            .collect();
        let changes = plans
            .iter()
            .flat_map(|plan| plan.changes.iter().cloned())
            .collect();

        Ok(Self {
            plans: Some(plans),
            summaries,
            risks,
            changes,
        })
    }

    pub fn plans(&self) -> &[InstallPlanSummary] {
        &self.summaries
    }

    pub fn risks(&self) -> &[PlanRisk] {
        &self.risks
    }

    pub fn changes(&self) -> &[PlannedChange] {
        &self.changes
    }

    fn into_plans(mut self) -> Vec<InstallPlan> {
        self.plans.take().unwrap_or_default()
    }
}

impl Drop for InstallPlanSet {
    fn drop(&mut self) {
        let Some(plans) = self.plans.take() else {
            return;
        };

        for plan in plans {
            install::discard_install_plan(plan);
        }
    }
}

fn parsed_archive_target(
    target: InstallTarget,
    candidates: &[InstallCandidate],
) -> Result<ParsedArchiveInstallTarget> {
    let candidate = find_candidate(candidates, &target.candidate_id)?;
    let family =
        candidate
            .families
            .first()
            .cloned()
            .ok_or_else(|| FontbrewError::ArchiveRejected {
                reason: format!(
                    "install candidate {} has no families",
                    candidate.id.as_str()
                ),
            })?;

    Ok(ParsedArchiveInstallTarget {
        family,
        package_id: candidate.package_id.clone(),
        package_id_override: target.package_id_override,
        reinstall: target.reinstall,
    })
}

fn single_prepared_target(
    targets: Vec<InstallTarget>,
    candidates: &[InstallCandidate],
    prepared: &PreparedInstallPackage,
) -> Result<InstallTarget> {
    if targets.len() != 1 {
        install::cleanup_staging(&prepared.staging_dir);
        return Err(FontbrewError::Config {
            message: "prepared provider sources require exactly one install target".to_string(),
        });
    }

    let mut targets = targets;
    let target = targets.remove(0);
    if let Err(error) = find_candidate(candidates, &target.candidate_id) {
        install::cleanup_staging(&prepared.staging_dir);
        return Err(error);
    }
    if target.package_id_override.is_some() {
        install::cleanup_staging(&prepared.staging_dir);
        return Err(FontbrewError::Config {
            message: "--id is only supported for local archive and direct GitHub sources"
                .to_string(),
        });
    }

    Ok(target)
}

fn find_candidate<'a>(
    candidates: &'a [InstallCandidate],
    candidate_id: &InstallCandidateId,
) -> Result<&'a InstallCandidate> {
    candidates
        .iter()
        .find(|candidate| &candidate.id == candidate_id)
        .ok_or_else(|| FontbrewError::Config {
            message: format!("unknown install candidate: {}", candidate_id.as_str()),
        })
}

fn absolute_or_default(
    path: Option<PathBuf>,
    default: impl FnOnce(&DefaultFontbrewLocations) -> &PathBuf,
    default_locations: &Option<DefaultFontbrewLocations>,
    cwd: &Path,
) -> PathBuf {
    let path = match path {
        Some(path) => path,
        None => default(
            default_locations
                .as_ref()
                .expect("default locations should be resolved when any option is absent"),
        )
        .clone(),
    };
    if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    }
}

fn default_store_dir(defaults: &DefaultFontbrewLocations) -> &PathBuf {
    &defaults.store_dir
}

fn default_config_path(defaults: &DefaultFontbrewLocations) -> &PathBuf {
    &defaults.config_path
}

fn default_activation_dir(defaults: &DefaultFontbrewLocations) -> &PathBuf {
    &defaults.activation_dir
}

fn parsed_font_face_info(face: FontFaceMetadata) -> ParsedFontFaceInfo {
    ParsedFontFaceInfo {
        family: face.family_name.clone(),
        style: font_face_style(&face),
        weight: face.weight.unwrap_or(400),
        format: font_format_from_reader_format(face.format),
    }
}

fn font_face_style(face: &FontFaceMetadata) -> String {
    if let Some(subfamily_name) = &face.subfamily_name {
        return subfamily_name.clone();
    }

    if face.is_italic {
        "Italic".to_string()
    } else if face.is_oblique {
        "Oblique".to_string()
    } else {
        "Regular".to_string()
    }
}

fn font_format_from_reader_format(format: FontFileFormat) -> FontFormat {
    match format {
        FontFileFormat::Ttf => FontFormat::Ttf,
        FontFileFormat::Otf => FontFormat::Otf,
        FontFileFormat::Ttc => FontFormat::Ttc,
        FontFileFormat::Otc => FontFormat::Otc,
    }
}

fn font_reader_format(format: FontFormat) -> FontFileFormat {
    match format {
        FontFormat::Ttf => FontFileFormat::Ttf,
        FontFormat::Otf => FontFileFormat::Otf,
        FontFormat::Ttc => FontFileFormat::Ttc,
        FontFormat::Otc => FontFileFormat::Otc,
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
