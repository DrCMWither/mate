mod adapters;
mod cli;
mod context;
mod engine;
mod model;
mod planner;
mod platform;
mod process;
mod ui;

use anyhow::Result;
use clap::Parser;

use crate::cli::{Cli, Command};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    process::ensure_unprivileged()?;

    match cli.command {
        Command::Doctor { json } => engine::doctor(json).await,
        Command::Search {
            packages,
            manager,
            json,
        } => engine::search(packages, manager, json).await,
        Command::Install {
            packages,
            manager,
            target,
            instance,
            dry_run,
            yes,
        } => engine::install(packages, manager, target, instance, dry_run, yes).await,
    }
}
