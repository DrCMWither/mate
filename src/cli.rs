use clap::{Parser, Subcommand};

use crate::model::ManagerKind;

#[derive(Debug, Parser)]
#[command(
    name = "mate",
    version,
    about = "Right then. Scour your package managers, have a butchers at the plan, and get it SORTED."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Discover supported package-manager instances and installation targets.
    Doctor {
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },

    /// Search every discovered manager without changing the system.
    Search {
        /// One or more package names.
        #[arg(required = true)]
        packages: Vec<String>,

        /// Restrict search to one or more managers.
        #[arg(long, value_enum, value_delimiter = ',')]
        manager: Vec<ManagerKind>,

        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },

    /// Select candidates and targets, preview the complete plan, then install.
    Install {
        /// One or more package names.
        #[arg(required = true)]
        packages: Vec<String>,

        /// Restrict selection to one or more managers.
        #[arg(long, value_enum, value_delimiter = ',')]
        manager: Vec<ManagerKind>,

        /// Target id from `mate doctor`, such as system, user, or venv:/path.
        #[arg(long)]
        target: Option<String>,

        /// Exact manager-instance id from `mate doctor` when several instances match.
        #[arg(long)]
        instance: Option<String>,

        /// Print the plan and stop before confirmation or execution.
        #[arg(long)]
        dry_run: bool,

        /// Accept the final plan without an interactive confirmation.
        #[arg(long)]
        yes: bool,
    },
}
