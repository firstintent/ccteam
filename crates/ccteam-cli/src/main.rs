//! `ccteam` binary entry point. Subcommand groups land in later M0 tasks:
//! M0.3 wires the `hook` group, M0.6 the orchestrator daemon (`start`),
//! and M0.11 the project management commands (`new` / `ls` / `show` / ...).

use anyhow::Result;
use clap::Parser;

#[derive(Parser)]
#[command(
    name = "ccteam",
    version,
    about = "Autonomous AI development orchestrator built on Claude Code",
)]
struct Cli {}

fn main() -> Result<()> {
    let _ = Cli::parse();
    Ok(())
}
