use std::{
    collections::BTreeSet,
    io::{self, IsTerminal, Write},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use fontbrew_core::{
    ConfigValue, FamilyName, FontFormat, InstallReport, ManagedActivationArtifact, ManagedFontFile,
    OutdatedReport, ProgressEvent, RemoveReport, UpdateReport,
};

use crate::{
    exit::{CliError, CliResult},
    reporter::{ConfigReport, InfoReport, ListReport, Reporter, SearchReport},
    self_update::{SelfUpdateReport, SelfUpdateStatus},
};

const SPINNER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const SPINNER_INTERVAL: Duration = Duration::from_millis(80);

pub struct HumanReporter {
    stdout: io::Stdout,
    stderr: io::Stderr,
    quiet: bool,
    verbose: bool,
    show_progress: bool,
    stderr_is_terminal: bool,
    resolved_sources: BTreeSet<String>,
    activity: Option<ActivitySpinner>,
    last_logged_activity: Option<String>,
}

impl HumanReporter {
    pub fn new(quiet: bool, verbose: bool) -> Self {
        let stderr_is_terminal = io::stderr().is_terminal();
        Self {
            stdout: io::stdout(),
            stderr: io::stderr(),
            quiet,
            verbose,
            show_progress: verbose || stderr_is_terminal,
            stderr_is_terminal,
            resolved_sources: BTreeSet::new(),
            activity: None,
            last_logged_activity: None,
        }
    }

    fn finish_activity_line(&mut self) -> CliResult<()> {
        if let Some(activity) = self.activity.take() {
            activity.stop();
        }

        Ok(())
    }
}

impl Drop for HumanReporter {
    fn drop(&mut self) {
        let _ = self.finish_activity_line();
    }
}

struct ActivitySpinner {
    message: Arc<Mutex<String>>,
    stopped: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl ActivitySpinner {
    fn start(message: &str) -> Self {
        let message = Arc::new(Mutex::new(message.to_string()));
        let stopped = Arc::new(AtomicBool::new(false));
        let thread_message = Arc::clone(&message);
        let thread_stopped = Arc::clone(&stopped);
        let handle = thread::spawn(move || {
            let mut frame_index = 0;

            while !thread_stopped.load(Ordering::SeqCst) {
                let message = thread_message
                    .lock()
                    .map(|message| message.clone())
                    .unwrap_or_default();
                {
                    let mut stderr = io::stderr().lock();
                    let _ = write!(
                        stderr,
                        "\r\x1b[2K{} {}",
                        SPINNER_FRAMES[frame_index % SPINNER_FRAMES.len()],
                        message
                    );
                    let _ = stderr.flush();
                }

                frame_index += 1;
                thread::sleep(SPINNER_INTERVAL);
            }
        });

        Self {
            message,
            stopped,
            handle: Some(handle),
        }
    }

    fn update(&self, message: &str) {
        if let Ok(mut current_message) = self.message.lock() {
            *current_message = message.to_string();
        }
    }

    fn stop(mut self) {
        self.stopped.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }

        let mut stderr = io::stderr().lock();
        let _ = write!(stderr, "\r\x1b[2K");
        let _ = stderr.flush();
    }
}

impl Reporter for HumanReporter {
    fn render_install_report(&mut self, report: InstallReport) -> CliResult<()> {
        self.finish_activity()?;
        let mut stdout = self.stdout.lock();
        let families = families_label(&report.families);

        if report.installed {
            writeln!(
                stdout,
                "Installed {} {} ({families})",
                report.package_id.as_str(),
                report.installed_version.as_str()
            )?;
            return Ok(());
        }

        if report.already_installed {
            writeln!(
                stdout,
                "{} is already installed at {}.",
                report.package_id.as_str(),
                report.installed_version.as_str()
            )?;
            return Ok(());
        }

        writeln!(
            stdout,
            "Planned install {} {} ({families}); no changes applied.",
            report.package_id.as_str(),
            report.installed_version.as_str()
        )?;

        Ok(())
    }

    fn render_list_report(&mut self, report: ListReport) -> CliResult<()> {
        self.finish_activity()?;
        let mut stdout = self.stdout.lock();

        if report.packages.is_empty() {
            writeln!(stdout, "No managed packages installed.")?;
            return Ok(());
        }

        let rows = report
            .packages
            .into_iter()
            .map(|package| {
                [
                    package.package_id.as_str().to_string(),
                    package.version.as_str().to_string(),
                    families_overview_label(&package.families),
                    if package.activated {
                        "active".to_string()
                    } else {
                        "inactive".to_string()
                    },
                ]
            })
            .collect::<Vec<_>>();

        write_table(
            &mut stdout,
            ["Package", "Version", "Families", "Status"],
            rows,
        )
    }

    fn render_info_report(&mut self, report: InfoReport) -> CliResult<()> {
        self.finish_activity()?;
        let verbose = self.verbose;
        let mut stdout = self.stdout.lock();
        let package = report.package;
        let families_heading = if package.families.len() == 1 {
            "Family"
        } else {
            "Families"
        };

        writeln!(stdout, "Package: {}", package.package_id.as_str())?;
        writeln!(stdout, "Version: {}", package.version.as_str())?;
        writeln!(
            stdout,
            "{families_heading}: {}",
            families_label(&package.families)
        )?;
        writeln!(
            stdout,
            "Status: {}",
            package_status_label(package.activated, package.managed)
        )?;
        writeln!(stdout, "Source: {}", package.source)?;
        writeln!(
            stdout,
            "Updates: {}",
            update_status_label(package.update_source.as_deref(), package.update_available)
        )?;
        writeln!(stdout)?;
        write_font_status_table(
            &mut stdout,
            &package.font_files,
            &package.activation_artifacts,
        )?;

        if verbose {
            writeln!(stdout)?;
            write_font_files(&mut stdout, "Installed files:", &package.font_files)?;
            write_activation_artifacts(
                &mut stdout,
                "Activation artifacts:",
                &package.activation_artifacts,
            )?;
        }

        Ok(())
    }

    fn render_remove_report(&mut self, report: RemoveReport) -> CliResult<()> {
        self.finish_activity()?;
        let mut stdout = self.stdout.lock();

        if report.planned {
            writeln!(stdout, "Planned removal {}.", report.package_id.as_str())?;
            write_font_files(&mut stdout, "Will remove font files:", &report.font_files)?;
            write_activation_artifacts(
                &mut stdout,
                "Will remove activation artifacts:",
                &report.activation_artifacts,
            )?;
            return Ok(());
        }

        if report.removed {
            writeln!(stdout, "Removed {}.", report.package_id.as_str())?;
        } else {
            writeln!(stdout, "{} is not installed.", report.package_id.as_str())?;
        }

        Ok(())
    }

    fn render_search_report(&mut self, report: SearchReport) -> CliResult<()> {
        self.finish_activity()?;
        let mut stdout = self.stdout.lock();

        if report.results.is_empty() {
            writeln!(stdout, "No packages found.")?;
            return Ok(());
        }

        let rows = report
            .results
            .into_iter()
            .map(|result| {
                [
                    result.display_name,
                    families_label(&result.families),
                    result.source,
                ]
            })
            .collect::<Vec<_>>();

        write_table(&mut stdout, ["Name", "Families", "Source"], rows)
    }

    fn render_outdated_report(&mut self, report: OutdatedReport) -> CliResult<()> {
        self.finish_activity()?;
        let mut stdout = self.stdout.lock();

        if report.packages.is_empty() && report.not_updatable.is_empty() {
            writeln!(stdout, "All checked packages are up to date.")?;
            return Ok(());
        }

        let mut rows = report
            .packages
            .into_iter()
            .map(|package| {
                [
                    package.package_id.as_str().to_string(),
                    package.current_version.as_str().to_string(),
                    package.latest_version.as_str().to_string(),
                    "outdated".to_string(),
                    "-".to_string(),
                ]
            })
            .collect::<Vec<_>>();

        rows.extend(report.not_updatable.into_iter().map(|package| {
            [
                package.package_id.as_str().to_string(),
                "-".to_string(),
                "-".to_string(),
                "not updatable".to_string(),
                package.reason,
            ]
        }));

        write_table(
            &mut stdout,
            ["Package", "Current", "Latest", "Status", "Reason"],
            rows,
        )
    }

    fn render_update_report(&mut self, report: UpdateReport) -> CliResult<()> {
        self.finish_activity()?;
        let mut stdout = self.stdout.lock();

        if report.planned.is_empty() && report.updated.is_empty() {
            writeln!(stdout, "No updates prepared.")?;
        }

        for package in report.planned {
            writeln!(
                stdout,
                "Planned update {} {} -> {}; no changes applied.",
                package.package_id.as_str(),
                package.current_version.as_str(),
                package.target_version.as_str()
            )?;
        }

        for package in report.updated {
            writeln!(
                stdout,
                "Updated {} {} -> {}.",
                package.package_id.as_str(),
                package.previous_version.as_str(),
                package.installed_version.as_str()
            )?;
        }

        if !report.skipped.is_empty() {
            let rows = report
                .skipped
                .into_iter()
                .map(|package| {
                    [
                        package.package_id.as_str().to_string(),
                        "not prepared".to_string(),
                        package.reason,
                    ]
                })
                .collect::<Vec<_>>();
            write_table(&mut stdout, ["Package", "Status", "Reason"], rows)?;
        }

        Ok(())
    }

    fn render_config_get_report(&mut self, report: ConfigReport) -> CliResult<()> {
        self.finish_activity()?;
        let mut stdout = self.stdout.lock();

        writeln!(
            stdout,
            "{} = {}",
            report.key,
            config_value_label(&report.value)
        )?;

        Ok(())
    }

    fn render_config_set_report(&mut self, report: ConfigReport) -> CliResult<()> {
        self.finish_activity()?;
        let mut stdout = self.stdout.lock();

        writeln!(
            stdout,
            "{} = {}",
            report.key,
            config_value_label(&report.value)
        )?;

        Ok(())
    }

    fn render_self_update_report(&mut self, report: SelfUpdateReport) -> CliResult<()> {
        self.finish_activity()?;
        let mut stdout = self.stdout.lock();

        writeln!(stdout, "{}", self_update_status_message(&report))?;

        Ok(())
    }

    fn render_error(&mut self, error: &CliError) -> CliResult<()> {
        self.finish_activity()?;
        let mut stderr = self.stderr.lock();

        writeln!(stderr, "error: {}", error.message())?;

        Ok(())
    }

    fn warn(&mut self, warning: &str) -> CliResult<()> {
        if self.quiet {
            return Ok(());
        }

        self.finish_activity()?;
        let mut stderr = self.stderr.lock();
        writeln!(stderr, "warning: {warning}")?;

        Ok(())
    }

    fn start_activity(&mut self, message: &str) -> CliResult<()> {
        if self.quiet || !self.show_progress {
            return Ok(());
        }

        if self.stderr_is_terminal {
            if let Some(activity) = &self.activity {
                activity.update(message);
            } else {
                self.activity = Some(ActivitySpinner::start(message));
            }
            return Ok(());
        }

        if self.last_logged_activity.as_deref() == Some(message) {
            return Ok(());
        }

        self.last_logged_activity = Some(message.to_string());
        let mut stderr = self.stderr.lock();
        writeln!(stderr, "{message}")?;

        Ok(())
    }

    fn finish_activity(&mut self) -> CliResult<()> {
        self.finish_activity_line()
    }

    fn progress(&mut self, event: &ProgressEvent) -> CliResult<()> {
        if self.quiet || !self.show_progress {
            return Ok(());
        }

        match event {
            ProgressEvent::ResolvingSource { source } => {
                if !self.resolved_sources.insert(source.clone()) {
                    return Ok(());
                }
                self.start_activity(&format!("Resolving {source}"))?;
            }
            ProgressEvent::DownloadStarted { subject, .. } => {
                self.start_activity(&format!("Downloading {}", subject.label()))?;
            }
            ProgressEvent::DownloadProgress {
                subject,
                downloaded,
                total,
            } => {
                if !self.verbose {
                    return Ok(());
                }

                match total {
                    Some(total) => self.start_activity(&format!(
                        "Downloading {}: {downloaded}/{total} bytes",
                        subject.label()
                    ))?,
                    None => self.start_activity(&format!(
                        "Downloading {}: {downloaded} bytes",
                        subject.label()
                    ))?,
                }
            }
            ProgressEvent::ExtractingArchive { subject } => {
                self.start_activity(&format!("Extracting {}", subject.label()))?;
            }
            ProgressEvent::ParsingFonts { subject } => {
                self.start_activity(&format!("Parsing {}", subject.label()))?;
            }
            ProgressEvent::CheckingInstallRisks { package_id } => {
                self.start_activity(&format!(
                    "Checking installed fonts for {}",
                    package_id.as_str()
                ))?;
            }
            ProgressEvent::PreparingUpdate { package_id } => {
                self.start_activity(&format!("Preparing {}", package_id.as_str()))?;
            }
            ProgressEvent::ApplyingUpdate { package_id } => {
                self.start_activity(&format!("Applying {}", package_id.as_str()))?;
            }
            ProgressEvent::FinishedPackage { package_id } => {
                self.finish_activity()?;
                let mut stderr = self.stderr.lock();
                writeln!(stderr, "Finished {}", package_id.as_str())?;
            }
        }

        Ok(())
    }

    fn self_update_progress(&mut self, message: &str) -> CliResult<()> {
        self.start_activity(message)
    }
}

fn write_table<const COLUMN_COUNT: usize>(
    stdout: &mut impl Write,
    headers: [&str; COLUMN_COUNT],
    rows: Vec<[String; COLUMN_COUNT]>,
) -> CliResult<()> {
    let widths = table_widths(&headers, &rows);

    write_table_row(stdout, &headers, &widths)?;
    write_table_separator(stdout, &widths)?;
    for row in rows {
        write_table_row(stdout, &row, &widths)?;
    }

    Ok(())
}

fn table_widths<const COLUMN_COUNT: usize>(
    headers: &[&str; COLUMN_COUNT],
    rows: &[[String; COLUMN_COUNT]],
) -> [usize; COLUMN_COUNT] {
    let mut widths = headers.map(str::len);

    for row in rows {
        for (index, column) in row.iter().enumerate() {
            widths[index] = widths[index].max(column.len());
        }
    }

    widths
}

fn write_table_separator<const COLUMN_COUNT: usize>(
    stdout: &mut impl Write,
    widths: &[usize; COLUMN_COUNT],
) -> CliResult<()> {
    for (index, width) in widths.iter().enumerate() {
        if index > 0 {
            write!(stdout, "  ")?;
        }

        write!(stdout, "{}", "-".repeat(*width))?;
    }
    writeln!(stdout)?;

    Ok(())
}

fn write_table_row<T, const COLUMN_COUNT: usize>(
    stdout: &mut impl Write,
    columns: &[T; COLUMN_COUNT],
    widths: &[usize; COLUMN_COUNT],
) -> CliResult<()>
where
    T: AsRef<str>,
{
    for (index, column) in columns.iter().enumerate() {
        if index > 0 {
            write!(stdout, "  ")?;
        }

        let column = column.as_ref();
        if index + 1 == COLUMN_COUNT {
            write!(stdout, "{column}")?;
        } else {
            write!(stdout, "{column:<width$}", width = widths[index])?;
        }
    }
    writeln!(stdout)?;

    Ok(())
}

fn families_label(families: &[FamilyName]) -> String {
    if families.is_empty() {
        return "unknown family".to_string();
    }

    families
        .iter()
        .map(FamilyName::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}

fn families_overview_label(families: &[FamilyName]) -> String {
    let Some(first_family) = families.first() else {
        return "unknown family".to_string();
    };

    if families.len() == 1 {
        return first_family.as_str().to_string();
    }

    format!("{} (+{} more)", first_family.as_str(), families.len() - 1)
}

fn package_status_label(activated: bool, managed: bool) -> String {
    let status = if activated { "active" } else { "inactive" };
    if managed {
        status.to_string()
    } else {
        format!("{status}, unmanaged")
    }
}

fn update_status_label(update_source: Option<&str>, update_available: Option<bool>) -> String {
    match (update_available, update_source) {
        (Some(true), Some(source)) => format!("available ({source})"),
        (Some(true), None) => "available".to_string(),
        (Some(false), Some(source)) => format!("up to date ({source})"),
        (Some(false), None) => "up to date".to_string(),
        (None, Some(source)) => format!("not checked ({source})"),
        (None, None) => "not configured".to_string(),
    }
}

struct FontStatusRow {
    name: String,
    weight: u16,
    is_italic: bool,
    is_activated: bool,
}

fn write_font_status_table(
    stdout: &mut impl Write,
    font_files: &[ManagedFontFile],
    activation_artifacts: &[ManagedActivationArtifact],
) -> CliResult<()> {
    writeln!(stdout, "Fonts:")?;
    if font_files.is_empty() {
        writeln!(stdout, "- none")?;
        return Ok(());
    }

    let mut rows = font_files
        .iter()
        .map(|font_file| FontStatusRow {
            name: font_file_name(font_file),
            weight: font_file.weight,
            is_italic: is_italic_style(&font_file.style),
            is_activated: font_file_is_activated(font_file, activation_artifacts),
        })
        .collect::<Vec<_>>();

    rows.sort_by(|left, right| {
        left.weight
            .cmp(&right.weight)
            .then(left.is_italic.cmp(&right.is_italic))
            .then(left.name.cmp(&right.name))
    });

    let table_rows = rows
        .into_iter()
        .map(|row| {
            [
                row.name,
                row.weight.to_string(),
                yes_no_label(row.is_italic).to_string(),
                "yes".to_string(),
                yes_no_label(row.is_activated).to_string(),
            ]
        })
        .collect::<Vec<_>>();

    write_table(
        stdout,
        ["Name", "Weight", "Italic", "Installed", "Activated"],
        table_rows,
    )
}

fn font_file_name(font_file: &ManagedFontFile) -> String {
    font_file
        .path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| font_file.path.display().to_string())
}

fn is_italic_style(style: &str) -> bool {
    style.to_ascii_lowercase().contains("italic")
}

fn font_file_is_activated(
    font_file: &ManagedFontFile,
    activation_artifacts: &[ManagedActivationArtifact],
) -> bool {
    activation_artifacts
        .iter()
        .any(|artifact| artifact.source_path == font_file.path)
}

fn yes_no_label(value: bool) -> &'static str {
    if value {
        "yes"
    } else {
        "no"
    }
}

