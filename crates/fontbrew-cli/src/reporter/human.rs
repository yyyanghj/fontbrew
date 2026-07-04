use std::io::{self, IsTerminal, Write};

use fontbrew_core::{
    config::ActivationStrategy, ConfigReport, ConfigValue, FamilyName, FontFormat, InfoReport,
    InstallReport, ListReport, ManagedActivationArtifact, ManagedFontFile, OutdatedReport,
    ProgressEvent, RegistryStatusReport, RegistryUpdateReport, RemoveReport, SearchReport,
    UpdateReport,
};

use crate::{
    exit::{CliError, CliResult},
    reporter::Reporter,
    self_update::{SelfUpdateReport, SelfUpdateStatus},
};

pub struct HumanReporter {
    stdout: io::Stdout,
    stderr: io::Stderr,
    quiet: bool,
    verbose: bool,
    show_progress: bool,
}

impl HumanReporter {
    pub fn new(quiet: bool, verbose: bool) -> Self {
        Self {
            stdout: io::stdout(),
            stderr: io::stderr(),
            quiet,
            verbose,
            show_progress: verbose || io::stderr().is_terminal(),
        }
    }
}

impl Reporter for HumanReporter {
    fn render_install_report(&mut self, report: InstallReport) -> CliResult<()> {
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
        let mut stdout = self.stdout.lock();

        if report.packages.is_empty() {
            writeln!(stdout, "No managed packages installed.")?;
            return Ok(());
        }

        for package in report.packages {
            let status = if package.activated {
                "active"
            } else {
                "inactive"
            };
            writeln!(
                stdout,
                "{}\t{}\t{}\t{status}",
                package.package_id.as_str(),
                package.version.as_str(),
                families_label(&package.families)
            )?;
        }

        Ok(())
    }

    fn render_info_report(&mut self, report: InfoReport) -> CliResult<()> {
        let mut stdout = self.stdout.lock();
        let package = report.package;
        let update_source = package.update_source.as_deref().unwrap_or("none");
        let activated = if package.activated { "yes" } else { "no" };
        let managed = if package.managed { "yes" } else { "no" };
        let update_available = update_available_label(package.update_available);

        writeln!(stdout, "Package: {}", package.package_id.as_str())?;
        writeln!(stdout, "Version: {}", package.version.as_str())?;
        writeln!(stdout, "Families: {}", families_label(&package.families))?;
        writeln!(stdout, "Source: {}", package.source)?;
        writeln!(stdout, "Update source: {update_source}")?;
        writeln!(stdout, "Activated: {activated}")?;
        writeln!(stdout, "Managed: {managed}")?;
        writeln!(stdout, "Update available: {update_available}")?;
        write_font_files(&mut stdout, "Installed files:", &package.font_files)?;
        write_activation_artifacts(
            &mut stdout,
            "Activation artifacts:",
            &package.activation_artifacts,
        )?;

        Ok(())
    }

    fn render_remove_report(&mut self, report: RemoveReport) -> CliResult<()> {
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
        let mut stdout = self.stdout.lock();

        if report.results.is_empty() {
            writeln!(stdout, "No packages found.")?;
            return Ok(());
        }

        let rows = report
            .results
            .into_iter()
            .map(|result| SearchResultRow {
                package_id: result.package_id.as_str().to_string(),
                name: result.display_name,
                families: families_label(&result.families),
                source: result.source,
            })
            .collect::<Vec<_>>();
        let widths = search_result_widths(&rows);

        writeln!(
            stdout,
            "{:<package_id_width$}  {:<name_width$}  {:<families_width$}  SOURCE",
            "PACKAGE ID",
            "NAME",
            "FAMILIES",
            package_id_width = widths.package_id,
            name_width = widths.name,
            families_width = widths.families,
        )?;
        for row in rows {
            writeln!(
                stdout,
                "{:<package_id_width$}  {:<name_width$}  {:<families_width$}  {}",
                row.package_id,
                row.name,
                row.families,
                row.source,
                package_id_width = widths.package_id,
                name_width = widths.name,
                families_width = widths.families,
            )?;
        }

        Ok(())
    }

    fn render_outdated_report(&mut self, report: OutdatedReport) -> CliResult<()> {
        let mut stdout = self.stdout.lock();

        if report.packages.is_empty() && report.not_updatable.is_empty() {
            writeln!(stdout, "All checked packages are up to date.")?;
            return Ok(());
        }

        for package in report.packages {
            writeln!(
                stdout,
                "{}\t{} -> {}",
                package.package_id.as_str(),
                package.current_version.as_str(),
                package.latest_version.as_str()
            )?;
        }

        for package in report.not_updatable {
            writeln!(
                stdout,
                "{}\tnot updatable: {}",
                package.package_id.as_str(),
                package.reason
            )?;
        }

        Ok(())
    }

    fn render_update_report(&mut self, report: UpdateReport) -> CliResult<()> {
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

        for package in report.skipped {
            writeln!(
                stdout,
                "{}\tnot prepared: {}",
                package.package_id.as_str(),
                package.reason
            )?;
        }

        Ok(())
    }

    fn render_config_get_report(&mut self, report: ConfigReport) -> CliResult<()> {
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
        let mut stdout = self.stdout.lock();

        writeln!(
            stdout,
            "{} = {}",
            report.key,
            config_value_label(&report.value)
        )?;

        Ok(())
    }

