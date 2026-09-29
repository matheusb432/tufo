use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// Check Tufo's services, control its local audio server, and open its window.
#[derive(Parser)]
#[command(name = "tufo", version, about, styles = clap_cargo::style::CLAP_STYLING)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub(crate) enum Command {
    /// Check server health and Discord configuration without changing state.
    Doctor {
        /// Print a machine-readable report without terminal styling.
        #[arg(long)]
        json: bool,
    },
    /// Inspect or control the resident server selected by `TUFO_RUNTIME_DIR`.
    Server {
        #[command(subcommand)]
        command: ServerCommand,
    },
    /// Open the desktop window.
    Gui {
        #[command(subcommand)]
        command: GuiCommand,
    },
    #[command(about = "Export or import the database and portable settings for machine handoff.")]
    Data {
        #[command(subcommand)]
        command: DataCommand,
    },
}

#[derive(Subcommand)]
pub(crate) enum ServerCommand {
    /// Show health, version, process, uptime, and Discord status.
    Status {
        /// Print a machine-readable report without terminal styling.
        #[arg(long)]
        json: bool,
    },
    /// Start the sibling server (or tufo-server on PATH), then wait until healthy.
    Start,
    /// Stop all audio and shut down the server through its private control endpoint.
    Kill,
    /// Shut down the server, then start it and wait until healthy.
    Restart,
}

#[derive(Subcommand)]
pub(crate) enum GuiCommand {
    /// Start the sibling GUI (or tufo-gui on PATH), which raises the running window instead when
    /// there is one.
    Open,
}

#[derive(Subcommand)]
#[command(about = "Selects a data snapshot operation.")]
pub(crate) enum DataCommand {
    #[command(
        about = "Write a consistent snapshot of the database and portable settings into DIR."
    )]
    Export {
        #[arg(
            long,
            value_name = "DIR",
            help = "Snapshot directory to create; its parent must exist."
        )]
        to: PathBuf,
    },
    #[command(
        about = "Replace the database and portable settings with a snapshot, migrating an older snapshot forward."
    )]
    Import {
        #[arg(
            long,
            value_name = "DIR",
            help = "Snapshot directory written by `tufo data export`."
        )]
        from: PathBuf,
    },
}
