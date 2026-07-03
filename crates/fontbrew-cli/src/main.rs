use clap::Parser;
use reporter::Reporter;
use std::process::ExitCode;

mod cli;
mod confirm;
mod exit;
mod progress;
mod reporter;

fn main() -> ExitCode {
    match cli::Cli::try_parse() {
        Ok(cli) => ExitCode::from(cli::run(cli)),
        Err(error) => {
            if std::env::args_os().any(|arg| arg == "--json") {
                let mut reporter = reporter::json::JsonReporter::new();
                let cli_error = exit::CliError::Usage {
                    message: error.to_string(),
                };
                let _ = reporter.render_error(&cli_error);
                ExitCode::from(exit::FAILURE)
            } else {
                let _ = error.print();
                let code = u8::try_from(error.exit_code()).unwrap_or(exit::FAILURE);
                ExitCode::from(code)
            }
        }
    }
}
