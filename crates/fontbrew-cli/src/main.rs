use anyhow::Result;
use clap::Parser;

#[derive(Parser)]
#[command(
    name = "fontbrew",
    version,
    about = "Manage third-party open-source fonts on macOS"
)]
struct Cli;

fn main() -> Result<()> {
    Cli::parse();

    Ok(())
}
