use std::io::{self, Write};

use fontbrew_core::{
    InfoReport, InstallReport, ListReport, ProgressEvent, RegistryStatusReport,
    RegistryUpdateReport, RemoveReport,
};
use serde::Serialize;

use crate::{
    exit::{CliError, CliResult},
    reporter::Reporter,
};

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

    fn render_list_report(&mut self, report: ListReport) -> CliResult<()> {
        self.render_report("list", &report)
    }

    fn render_info_report(&mut self, report: InfoReport) -> CliResult<()> {
        self.render_report("info", &report)
    }

    fn render_remove_report(&mut self, report: RemoveReport) -> CliResult<()> {
        self.render_report("remove", &report)
    }

    fn render_registry_update_report(&mut self, report: RegistryUpdateReport) -> CliResult<()> {
        self.render_report("registry_update", &report)
    }

    fn render_registry_status_report(&mut self, report: RegistryStatusReport) -> CliResult<()> {
        self.render_report("registry_status", &report)
    }

    fn render_error(&mut self, error: &CliError) -> CliResult<()> {
        let envelope = ErrorEnvelope {
            schema_version: 1,
            error: ErrorBody {
                kind: error.kind(),
                message: error.message(),
                risks: error.risks(),
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
}
