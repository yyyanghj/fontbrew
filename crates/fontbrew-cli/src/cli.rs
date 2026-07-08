use std::{
    collections::BTreeSet,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use clap::{ArgAction, Args, Parser, Subcommand, ValueEnum};
use fontbrew_core::{
    sources::{GitHubRepo, ProviderSource},
    CancellationToken, ConfigGetRequest, ConfigSetRequest, FamilyName, FontFormat, FontbrewApp,
    InfoRequest, InstallBatchReport, InstallPlan, InstallPreparation, InstallReport,
    InstallRequest, InstallSource, OutdatedRequest, PackageId, PendingFamilySelection,
    RemoveRequest, SearchRequest, UpdateRequest,
};

use crate::{
    confirm::{ConfirmationOptions, Confirmer, HumanConfirmer, JsonConfirmer},
    exit::{self, CliResult},
    progress::ProgressAdapter,
    reporter::{human::HumanReporter, json::JsonReporter, Reporter},
    self_update::{self as self_update_command, SelfUpdateRequest},
};

#[derive(Debug, Parser)]
#[command(
    name = "fontbrew",
    version,
    about = "Manage third-party open-source fonts on macOS"
)]
pub struct Cli {
    #[arg(long, global = true, help = "Write machine-readable JSON to stdout")]
    json: bool,

    #[arg(long, global = true, help = "Suppress progress and warning output")]
    quiet: bool,

    #[arg(
        short,
        long,
        global = true,
        action = ArgAction::Count,
        conflicts_with = "quiet",
        help = "Increase diagnostic verbosity"
    )]
    verbose: u8,

    #[arg(long = "no-color", global = true, help = "Disable color output")]
    _no_color: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Install and activate a package from a source.
    Install(InstallArgs),
    /// List managed packages.
    List,
    /// Show details for a managed package.
    Info(InfoArgs),
    /// Remove a managed package.
    #[command(alias = "uninstall")]
    Remove(RemoveArgs),
    /// Search installable Fontsource packages.
    Search(SearchArgs),
    /// Check managed packages for available updates.
    Outdated(OutdatedArgs),
    /// Update managed packages.
    Update(UpdateArgs),
    /// Read and update Fontbrew configuration.
    Config(ConfigArgs),
    /// Update the fontbrew CLI binary to the latest stable release.
    SelfUpdate(SelfUpdateArgs),
}

impl Command {
    fn consumes_cancellation(&self) -> bool {
        matches!(
            self,
            Command::Install(_) | Command::Remove(_) | Command::Update(_) | Command::SelfUpdate(_)
        )
    }
}

#[derive(Debug, Args)]
struct InstallArgs {
    #[arg(
        help = "Source to install: Fontsource id, fontsource:<id>, owner/repo, or local archive.",
        long_help = "Source to install: Fontsource id, fontsource:<id>, owner/repo, or local archive. Unprefixed names are exact Fontsource package IDs."
    )]
    source: String,

    #[arg(long, help = "Reinstall an already managed package")]
    reinstall: bool,

    #[arg(long, help = "Assume yes for approval prompts")]
    yes: bool,

    #[arg(long, help = "Build the install plan without applying changes")]
    dry_run: bool,

    #[arg(
        long = "id",
        conflicts_with_all = ["families", "all_families"],
        help = "Package ID override for local archive sources"
    )]
    package_id: Option<String>,

    #[arg(long = "asset", help = "Select a release asset by name or pattern")]
    asset_selector: Option<String>,

    #[arg(long = "format", value_enum, help = "Preferred desktop font format")]
    format_preference: Vec<CliFontFormat>,

    #[arg(
        long = "family",
        value_name = "NAME",
        conflicts_with = "all_families",
        help = "Select a font family from a multi-family source; may be repeated"
    )]
    families: Vec<String>,

    #[arg(
        short = 'a',
        long = "all",
        conflicts_with = "families",
        help = "Install every font family discovered in a multi-family source"
    )]
    all_families: bool,
}

#[derive(Debug, Args)]
struct InfoArgs {
    package_id: String,
}

#[derive(Debug, Args)]
struct RemoveArgs {
    package_id: String,

    #[arg(long, help = "Assume yes for approval prompts")]
    yes: bool,

    #[arg(long, help = "Build the remove plan without applying changes")]
    dry_run: bool,
}

