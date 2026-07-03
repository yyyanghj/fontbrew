use std::path::PathBuf;

use clap::{ArgAction, Args, Parser, Subcommand, ValueEnum};
use fontbrew_core::{
    CancellationToken, FontFormat, FontbrewApp, InfoRequest, InstallRequest, InstallSource,
    PackageId, RemoveRequest,
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

fn install_source_from_arg(source: &str) -> InstallSource {
    let path = PathBuf::from(source);

    if path.exists() || looks_like_local_path(source) {
        InstallSource::LocalPath(path)
    } else {
        InstallSource::RegistryName(source.to_string())
    }
}

fn looks_like_local_path(source: &str) -> bool {
    source.starts_with('.')
        || source.starts_with('/')
        || source.contains('/')
        || source.contains('\\')
        || source.ends_with(".zip")
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
