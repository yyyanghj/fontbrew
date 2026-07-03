use std::path::PathBuf;

use clap::{ArgAction, Args, Parser, Subcommand, ValueEnum};
use fontbrew_core::{
    sources::GitHubRepo, CancellationToken, FontFormat, FontbrewApp, InfoRequest, InstallRequest,
    InstallSource, OutdatedRequest, PackageId, RemoveRequest, SearchRequest, UpdateRequest,
};

use crate::{
    confirm::{ConfirmationOptions, Confirmer, HumanConfirmer, JsonConfirmer},
    exit::{self, CliResult},
    progress::ProgressAdapter,
    reporter::{human::HumanReporter, json::JsonReporter, Reporter},
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
    /// Search packages in the local registry snapshot.
    Search(SearchArgs),
    /// Check managed packages for available updates.
    Outdated(OutdatedArgs),
    /// Update managed packages.
    Update(UpdateArgs),
    /// Manage the local first-party registry snapshot.
    Registry(RegistryArgs),
}

#[derive(Debug, Args)]
struct InstallArgs {
    /// Source to install. Local archive paths are supported in the MVP.
    source: String,

    #[arg(long, help = "Reinstall an already managed package")]
    reinstall: bool,

    #[arg(long, help = "Assume yes for approval prompts")]
    yes: bool,

    #[arg(long, help = "Build the install plan without applying changes")]
    dry_run: bool,

    #[arg(long, help = "Refresh source metadata before installing")]
    refresh: bool,

    #[arg(long, help = "Use local metadata and archives only")]
    offline: bool,

    #[arg(long = "asset", help = "Select a release asset by name or pattern")]
    asset_selector: Option<String>,

    #[arg(long = "format", value_enum, help = "Preferred desktop font format")]
    format_preference: Vec<CliFontFormat>,

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
    query: Option<String>,

    #[arg(long, help = "Maximum number of results to return")]
    limit: Option<usize>,

    #[arg(long, help = "Refresh registry metadata before searching")]
    refresh: bool,

    #[arg(long, help = "Use the local registry snapshot only")]
    offline: bool,
}

#[derive(Debug, Args)]
struct OutdatedArgs {
    package_ids: Vec<String>,