#[derive(Debug, Args)]
struct SearchArgs {
    #[arg(
        help = "Query or fontsource:<id> to search.",
        long_help = "Query or fontsource:<id> to search. Search uses Fontsource package metadata."
    )]
    query: Option<String>,

    #[arg(long, help = "Maximum number of results to return")]
    limit: Option<usize>,
}

#[derive(Debug, Args)]
struct OutdatedArgs {
    package_ids: Vec<String>,
}

#[derive(Debug, Args)]
struct UpdateArgs {
    package_ids: Vec<String>,

    #[arg(long, help = "Assume yes for approval prompts")]
    yes: bool,

    #[arg(long, help = "Prepare the update plan without applying changes")]
    dry_run: bool,

    #[arg(long, help = "Maximum number of concurrent prepare jobs")]
    jobs: Option<usize>,
}

#[derive(Debug, Args)]
struct ConfigArgs {
    #[command(subcommand)]
    command: ConfigCommand,
}

#[derive(Debug, Subcommand)]
enum ConfigCommand {
    /// Print a known config key.
    Get(ConfigGetArgs),
    /// Persist a known config key.
    Set(ConfigSetArgs),
}

#[derive(Debug, Args)]
struct ConfigGetArgs {
    key: String,
}

#[derive(Debug, Args)]
struct ConfigSetArgs {
    key: String,
    value: String,
}

#[derive(Debug, Args)]
struct SelfUpdateArgs {
    #[arg(
        long,
        help = "Check the latest release without replacing the executable"
    )]
    dry_run: bool,

    #[arg(long, help = "Assume yes for the replacement prompt")]
    yes: bool,

    #[arg(
        long,
        help = "Reinstall latest stable even when current is latest or newer"
    )]
    force: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CliFontFormat {
    Otf,
    Ttf,
    Ttc,
    Otc,
}

pub async fn run(cli: Cli) -> u8 {
    let app = FontbrewApp::new();
    let cancellation = Arc::new(CliCancellation::new());
    if cli.command.consumes_cancellation() {
        let _ = cancellation.install_ctrlc_handler();
    }

    if cli.json {
        let mut reporter = JsonReporter::new();
        let mut confirmer = JsonConfirmer::new();
        return run_with_reporter(
            cli.command,
            &app,
            &mut reporter,
            &mut confirmer,
            cancellation.clone(),
        )
        .await;
    }

    let mut reporter = HumanReporter::new(cli.quiet, cli.verbose > 0);
    let mut confirmer = HumanConfirmer::new();

    run_with_reporter(
        cli.command,
        &app,
        &mut reporter,
        &mut confirmer,
        cancellation.clone(),
    )
    .await
}

async fn run_with_reporter(
    command: Command,
    app: &FontbrewApp,
    reporter: &mut dyn Reporter,
    confirmer: &mut dyn Confirmer,
    cancellation: Arc<dyn CancellationToken>,
) -> u8 {
    match execute(command, app, reporter, confirmer, cancellation).await {
        Ok(()) => exit::SUCCESS,
        Err(error) => {
            let exit_code = error.exit_code();
            let _ = reporter.render_error(&error);
            exit_code
        }
    }
}

async fn execute(
    command: Command,
    app: &FontbrewApp,
    reporter: &mut dyn Reporter,
    confirmer: &mut dyn Confirmer,
    cancellation: Arc<dyn CancellationToken>,
) -> CliResult<()> {
    match command {
        Command::Install(args) => install(args, app, reporter, confirmer, cancellation).await,
        Command::List => list(app, reporter).await,
        Command::Info(args) => info(args, app, reporter).await,
        Command::Remove(args) => remove(args, app, reporter, confirmer, cancellation).await,
        Command::Search(args) => search(args, app, reporter).await,
        Command::Outdated(args) => outdated(args, app, reporter).await,
        Command::Update(args) => update(args, app, reporter, confirmer, cancellation).await,
        Command::Config(args) => config(args, app, reporter).await,
        Command::SelfUpdate(args) => run_self_update(args, reporter, confirmer, cancellation).await,
    }
}

