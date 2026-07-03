pub mod human;
pub mod json;

use fontbrew_core::{
    InfoReport, InstallReport, ListReport, ProgressEvent, RegistryStatusReport,
    RegistryUpdateReport, RemoveReport,
};

use crate::exit::{CliError, CliResult};

pub trait Reporter {
    fn render_install_report(&mut self, report: InstallReport) -> CliResult<()>;
    fn render_list_report(&mut self, report: ListReport) -> CliResult<()>;
    fn render_info_report(&mut self, report: InfoReport) -> CliResult<()>;
    fn render_remove_report(&mut self, report: RemoveReport) -> CliResult<()>;
    fn render_registry_update_report(&mut self, report: RegistryUpdateReport) -> CliResult<()>;
    fn render_registry_status_report(&mut self, report: RegistryStatusReport) -> CliResult<()>;
    fn render_error(&mut self, error: &CliError) -> CliResult<()>;
    #[allow(dead_code)]
    fn warn(&mut self, warning: &str) -> CliResult<()>;
    fn progress(&mut self, event: &ProgressEvent) -> CliResult<()>;
}