fn write_font_files(
    stdout: &mut impl Write,
    heading: &str,
    font_files: &[ManagedFontFile],
) -> CliResult<()> {
    writeln!(stdout, "{heading}")?;
    if font_files.is_empty() {
        writeln!(stdout, "- none")?;
        return Ok(());
    }

    for font_file in font_files {
        writeln!(
            stdout,
            "- {} ({}, {}, weight {}, {})",
            font_file.path.display(),
            font_file.family.as_str(),
            font_file.style,
            font_file.weight,
            font_format_label(font_file.format)
        )?;
    }

    Ok(())
}

fn write_activation_artifacts(
    stdout: &mut impl Write,
    heading: &str,
    artifacts: &[ManagedActivationArtifact],
) -> CliResult<()> {
    writeln!(stdout, "{heading}")?;
    if artifacts.is_empty() {
        writeln!(stdout, "- none")?;
        return Ok(());
    }

    for artifact in artifacts {
        writeln!(
            stdout,
            "- {} -> {}",
            artifact.path.display(),
            artifact.source_path.display()
        )?;
    }

    Ok(())
}

fn font_format_label(format: FontFormat) -> &'static str {
    match format {
        FontFormat::Otf => "otf",
        FontFormat::Ttf => "ttf",
        FontFormat::Ttc => "ttc",
        FontFormat::Otc => "otc",
    }
}