async fn install(
    args: InstallArgs,
    app: &FontbrewApp,
    reporter: &mut dyn Reporter,
    confirmer: &mut dyn Confirmer,
    cancellation: Arc<dyn CancellationToken>,
) -> CliResult<()> {
    let explicit_families = selected_family_args(&args.families);
    if !explicit_families.is_empty() {
        let plans = build_family_install_plans(
            &args,
            explicit_families,
            app,
            reporter,
            cancellation.clone(),
        )
        .await?;
        let reports =
            apply_install_plans(&args, plans, app, reporter, confirmer, cancellation).await?;
        return render_install_reports(reporter, reports);
    }

    let request = install_request_from_args(&args, Vec::new())?;
    let preparation = prepare_install(request, app, reporter, cancellation.clone()).await?;
    let plan = match preparation {
        InstallPreparation::Plan(plan) => plan,
        InstallPreparation::FamilySelection(pending) => {
            let selected_families = if args.all_families {
                pending.families().to_vec()
            } else {
                confirmer.select_families(pending.families())?
            };
            let plans = prepare_selected_families(
                pending,
                selected_families,
                app,
                reporter,
                cancellation.clone(),
            )
            .await?;
            let reports =
                apply_install_plans(&args, plans, app, reporter, confirmer, cancellation).await?;
            return render_install_reports(reporter, reports);
        }
    };

    let reports =
        apply_install_plans(&args, vec![plan], app, reporter, confirmer, cancellation).await?;
    render_install_reports(reporter, reports)
}

fn install_request_from_args(
    args: &InstallArgs,
    selected_families: Vec<FamilyName>,
) -> CliResult<InstallRequest> {
    Ok(InstallRequest {
        source: install_source_from_arg(&args.source),
        package_id_override: args
            .package_id
            .as_deref()
            .map(PackageId::parse)
            .transpose()?,
        format_preference: font_format_preference(args),
        asset_selector: args.asset_selector.clone(),
        selected_families,
        reinstall: args.reinstall,
    })
}

fn selected_family_args(families: &[String]) -> Vec<FamilyName> {
    let mut seen = BTreeSet::new();
    let mut selected_families = Vec::new();

    for family in families {
        let trimmed = family.trim();
        if trimmed.is_empty() {
            continue;
        }

        let normalized = trimmed
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_ascii_lowercase();
        if seen.insert(normalized) {
            selected_families.push(FamilyName::new(trimmed.to_string()));
        }
    }

    selected_families
}

async fn build_family_install_plans(
    args: &InstallArgs,
    selected_families: Vec<FamilyName>,
    app: &FontbrewApp,
    reporter: &mut dyn Reporter,
    cancellation: Arc<dyn CancellationToken>,
) -> CliResult<Vec<InstallPlan>> {
    if selected_families.len() > 1 {
        let request = install_request_from_args(args, selected_families)?;
        return build_install_plans(request, app, reporter, cancellation).await;
    }

    let mut plans = Vec::new();

    for family in selected_families {
        let request = install_request_from_args(args, vec![family])?;
        match build_install_plan(request, app, reporter, cancellation.clone()).await {
            Ok(plan) => plans.push(plan),
            Err(error) => {
                for plan in plans {
                    app.discard_install_plan(plan);
                }
                return Err(error);
            }
        }
    }

    Ok(plans)
}

async fn prepare_install(
    request: InstallRequest,
    app: &FontbrewApp,
    reporter: &mut dyn Reporter,
    cancellation: Arc<dyn CancellationToken>,
) -> CliResult<InstallPreparation> {
    reporter.start_activity(&install_plan_activity_message(&request))?;
    let mut progress = ProgressAdapter::new(reporter);
    let preparation = app
        .prepare_install(request, &mut progress, cancellation)
        .await?;
    progress.finish()?;

    Ok(preparation)
}

async fn prepare_selected_families(
    pending: PendingFamilySelection,
    selected_families: Vec<FamilyName>,
    app: &FontbrewApp,
    reporter: &mut dyn Reporter,
    cancellation: Arc<dyn CancellationToken>,
) -> CliResult<Vec<InstallPlan>> {
    reporter.start_activity(&format!(
        "Preparing install plan for {}",
        family_activity_label(&selected_families)
    ))?;
    let mut progress = ProgressAdapter::new(reporter);
    let plans = app
        .prepare_selected_families(pending, selected_families, &mut progress, cancellation)
        .await?;
    progress.finish()?;

    Ok(plans)
}

