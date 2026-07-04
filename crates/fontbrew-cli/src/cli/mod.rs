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
    FontbrewError, InfoRequest, InstallBatchReport, InstallPlan, InstallReport, InstallRequest,
    InstallSource, OutdatedRequest, PackageId, RemoveRequest, SearchRequest, UpdateRequest,
};

use crate::{
    confirm::{ConfirmationOptions, Confirmer, HumanConfirmer, JsonConfirmer},
    exit::{self, CliError, CliResult},
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
    /// Search installable registry and provider packages.
    Search(SearchArgs),
    /// Check managed packages for available updates.
    Outdated(OutdatedArgs),
    /// Update managed packages.
    Update(UpdateArgs),
    /// Read and update Fontbrew configuration.
    Config(ConfigArgs),
    /// Manage the local first-party registry snapshot.
    Registry(RegistryArgs),
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
        help = "Source to install: registry name, provider:id, owner/repo, or local archive.",
        long_help = "Source to install: registry name, provider:id, owner/repo, or local archive. Google Fonts sources use google:<id>, require GOOGLE_FONTS_API_KEY from the environment, and the key is never stored in config."
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
        long = "all-families",
        conflicts_with = "families",
        help = "Install every font family discovered in a multi-family source"
    )]
    all_families: bool,

    #[arg(long, help = "Prefer OTF files")]
    otf: bool,

    #[arg(long, help = "Prefer TTF files")]
    ttf: bool,
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
        help = "Query or provider:id to search.",
        long_help = "Query or provider:id to search. Google Fonts provider searches use google:<id>, require GOOGLE_FONTS_API_KEY from the environment, and the key is never stored in config."
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
struct RegistryArgs {
    #[command(subcommand)]
    command: RegistryCommand,
}

#[derive(Debug, Subcommand)]
enum RegistryCommand {
    /// Refresh the local registry metadata snapshot.
    Update,
    /// Show local registry snapshot status.
    Status,
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

pub fn run(cli: Cli) -> u8 {
    let app = FontbrewApp::new();
    let cancellation = CliCancellation::new();
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
            &cancellation,
        );
    }

    let mut reporter = HumanReporter::new(cli.quiet, cli.verbose > 0);
    let mut confirmer = HumanConfirmer::new();

    run_with_reporter(
        cli.command,
        &app,
        &mut reporter,
        &mut confirmer,
        &cancellation,
    )
}

fn run_with_reporter(
    command: Command,
    app: &FontbrewApp,
    reporter: &mut dyn Reporter,
    confirmer: &mut dyn Confirmer,
    cancellation: &dyn CancellationToken,
) -> u8 {
    match execute(command, app, reporter, confirmer, cancellation) {
        Ok(()) => exit::SUCCESS,
        Err(error) => {
            let exit_code = error.exit_code();
            let _ = reporter.render_error(&error);
            exit_code
        }
    }
}

fn execute(
    command: Command,
    app: &FontbrewApp,
    reporter: &mut dyn Reporter,
    confirmer: &mut dyn Confirmer,
    cancellation: &dyn CancellationToken,
) -> CliResult<()> {
    match command {
        Command::Install(args) => install(args, app, reporter, confirmer, cancellation),
        Command::List => list(app, reporter),
        Command::Info(args) => info(args, app, reporter),
        Command::Remove(args) => remove(args, app, reporter, confirmer, cancellation),
        Command::Search(args) => search(args, app, reporter),
        Command::Outdated(args) => outdated(args, app, reporter),
        Command::Update(args) => update(args, app, reporter, confirmer, cancellation),
        Command::Config(args) => config(args, app, reporter),
        Command::Registry(args) => registry(args, app, reporter),
        Command::SelfUpdate(args) => run_self_update(args, reporter, confirmer, cancellation),
    }
}

