use std::io::{self, Write};

use fontbrew_core::{
    ConfigReport, InfoReport, InstallBatchReport, InstallReport, ListReport, OutdatedReport,
    ProgressEvent, RemoveReport, SearchReport, UpdateReport,
};
use serde::Serialize;

use crate::{
    exit::{CliError, CliResult},
    reporter::Reporter,
    self_update::SelfUpdateReport,
};

const SELF_UPDATE_COMMAND: &str = "self_update";

pub struct JsonReporter {
    stdout: io::Stdout,
}

impl JsonReporter {
    pub fn new() -> Self {
        Self {
            stdout: io::stdout(),
        }
    }

    fn render_report<T>(&mut self, command: &'static str, report: &T) -> CliResult<()>
    where
        T: Serialize,
    {
        let envelope = ReportEnvelope {
            schema_version: 1,
            command,
            report,
        };

        self.write_json(&envelope)
    }

    fn write_json<T>(&mut self, payload: &T) -> CliResult<()>
    where
        T: Serialize,
    {
        let mut stdout = self.stdout.lock();
        serde_json::to_writer(&mut stdout, payload)?;
        writeln!(stdout)?;

        Ok(())
    }
}

impl Reporter for JsonReporter {
    fn render_install_report(&mut self, report: InstallReport) -> CliResult<()> {
        self.render_report("install", &report)
    }

    fn render_install_batch_report(&mut self, report: InstallBatchReport) -> CliResult<()> {
        self.render_report("install", &report)
    }

    fn render_list_report(&mut self, report: ListReport) -> CliResult<()> {
        self.render_report("list", &report)
    }

    fn render_info_report(&mut self, report: InfoReport) -> CliResult<()> {
        self.render_report("info", &report)
    }

    fn render_remove_report(&mut self, report: RemoveReport) -> CliResult<()> {
        self.render_report("remove", &report)
    }

    fn render_search_report(&mut self, report: SearchReport) -> CliResult<()> {
        self.render_report("search", &report)
    }

    fn render_outdated_report(&mut self, report: OutdatedReport) -> CliResult<()> {
        self.render_report("outdated", &report)
    }

    fn render_update_report(&mut self, report: UpdateReport) -> CliResult<()> {
        self.render_report("update", &report)
    }

    fn render_config_get_report(&mut self, report: ConfigReport) -> CliResult<()> {
        self.render_report("config_get", &report)
    }

    fn render_config_set_report(&mut self, report: ConfigReport) -> CliResult<()> {
        self.render_report("config_set", &report)
    }

    fn render_self_update_report(&mut self, report: SelfUpdateReport) -> CliResult<()> {
        self.render_report(SELF_UPDATE_COMMAND, &report)
    }

    fn render_error(&mut self, error: &CliError) -> CliResult<()> {
        let envelope = ErrorEnvelope {
            schema_version: 1,
            error: ErrorBody {
                kind: error.kind(),
                message: error.message(),
                risks: error.risks(),
                families: error.families(),
            },
        };

        self.write_json(&envelope)
    }

    fn warn(&mut self, _warning: &str) -> CliResult<()> {
        Ok(())
    }

    fn progress(&mut self, _event: &ProgressEvent) -> CliResult<()> {
        Ok(())
    }

    fn self_update_progress(&mut self, _message: &str) -> CliResult<()> {
        Ok(())
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ReportEnvelope<'a, T>
where
    T: Serialize,
{
    schema_version: u8,
    command: &'static str,
    report: &'a T,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ErrorEnvelope<'a> {
    schema_version: u8,
    error: ErrorBody<'a>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ErrorBody<'a> {
    kind: &'static str,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    risks: Option<&'a [fontbrew_core::PlanRisk]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    families: Option<&'a [fontbrew_core::FamilyName]>,
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::self_update::{SelfUpdateInstallMethod, SelfUpdateReport, SelfUpdateStatus};

    use super::{ReportEnvelope, SELF_UPDATE_COMMAND};

    #[test]
    fn self_update_json_envelope_uses_stable_command_name() {
        let report = SelfUpdateReport {
            current_version: "0.1.1".to_string(),
            latest_version: "0.1.2".to_string(),
            target_version: "0.1.2".to_string(),
            executable_path: PathBuf::from("/tmp/bin/fontbrew"),
            install_method: SelfUpdateInstallMethod::Standalone,
            status: SelfUpdateStatus::Planned,
            backup_path: None,
        };
        let envelope = ReportEnvelope {
            schema_version: 1,
            command: SELF_UPDATE_COMMAND,
            report: &report,
        };

        let json = serde_json::to_value(envelope).expect("serialize envelope");

        assert_eq!(json["schemaVersion"], 1);
        assert_eq!(json["command"], "self_update");
        assert_eq!(json["report"]["status"], "planned");
    }
}