async fn build_install_plans(
    request: InstallRequest,
    app: &FontbrewApp,
    reporter: &mut dyn Reporter,
    cancellation: Arc<dyn CancellationToken>,
) -> CliResult<Vec<InstallPlan>> {
    reporter.start_activity(&install_plan_activity_message(&request))?;
    let mut progress = ProgressAdapter::new(reporter);
    let plans = app
        .install_plans_with_progress_and_cancellation(request, &mut progress, cancellation)
        .await?;
    progress.finish()?;

    Ok(plans)
}

async fn build_install_plan(
    request: InstallRequest,
    app: &FontbrewApp,
    reporter: &mut dyn Reporter,
    cancellation: Arc<dyn CancellationToken>,
) -> CliResult<InstallPlan> {
    reporter.start_activity(&install_plan_activity_message(&request))?;
    let mut progress = ProgressAdapter::new(reporter);
    let plan = app
        .install_plan_with_progress_and_cancellation(request, &mut progress, cancellation)
        .await?;
    progress.finish()?;

    Ok(plan)
}

fn install_plan_activity_message(request: &InstallRequest) -> String {
    let target = if request.selected_families.is_empty() {
        install_source_label(&request.source)
    } else {
        family_activity_label(&request.selected_families)
    };

    format!("Preparing install plan for {target}")
}

