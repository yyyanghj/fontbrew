pub mod human;
pub mod json;

use fontbrew_core::{
    ConfigReport, InfoReport, InstallReport, ListReport, OutdatedReport, ProgressEvent,
    RegistryStatusReport, RegistryUpdateReport, RemoveReport, SearchReport, UpdateReport,
};

use crate::exit::{CliError, CliResult};
use crate::self_update::SelfUpdateReport;

pub trait Reporter {
    fn render_install_report(&mut self, report: InstallReport) -> CliResult<()>;
    fn render_list_report(&mut self, report: ListReport) -> CliResult<()>;
    fn render_info_report(&mut self, report: InfoReport) -> CliResult<()>;
    fn render_remove_report(&mut self, report: RemoveReport) -> CliResult<()>;
    fn render_search_report(&mut self, report: SearchReport) -> CliResult<()>;
    fn render_outdated_report(&mut self, report: OutdatedReport) -> CliResult<()>;
    fn render_update_report(&mut self, report: UpdateReport) -> CliResult<()>;
    fn render_config_get_report(&mut self, report: ConfigReport) -> CliResult<()>;
    fn render_config_set_report(&mut self, report: ConfigReport) -> CliResult<()>;
    fn render_registry_update_report(&mut self, report: RegistryUpdateReport) -> CliResult<()>;
    fn render_registry_status_report(&mut self, report: RegistryStatusReport) -> CliResult<()>;
    fn render_self_update_report(&mut self, report: SelfUpdateReport) -> CliResult<()>;
    fn render_error(&mut self, error: &CliError) -> CliResult<()>;
    #[allow(dead_code)]
    fn warn(&mut self, warning: &str) -> CliResult<()>;
    fn progress(&mut self, event: &ProgressEvent) -> CliResult<()>;
    fn self_update_progress(&mut self, message: &str) -> CliResult<()>;
}