fn config_value_label(value: &ConfigValue) -> String {
    match value {
        ConfigValue::List(values) => {
            let quoted = values
                .iter()
                .map(|value| format!("\"{value}\""))
                .collect::<Vec<_>>()
                .join(", ");
            format!("[{quoted}]")
        }
        ConfigValue::String(value) => value.clone(),
        ConfigValue::Bool(value) => value.to_string(),
        ConfigValue::Integer(value) => value.to_string(),
    }
}

fn self_update_status_message(report: &SelfUpdateReport) -> String {
    match report.status {
        SelfUpdateStatus::UpToDate => {
            format!("fontbrew {} is up to date.", report.current_version)
        }
        SelfUpdateStatus::Planned => format!(
            "Planned self-update {} -> {}; no changes applied.",
            report.current_version, report.target_version
        ),
        SelfUpdateStatus::Updated => format!(
            "Updated fontbrew {} -> {}.",
            report.current_version, report.target_version
        ),
        SelfUpdateStatus::Reinstalled => {
            format!("Reinstalled fontbrew {}.", report.target_version)
        }
        SelfUpdateStatus::SkippedNewerCurrent => format!(
            "fontbrew {} is newer than the latest stable release {}.",
            report.current_version, report.latest_version
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::self_update::{SelfUpdateInstallMethod, SelfUpdateStatus};

    use super::{self_update_status_message, write_table, SelfUpdateReport};

    #[test]
    fn write_table_separates_header_from_body() {
        let mut output = Vec::new();

        write_table(
            &mut output,
            ["Name", "Status"],
            vec![["source-code-pro".to_string(), "active".to_string()]],
        )
        .expect("write table");

        assert_eq!(
            String::from_utf8(output).expect("table output should be utf-8"),
            "Name             Status\n---------------  ------\nsource-code-pro  active\n"
        );
    }

    fn report(status: SelfUpdateStatus, current_version: &str) -> SelfUpdateReport {
        SelfUpdateReport {
            current_version: current_version.to_string(),
            latest_version: "0.1.2".to_string(),
            target_version: "0.1.2".to_string(),
            executable_path: PathBuf::from("/tmp/bin/fontbrew"),
            install_method: SelfUpdateInstallMethod::Standalone,
            status,
            backup_path: None,
        }
    }

    #[test]
    fn self_update_status_messages_match_human_output_contract() {
        assert_eq!(
            self_update_status_message(&report(SelfUpdateStatus::UpToDate, "0.1.2")),
            "fontbrew 0.1.2 is up to date."
        );
        assert_eq!(
            self_update_status_message(&report(SelfUpdateStatus::Planned, "0.1.1")),
            "Planned self-update 0.1.1 -> 0.1.2; no changes applied."
        );
        assert_eq!(
            self_update_status_message(&report(SelfUpdateStatus::Updated, "0.1.1")),
            "Updated fontbrew 0.1.1 -> 0.1.2."
        );
        assert_eq!(
            self_update_status_message(&report(SelfUpdateStatus::Reinstalled, "0.1.2")),
            "Reinstalled fontbrew 0.1.2."
        );
        assert_eq!(
            self_update_status_message(&report(SelfUpdateStatus::SkippedNewerCurrent, "0.1.3")),
            "fontbrew 0.1.3 is newer than the latest stable release 0.1.2."
        );
    }
}