    fn render_registry_update_report(&mut self, report: RegistryUpdateReport) -> CliResult<()> {
        let mut stdout = self.stdout.lock();

        writeln!(stdout, "Updated registry snapshot.")?;
        writeln!(stdout, "Source: {}", report.registry_url)?;
        writeln!(stdout, "Snapshot: {}", report.snapshot_path.display())?;
        writeln!(
            stdout,
            "Registry updated at: {}",
            report.registry_updated_at
        )?;
        writeln!(stdout, "Packages: {}", report.package_count)?;

        Ok(())
    }

    fn render_registry_status_report(&mut self, report: RegistryStatusReport) -> CliResult<()> {
        let mut stdout = self.stdout.lock();

        if !report.available {
            writeln!(stdout, "Registry snapshot: missing")?;
            writeln!(stdout, "Path: {}", report.snapshot_path.display())?;
            return Ok(());
        }

        writeln!(stdout, "Registry snapshot: available")?;
        writeln!(stdout, "Path: {}", report.snapshot_path.display())?;
        if let Some(schema_version) = report.schema_version {
            writeln!(stdout, "Schema version: {schema_version}")?;
        }
        if let Some(updated_at) = report.registry_updated_at {
            writeln!(stdout, "Registry updated at: {updated_at}")?;
        }
        if let Some(modified_at) = report.snapshot_modified_at {
            writeln!(stdout, "Snapshot refreshed at: {modified_at}")?;
        }
        writeln!(stdout, "Packages: {}", report.package_count)?;

        Ok(())
    }

    fn render_self_update_report(&mut self, report: SelfUpdateReport) -> CliResult<()> {
        let mut stdout = self.stdout.lock();

        writeln!(stdout, "{}", self_update_status_message(&report))?;

        Ok(())
    }

    fn render_error(&mut self, error: &CliError) -> CliResult<()> {
        let mut stderr = self.stderr.lock();

        writeln!(stderr, "error: {}", error.message())?;

        Ok(())
    }

    fn warn(&mut self, warning: &str) -> CliResult<()> {
        if self.quiet {
            return Ok(());
        }

        let mut stderr = self.stderr.lock();
        writeln!(stderr, "warning: {warning}")?;

        Ok(())
    }

    fn progress(&mut self, event: &ProgressEvent) -> CliResult<()> {
        if self.quiet || !self.show_progress {
            return Ok(());
        }

        let mut stderr = self.stderr.lock();
        match event {
            ProgressEvent::ResolvingSource { source } => {
                writeln!(stderr, "Resolving {source}")?;
            }
            ProgressEvent::DownloadStarted { package_id, .. } => {
                writeln!(stderr, "Downloading {}", package_id.as_str())?;
            }
            ProgressEvent::DownloadProgress {
                package_id,
                downloaded,
                total,
            } => {
                if !self.verbose {
                    return Ok(());
                }

                match total {
                    Some(total) => writeln!(
                        stderr,
                        "Downloading {}: {downloaded}/{total} bytes",
                        package_id.as_str()
                    )?,
                    None => writeln!(
                        stderr,
                        "Downloading {}: {downloaded} bytes",
                        package_id.as_str()
                    )?,
                }
            }
            ProgressEvent::ExtractingArchive { package_id } => {
                writeln!(stderr, "Extracting {}", package_id.as_str())?;
            }
            ProgressEvent::ParsingFonts { package_id } => {
                writeln!(stderr, "Parsing {}", package_id.as_str())?;
            }
            ProgressEvent::PreparingUpdate { package_id } => {
                writeln!(stderr, "Preparing {}", package_id.as_str())?;
            }
            ProgressEvent::ApplyingUpdate { package_id } => {
                writeln!(stderr, "Applying {}", package_id.as_str())?;
            }
            ProgressEvent::FinishedPackage { package_id } => {
                writeln!(stderr, "Finished {}", package_id.as_str())?;
            }
        }

        Ok(())
    }

    fn self_update_progress(&mut self, message: &str) -> CliResult<()> {
        if self.quiet || !self.show_progress {
            return Ok(());
        }

        let mut stderr = self.stderr.lock();
        writeln!(stderr, "{message}")?;

        Ok(())
    }
}

struct SearchResultRow {
    package_id: String,
    name: String,
    families: String,
    source: String,
}

struct SearchResultWidths {
    package_id: usize,
    name: usize,
    families: usize,
}

fn search_result_widths(rows: &[SearchResultRow]) -> SearchResultWidths {
    let mut widths = SearchResultWidths {
        package_id: "PACKAGE ID".len(),
        name: "NAME".len(),
        families: "FAMILIES".len(),
    };

    for row in rows {
        widths.package_id = widths.package_id.max(row.package_id.len());
        widths.name = widths.name.max(row.name.len());
        widths.families = widths.families.max(row.families.len());
    }

    widths
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

fn update_available_label(update_available: Option<bool>) -> &'static str {
    match update_available {
        Some(true) => "yes",
        Some(false) => "no",
        None => "unknown",
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
            "- {} -> {} ({})",
            artifact.path.display(),
            artifact.source_path.display(),
            activation_strategy_label(artifact.strategy)
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

fn activation_strategy_label(strategy: ActivationStrategy) -> &'static str {
    match strategy {
        ActivationStrategy::Symlink => "symlink",
        ActivationStrategy::Copy => "copy",
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

    use super::{self_update_status_message, SelfUpdateReport};

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