    #[arg(long, help = "Do not query GitHub release metadata")]
    offline: bool,
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

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CliFontFormat {
    Otf,
    Ttf,
    Ttc,
    Otc,
}

pub fn run(cli: Cli) -> u8 {
    let app = FontbrewApp::new();

    if cli.json {
        let mut reporter = JsonReporter::new();
        let mut confirmer = JsonConfirmer::new();
        return run_with_reporter(cli.command, &app, &mut reporter, &mut confirmer);
    }

    let mut reporter = HumanReporter::new(cli.quiet, cli.verbose > 0);
    let mut confirmer = HumanConfirmer::new();

    run_with_reporter(cli.command, &app, &mut reporter, &mut confirmer)
}

fn run_with_reporter(
    command: Command,
    app: &FontbrewApp,
    reporter: &mut dyn Reporter,
    confirmer: &mut dyn Confirmer,
) -> u8 {
    match execute(command, app, reporter, confirmer) {
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
) -> CliResult<()> {
    match command {
        Command::Install(args) => install(args, app, reporter, confirmer),
        Command::List => list(app, reporter),
        Command::Info(args) => info(args, app, reporter),
        Command::Remove(args) => remove(args, app, reporter, confirmer),
        Command::Search(args) => search(args, app, reporter),
        Command::Outdated(args) => outdated(args, app, reporter),
        Command::Update(args) => update(args, app, reporter, confirmer),
        Command::Registry(args) => registry(args, app, reporter),
    }
}

fn install(
    args: InstallArgs,
    app: &FontbrewApp,
    reporter: &mut dyn Reporter,
    confirmer: &mut dyn Confirmer,
) -> CliResult<()> {
    let request = InstallRequest {
        source: install_source_from_arg(&args.source),
        format_preference: font_format_preference(&args),
        asset_selector: args.asset_selector,
        reinstall: args.reinstall,
        refresh: args.refresh,
        offline: args.offline,
    };
    let plan = app.install_plan(request)?;
    let policy = confirmer.execution_policy(
        &plan.risks,
        ConfirmationOptions {
            assume_yes: args.yes,
            dry_run: args.dry_run,
        },
    )?;
    let report = {
        let mut progress = ProgressAdapter::new(reporter);
        let report = app.apply_install(plan, policy, &mut progress, &NeverCancelled)?;
        progress.finish()?;
        report
    };

    reporter.render_install_report(report)
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
) -> CliResult<()> {
    let package_id = PackageId::parse(args.package_id)?;
    let plan = app.remove_plan(RemoveRequest { package_id })?;
    let policy = confirmer.execution_policy(
        &plan.risks,
        ConfirmationOptions {
            assume_yes: args.yes,
            dry_run: args.dry_run,
        },
    )?;
    let report = {
        let mut progress = ProgressAdapter::new(reporter);
        let report = app.apply_remove(plan, policy, &mut progress, &NeverCancelled)?;
        progress.finish()?;
        report
    };

    reporter.render_remove_report(report)
}

fn search(args: SearchArgs, app: &FontbrewApp, reporter: &mut dyn Reporter) -> CliResult<()> {
    let report = app.search(SearchRequest {
        query: args.query.unwrap_or_default(),
        limit: args.limit,
        refresh: args.refresh,
        offline: args.offline,
    })?;

    reporter.render_search_report(report)
}

fn outdated(args: OutdatedArgs, app: &FontbrewApp, reporter: &mut dyn Reporter) -> CliResult<()> {
    let package_ids = args
        .package_ids
        .into_iter()
        .map(PackageId::parse)
        .collect::<fontbrew_core::Result<Vec<_>>>()?;
    let report = app.outdated(OutdatedRequest {
        package_ids,
        offline: args.offline,
    })?;

    reporter.render_outdated_report(report)
}

fn update(
    args: UpdateArgs,
    app: &FontbrewApp,
    reporter: &mut dyn Reporter,
    confirmer: &mut dyn Confirmer,
) -> CliResult<()> {
    let package_ids = args
        .package_ids
        .into_iter()
        .map(PackageId::parse)
        .collect::<fontbrew_core::Result<Vec<_>>>()?;
    let request = UpdateRequest {
        package_ids,
        jobs: args.jobs,
        offline: false,
    };
    let report = {
        let mut progress = ProgressAdapter::new(reporter);
        let plan = app.update_plan(request, &mut progress, &NeverCancelled)?;
        let policy = confirmer.execution_policy(
            &plan.risks,
            ConfirmationOptions {
                assume_yes: args.yes,
                dry_run: args.dry_run,
            },
        )?;
        let report = app.apply_update(plan, policy, &mut progress, &NeverCancelled)?;
        progress.finish()?;
        report
    };

    reporter.render_update_report(report)
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

fn install_source_from_arg(source: &str) -> InstallSource {
    let path = PathBuf::from(source);

    if path.exists() || looks_like_explicit_local_path(source) {
        InstallSource::LocalPath(path)
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
    let mut formats = args
        .format_preference
        .iter()
        .map(|format| match format {
            CliFontFormat::Otf => FontFormat::Otf,
            CliFontFormat::Ttf => FontFormat::Ttf,
            CliFontFormat::Ttc => FontFormat::Ttc,
            CliFontFormat::Otc => FontFormat::Otc,
        })
        .collect::<Vec<_>>();

    if args.otf {
        formats.push(FontFormat::Otf);
    }

    if args.ttf {
        formats.push(FontFormat::Ttf);
    }

    formats
}

struct NeverCancelled;

impl CancellationToken for NeverCancelled {
    fn is_cancelled(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