fn family_activity_label(families: &[FamilyName]) -> String {
    families
        .iter()
        .map(|family| family.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn install_source_label(source: &InstallSource) -> String {
    match source {
        InstallSource::LocalPath(path) => path.display().to_string(),
        InstallSource::GitHubRepo { owner, repo } => format!("{owner}/{repo}"),
        InstallSource::Provider {
            provider: fontbrew_core::ProviderKind::Fontsource,
            id,
        } => format!("fontsource:{id}"),
    }
}

async fn apply_install_plans(
    args: &InstallArgs,
    mut plans: Vec<InstallPlan>,
    app: &FontbrewApp,
    reporter: &mut dyn Reporter,
    confirmer: &mut dyn Confirmer,
    cancellation: Arc<dyn CancellationToken>,
) -> CliResult<Vec<InstallReport>> {
    let risks = plans
        .iter()
        .flat_map(|plan| plan.risks.iter().cloned())
        .collect::<Vec<_>>();
    let policy = match confirmer.execution_policy(
        &risks,
        ConfirmationOptions {
            assume_yes: args.yes,
            dry_run: args.dry_run,
        },
    ) {
        Ok(policy) => policy,
        Err(error) => {
            for plan in plans {
                app.discard_install_plan(plan);
            }
            return Err(error);
        }
    };

    plans.reverse();
    let mut reports = Vec::new();
    while let Some(plan) = plans.pop() {
        let result: CliResult<InstallReport> = {
            reporter.start_activity(&format!("Applying install {}", plan.package_id.as_str()))?;
            let mut progress = ProgressAdapter::new(reporter);
            let report = app
                .apply_install(plan, policy.clone(), &mut progress, cancellation.clone())
                .await?;
            progress.finish()?;
            Ok(report)
        };
        match result {
            Ok(report) => reports.push(report),
            Err(error) => {
                for plan in plans {
                    app.discard_install_plan(plan);
                }
                return Err(error);
            }
        };
    }

    Ok(reports)
}

fn render_install_reports(
    reporter: &mut dyn Reporter,
    mut reports: Vec<InstallReport>,
) -> CliResult<()> {
    if reports.len() == 1 {
        return reporter.render_install_report(reports.remove(0));
    }

    reporter.render_install_batch_report(InstallBatchReport { packages: reports })
}

async fn list(app: &FontbrewApp, reporter: &mut dyn Reporter) -> CliResult<()> {
    let report = app.list_packages().await?;

    reporter.render_list_report(report)
}

async fn info(args: InfoArgs, app: &FontbrewApp, reporter: &mut dyn Reporter) -> CliResult<()> {
    let package_id = PackageId::parse(args.package_id)?;
    let report = app.package_info(InfoRequest { package_id }).await?;

    reporter.render_info_report(report)
}

async fn remove(
    args: RemoveArgs,
    app: &FontbrewApp,
    reporter: &mut dyn Reporter,
    confirmer: &mut dyn Confirmer,
    cancellation: Arc<dyn CancellationToken>,
) -> CliResult<()> {
    let package_id = PackageId::parse(args.package_id)?;
    reporter.start_activity(&format!("Planning removal {}", package_id.as_str()))?;
    let plan_result = app
        .remove_plan_with_cancellation(
            RemoveRequest {
                package_id: package_id.clone(),
            },
            cancellation.clone(),
        )
        .await;
    reporter.finish_activity()?;
    let plan = plan_result?;
    let policy = confirmer.execution_policy(
        &plan.risks,
        ConfirmationOptions {
            assume_yes: args.yes,
            dry_run: args.dry_run,
        },
    )?;
    let report = {
        reporter.start_activity(&format!("Removing {}", plan.package_id.as_str()))?;
        let mut progress = ProgressAdapter::new(reporter);
        let report = app
            .apply_remove(plan, policy, &mut progress, cancellation)
            .await?;
        progress.finish()?;
        report
    };

    reporter.render_remove_report(report)
}

async fn search(args: SearchArgs, app: &FontbrewApp, reporter: &mut dyn Reporter) -> CliResult<()> {
    reporter.start_activity("Searching packages")?;
    let report_result = app
        .search(SearchRequest {
            query: args.query.unwrap_or_default(),
            limit: args.limit,
        })
        .await;
    reporter.finish_activity()?;
    let report = report_result?;

    reporter.render_search_report(report)
}

async fn outdated(
    args: OutdatedArgs,
    app: &FontbrewApp,
    reporter: &mut dyn Reporter,
) -> CliResult<()> {
    let package_ids = args
        .package_ids
        .into_iter()
        .map(PackageId::parse)
        .collect::<fontbrew_core::Result<Vec<_>>>()?;
    reporter.start_activity("Checking for updates")?;
    let report_result = app.outdated(OutdatedRequest { package_ids }).await;
    reporter.finish_activity()?;
    let report = report_result?;

    reporter.render_outdated_report(report)
}

async fn update(
    args: UpdateArgs,
    app: &FontbrewApp,
    reporter: &mut dyn Reporter,
    confirmer: &mut dyn Confirmer,
    cancellation: Arc<dyn CancellationToken>,
) -> CliResult<()> {
    let package_ids = args
        .package_ids
        .into_iter()
        .map(PackageId::parse)
        .collect::<fontbrew_core::Result<Vec<_>>>()?;
    let request = UpdateRequest {
        package_ids,
        jobs: args.jobs,
    };
    let plan = {
        reporter.start_activity("Preparing updates")?;
        let mut progress = ProgressAdapter::new(reporter);
        let plan = app
            .update_plan(request, &mut progress, cancellation.clone())
            .await?;
        progress.finish()?;
        plan
    };
    let report = {
        let policy = match confirmer.execution_policy(
            &plan.risks,
            ConfirmationOptions {
                assume_yes: args.yes,
                dry_run: args.dry_run,
            },
        ) {
            Ok(policy) => policy,
            Err(error) => {
                app.discard_update_plan(plan);
                return Err(error);
            }
        };
        reporter.start_activity("Applying updates")?;
        let mut progress = ProgressAdapter::new(reporter);
        let report = app
            .apply_update(plan, policy, &mut progress, cancellation)
            .await?;
        progress.finish()?;
        report
    };

    reporter.render_update_report(report)
}

async fn config(args: ConfigArgs, app: &FontbrewApp, reporter: &mut dyn Reporter) -> CliResult<()> {
    match args.command {
        ConfigCommand::Get(args) => {
            let report = app.config_get(ConfigGetRequest { key: args.key }).await?;
            reporter.render_config_get_report(report)
        }
        ConfigCommand::Set(args) => {
            let report = app
                .config_set(ConfigSetRequest {
                    key: args.key,
                    value: args.value,
                })
                .await?;
            reporter.render_config_set_report(report)
        }
    }
}

async fn run_self_update(
    args: SelfUpdateArgs,
    reporter: &mut dyn Reporter,
    confirmer: &mut dyn Confirmer,
    cancellation: Arc<dyn CancellationToken>,
) -> CliResult<()> {
    let request = SelfUpdateRequest::from_environment(args.dry_run, args.yes, args.force)?;

    self_update_command::run(request, reporter, confirmer, cancellation).await
}

fn install_source_from_arg(source: &str) -> InstallSource {
    let path = PathBuf::from(source);

    if looks_like_explicit_local_path(source) {
        InstallSource::LocalPath(path)
    } else if let Some(provider_source) = ProviderSource::parse_prefixed(source) {
        InstallSource::Provider {
            provider: provider_source.provider,
            id: provider_source.id,
        }
    } else if let Ok(repo) = GitHubRepo::parse(source) {
        InstallSource::GitHubRepo {
            owner: repo.owner,
            repo: repo.repo,
        }
    } else if looks_like_invalid_local_path(source) {
        InstallSource::LocalPath(path)
    } else {
        InstallSource::Provider {
            provider: fontbrew_core::ProviderKind::Fontsource,
            id: source.to_string(),
        }
    }
}

fn looks_like_explicit_local_path(source: &str) -> bool {
    source.starts_with('.')
        || source.starts_with('/')
        || source.contains('\\')
        || source.ends_with(".zip")
}

fn looks_like_invalid_local_path(source: &str) -> bool {
    source.contains('/') || source.contains('\\')
}

fn font_format_preference(args: &InstallArgs) -> Vec<FontFormat> {
    let mut formats = Vec::new();

    for format in &args.format_preference {
        let format = match format {
            CliFontFormat::Otf => FontFormat::Otf,
            CliFontFormat::Ttf => FontFormat::Ttf,
            CliFontFormat::Ttc => FontFormat::Ttc,
            CliFontFormat::Otc => FontFormat::Otc,
        };

        if !formats.contains(&format) {
            formats.push(format);
        }
    }

    formats
}

#[derive(Clone)]
struct CliCancellation {
    cancelled: Arc<AtomicBool>,
}

impl CliCancellation {
    fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    fn install_ctrlc_handler(&self) -> Result<(), ctrlc::Error> {
        let cancelled = self.cancelled.clone();
        ctrlc::set_handler(move || {
            cancelled.store(true, Ordering::SeqCst);
        })
    }

    #[cfg(test)]
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }
}

impl CancellationToken for CliCancellation {
    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_help_documents_fontsource_sources() {
        let mut install_command = Cli::command();
        let install_help = install_command
            .find_subcommand_mut("install")
            .expect("install subcommand")
            .render_long_help()
            .to_string();
        let mut search_command = Cli::command();
        let search_help = search_command
            .find_subcommand_mut("search")
            .expect("search subcommand")
            .render_long_help()
            .to_string();

        for help in [install_help, search_help] {
            assert!(help.contains("fontsource:<id>"));
            assert!(help.contains("Fontsource"));
        }
    }

    #[test]
    fn cli_help_does_not_expose_manual_refresh_or_offline_flags() {
        for command_name in ["install", "search", "outdated"] {
            let mut command = Cli::command();
            let help = command
                .find_subcommand_mut(command_name)
                .expect("subcommand")
                .render_long_help()
                .to_string();

            assert!(!help.contains("--refresh"));
            assert!(!help.contains("--offline"));
        }
    }

    #[test]
    fn install_help_documents_local_archive_package_id_override() {
        let mut command = Cli::command();
        let help = command
            .find_subcommand_mut("install")
            .expect("install subcommand")
            .render_long_help()
            .to_string();

        assert!(help.contains("--id"));
        assert!(help.contains("local archive"));
    }

    #[test]
    fn install_help_exposes_current_install_flags_only() {
        let mut command = Cli::command();
        let help = command
            .find_subcommand_mut("install")
            .expect("install subcommand")
            .render_long_help()
            .to_string();

        assert!(help.contains("-a"));
        assert!(help.contains("--all"));
        assert!(help.contains("--format"));
        assert!(!help.contains("--all-families"));
        assert!(!help.contains("--otf"));
        assert!(!help.contains("--ttf"));
    }

    #[test]
    fn install_all_flag_accepts_short_and_long_forms() {
        for flag in ["--all", "-a"] {
            let cli = Cli::try_parse_from(["fontbrew", "install", "source-code-pro", flag])
                .expect("parse install all flag");
            let Command::Install(args) = cli.command else {
                panic!("expected install command");
            };

            assert!(args.all_families);
        }
    }

    #[test]
    fn install_legacy_flags_are_not_accepted() {
        for flag in ["--all-families", "--otf", "--ttf"] {
            assert!(Cli::try_parse_from(["fontbrew", "install", "source-code-pro", flag]).is_err());
        }
    }

    #[test]
    fn install_source_parses_owner_repo_as_github_repo() {
        let source = install_source_from_arg("adobe/source-code-pro");

        assert_eq!(
            source,
            InstallSource::GitHubRepo {
                owner: "adobe".to_string(),
                repo: "source-code-pro".to_string(),
            }
        );
    }

    #[test]
    fn install_source_parses_fontsource_prefix_as_provider_source() {
        let source = install_source_from_arg("fontsource:abel");

        assert_eq!(
            source,
            InstallSource::Provider {
                provider: fontbrew_core::ProviderKind::Fontsource,
                id: "abel".to_string(),
            }
        );
    }

    #[test]
    fn install_source_parses_unprefixed_name_as_fontsource_source() {
        let source = install_source_from_arg("source-sans-3");

        assert_eq!(
            source,
            InstallSource::Provider {
                provider: fontbrew_core::ProviderKind::Fontsource,
                id: "source-sans-3".to_string(),
            }
        );
    }

    #[test]
    fn install_source_keeps_bare_names_as_exact_fontsource_ids() {
        let source = install_source_from_arg("inter");

        assert_eq!(
            source,
            InstallSource::Provider {
                provider: fontbrew_core::ProviderKind::Fontsource,
                id: "inter".to_string(),
            }
        );
    }

    #[test]
    fn install_source_keeps_explicit_local_paths_local() {
        assert!(matches!(
            install_source_from_arg("./adobe/source-code-pro"),
            InstallSource::LocalPath(_)
        ));
        assert!(matches!(
            install_source_from_arg("downloads/fonts.zip"),
            InstallSource::LocalPath(_)
        ));
    }

    #[test]
    fn font_format_preference_deduplicates_cli_overrides_in_order() {
        let args = InstallArgs {
            source: "source-code-pro.zip".to_string(),
            reinstall: false,
            yes: false,
            dry_run: false,
            package_id: None,
            asset_selector: None,
            format_preference: vec![CliFontFormat::Otf, CliFontFormat::Ttf, CliFontFormat::Otf],
            families: Vec::new(),
            all_families: false,
        };

        assert_eq!(
            font_format_preference(&args),
            vec![FontFormat::Otf, FontFormat::Ttf]
        );
    }

    #[test]
    fn cli_cancellation_token_reflects_atomic_flag() {
        let cancellation = CliCancellation::new();

        assert!(!cancellation.is_cancelled());
        cancellation.cancel();
        assert!(cancellation.is_cancelled());
    }

    #[test]
    fn command_consumes_cancellation_only_for_write_operations_using_the_token() {
        assert!(Command::Install(InstallArgs {
            source: "source-code-pro".to_string(),
            reinstall: false,
            yes: false,
            dry_run: false,
            package_id: None,
            asset_selector: None,
            format_preference: Vec::new(),
            families: Vec::new(),
            all_families: false,
        })
        .consumes_cancellation());
        assert!(Command::Remove(RemoveArgs {
            package_id: "source-code-pro".to_string(),
            yes: false,
            dry_run: false,
        })
        .consumes_cancellation());
        assert!(Command::Update(UpdateArgs {
            package_ids: Vec::new(),
            yes: false,
            dry_run: false,
            jobs: None,
        })
        .consumes_cancellation());

        assert!(!Command::List.consumes_cancellation());
        assert!(!Command::Info(InfoArgs {
            package_id: "source-code-pro".to_string(),
        })
        .consumes_cancellation());
        assert!(!Command::Search(SearchArgs {
            query: None,
            limit: None,
        })
        .consumes_cancellation());
        assert!(!Command::Outdated(OutdatedArgs {
            package_ids: Vec::new(),
        })
        .consumes_cancellation());
        assert!(!Command::Config(ConfigArgs {
            command: ConfigCommand::Get(ConfigGetArgs {
                key: "network.metadata_ttl_hours".to_string(),
            }),
        })
        .consumes_cancellation());
        assert!(!Command::Config(ConfigArgs {
            command: ConfigCommand::Set(ConfigSetArgs {
                key: "network.metadata_ttl_hours".to_string(),
                value: "24".to_string(),
            }),
        })
        .consumes_cancellation());
    }
}