fn install(
    args: InstallArgs,
    app: &FontbrewApp,
    reporter: &mut dyn Reporter,
    confirmer: &mut dyn Confirmer,
    cancellation: &dyn CancellationToken,
) -> CliResult<()> {
    let explicit_families = selected_family_args(&args.families);
    if !explicit_families.is_empty() {
        let plans =
            build_family_install_plans(&args, explicit_families, app, reporter, cancellation)?;
        let reports = apply_install_plans(&args, plans, app, reporter, confirmer, cancellation)?;
        return render_install_reports(reporter, reports);
    }

    let request = install_request_from_args(&args, Vec::new())?;
    let plan_result = build_install_plan(request, app, reporter, cancellation);
    let plan = match plan_result {
        Ok(plan) => plan,
        Err(CliError::Core(FontbrewError::FamilySelectionRequired { families })) => {
            let selected_families = if args.all_families {
                families
            } else {
                confirmer.select_families(&families)?
            };
            let plans =
                build_family_install_plans(&args, selected_families, app, reporter, cancellation)?;
            let reports =
                apply_install_plans(&args, plans, app, reporter, confirmer, cancellation)?;
            return render_install_reports(reporter, reports);
        }
        Err(error) => return Err(error),
    };

    let reports = apply_install_plans(&args, vec![plan], app, reporter, confirmer, cancellation)?;
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

fn build_family_install_plans(
    args: &InstallArgs,
    selected_families: Vec<FamilyName>,
    app: &FontbrewApp,
    reporter: &mut dyn Reporter,
    cancellation: &dyn CancellationToken,
) -> CliResult<Vec<InstallPlan>> {
    let mut plans = Vec::new();

    for family in selected_families {
        let request = install_request_from_args(args, vec![family])?;
        match build_install_plan(request, app, reporter, cancellation) {
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

fn build_install_plan(
    request: InstallRequest,
    app: &FontbrewApp,
    reporter: &mut dyn Reporter,
    cancellation: &dyn CancellationToken,
) -> CliResult<InstallPlan> {
    let mut progress = ProgressAdapter::new(reporter);
    let plan =
        app.install_plan_with_progress_and_cancellation(request, &mut progress, cancellation)?;
    progress.finish()?;

    Ok(plan)
}

fn apply_install_plans(
    args: &InstallArgs,
    mut plans: Vec<InstallPlan>,
    app: &FontbrewApp,
    reporter: &mut dyn Reporter,
    confirmer: &mut dyn Confirmer,
    cancellation: &dyn CancellationToken,
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
            let mut progress = ProgressAdapter::new(reporter);
            let report = app.apply_install(plan, policy.clone(), &mut progress, cancellation)?;
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

fn list(app: &FontbrewApp, reporter: &mut dyn Reporter) -> CliResult<()> {
    let report = app.list_packages()?;

    reporter.render_list_report(report)
}

fn info(args: InfoArgs, app: &FontbrewApp, reporter: &mut dyn Reporter) -> CliResult<()> {
    let package_id = PackageId::parse(args.package_id)?;
    let report = app.package_info(InfoRequest { package_id })?;

    reporter.render_info_report(report)
}

fn remove(
    args: RemoveArgs,
    app: &FontbrewApp,
    reporter: &mut dyn Reporter,
    confirmer: &mut dyn Confirmer,
    cancellation: &dyn CancellationToken,
) -> CliResult<()> {
    let package_id = PackageId::parse(args.package_id)?;
    let plan = app.remove_plan_with_cancellation(RemoveRequest { package_id }, cancellation)?;
    let policy = confirmer.execution_policy(
        &plan.risks,
        ConfirmationOptions {
            assume_yes: args.yes,
            dry_run: args.dry_run,
        },
    )?;
    let report = {
        let mut progress = ProgressAdapter::new(reporter);
        let report = app.apply_remove(plan, policy, &mut progress, cancellation)?;
        progress.finish()?;
        report
    };

    reporter.render_remove_report(report)
}

fn search(args: SearchArgs, app: &FontbrewApp, reporter: &mut dyn Reporter) -> CliResult<()> {
    let report = app.search(SearchRequest {
        query: args.query.unwrap_or_default(),
        limit: args.limit,
    })?;

    reporter.render_search_report(report)
}

fn outdated(args: OutdatedArgs, app: &FontbrewApp, reporter: &mut dyn Reporter) -> CliResult<()> {
    let package_ids = args
        .package_ids
        .into_iter()
        .map(PackageId::parse)
        .collect::<fontbrew_core::Result<Vec<_>>>()?;
    let report = app.outdated(OutdatedRequest { package_ids })?;

    reporter.render_outdated_report(report)
}

fn update(
    args: UpdateArgs,
    app: &FontbrewApp,
    reporter: &mut dyn Reporter,
    confirmer: &mut dyn Confirmer,
    cancellation: &dyn CancellationToken,
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
    let report = {
        let mut progress = ProgressAdapter::new(reporter);
        let plan = app.update_plan(request, &mut progress, cancellation)?;
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
        let report = app.apply_update(plan, policy, &mut progress, cancellation)?;
        progress.finish()?;
        report
    };

    reporter.render_update_report(report)
}

fn config(args: ConfigArgs, app: &FontbrewApp, reporter: &mut dyn Reporter) -> CliResult<()> {
    match args.command {
        ConfigCommand::Get(args) => {
            let report = app.config_get(ConfigGetRequest { key: args.key })?;
            reporter.render_config_get_report(report)
        }
        ConfigCommand::Set(args) => {
            let report = app.config_set(ConfigSetRequest {
                key: args.key,
                value: args.value,
            })?;
            reporter.render_config_set_report(report)
        }
    }
}

fn registry(args: RegistryArgs, app: &FontbrewApp, reporter: &mut dyn Reporter) -> CliResult<()> {
    match args.command {
        RegistryCommand::Update => {
            let report = app.registry_update()?;
            reporter.render_registry_update_report(report)
        }
        RegistryCommand::Status => {
            let report = app.registry_status()?;
            reporter.render_registry_status_report(report)
        }
    }
}

fn run_self_update(
    args: SelfUpdateArgs,
    reporter: &mut dyn Reporter,
    confirmer: &mut dyn Confirmer,
    cancellation: &dyn CancellationToken,
) -> CliResult<()> {
    let request = SelfUpdateRequest::from_environment(args.dry_run, args.yes, args.force)?;

    self_update_command::run(request, reporter, confirmer, cancellation)
}

fn install_source_from_arg(source: &str) -> InstallSource {
    let path = PathBuf::from(source);

    if path.exists() || looks_like_explicit_local_path(source) {
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
        InstallSource::RegistryName(source.to_string())
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
        push_unique_format(
            &mut formats,
            match format {
                CliFontFormat::Otf => FontFormat::Otf,
                CliFontFormat::Ttf => FontFormat::Ttf,
                CliFontFormat::Ttc => FontFormat::Ttc,
                CliFontFormat::Otc => FontFormat::Otc,
            },
        );
    }

    if args.otf {
        push_unique_format(&mut formats, FontFormat::Otf);
    }

    if args.ttf {
        push_unique_format(&mut formats, FontFormat::Ttf);
    }

    formats
}

fn push_unique_format(formats: &mut Vec<FontFormat>, format: FontFormat) {
    if !formats.contains(&format) {
        formats.push(format);
    }
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
    fn cli_help_documents_google_fonts_api_key_environment_variable() {
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
            assert!(help.contains("google:<id>"));
            assert!(help.contains("GOOGLE_FONTS_API_KEY"));
            assert!(help.contains("environment"));
            assert!(help.contains("never stored in config"));
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
    fn install_source_parses_google_prefix_as_provider_source() {
        let source = install_source_from_arg("google:source-sans-3");

        assert_eq!(
            source,
            InstallSource::Provider {
                provider: fontbrew_core::ProviderKind::Google,
                id: "source-sans-3".to_string(),
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
            otf: true,
            ttf: true,
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
            otf: false,
            ttf: false,
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
                key: "registry.url".to_string(),
            }),
        })
        .consumes_cancellation());
        assert!(!Command::Config(ConfigArgs {
            command: ConfigCommand::Set(ConfigSetArgs {
                key: "registry.url".to_string(),
                value: "https://example.test/registry.json".to_string(),
            }),
        })
        .consumes_cancellation());
        assert!(!Command::Registry(RegistryArgs {
            command: RegistryCommand::Update,
        })
        .consumes_cancellation());
        assert!(!Command::Registry(RegistryArgs {
            command: RegistryCommand::Status,
        })
        .consumes_cancellation());
    }
}
