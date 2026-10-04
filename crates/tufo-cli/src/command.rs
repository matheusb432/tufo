use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// Manage Tufo's library, playback, desktop settings, and resident server.
#[derive(Parser)]
#[command(name = "tufo", version, about, styles = clap_cargo::style::CLAP_STYLING)]
pub(crate) struct Cli {
    /// Emit complete records as JSON; watch commands emit one object per line.
    #[arg(long, global = true)]
    pub json: bool,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub(crate) enum Command {
    /// Check server health and Discord configuration without changing state.
    Doctor,
    /// Inspect, import, rescan, or watch the complete library.
    Library {
        #[command(subcommand)]
        command: crate::library_command::LibraryCommand,
    },
    /// Discover and manage playlists and soundboards.
    Collection {
        #[command(subcommand)]
        command: crate::library_command::CollectionCommand,
    },
    /// Discover and edit collection entries and sound settings.
    Entry {
        #[command(subcommand)]
        command: crate::library_command::EntryCommand,
    },
    /// Discover songs and remove library references.
    Song {
        #[command(subcommand)]
        command: crate::library_command::SongCommand,
    },
    /// Inspect and unfollow tracked folders.
    Folder {
        #[command(subcommand)]
        command: crate::library_command::FolderCommand,
    },
    /// Inspect or watch playback, change settings, and stand by.
    Player {
        #[command(subcommand)]
        command: crate::player_command::PlayerCommand,
    },
    /// Play and control music.
    Music {
        #[command(subcommand)]
        command: crate::player_command::MusicCommand,
    },
    /// Inspect and change the queued songs.
    Queue {
        #[command(subcommand)]
        command: crate::player_command::QueueCommand,
    },
    /// Inspect and control simultaneous music layers.
    Layer {
        #[command(subcommand)]
        command: crate::player_command::LayerCommand,
    },
    /// Play and control sound effects and saved loops.
    Effect {
        #[command(subcommand)]
        command: crate::player_command::EffectCommand,
    },
    /// Inspect or select the playback output.
    Output {
        #[command(subcommand)]
        command: crate::player_command::OutputCommand,
    },
    /// Inspect Discord configuration and manage its connection.
    Discord {
        #[command(subcommand)]
        command: crate::player_command::DiscordCommand,
    },
    /// Inspect, save, delete, and preview song loops in source frames.
    Loop {
        #[command(subcommand)]
        command: crate::loop_command::LoopCommand,
    },
    /// Import Kenku FM libraries with explicit conflict choices.
    Kenku {
        #[command(subcommand)]
        command: crate::kenku_command::KenkuCommand,
    },
    /// Inspect or change local desktop settings without opening the window.
    Desktop {
        #[command(subcommand)]
        command: crate::desktop_command::DesktopCommand,
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
    /// Export or import songs' saved playback starts and loop regions as JSON.
    SongLoops {
        #[command(subcommand)]
        command: SongLoopsCommand,
    },
}

#[derive(Subcommand)]
pub(crate) enum SongLoopsCommand {
    /// Print every saved song loop as JSON, naming the audio each was saved against.
    Export,
    /// Match each loop in a JSON document to a song by fingerprint, or by path and SHA-256, and
    /// save it. Exits 1 when a loop matches no song or fails its checks.
    Import {
        /// A document written by `tufo song-loops export`.
        #[arg(value_name = "FILE")]
        from: PathBuf,
        /// Report what importing would change without saving anything.
        #[arg(long)]
        dry_run: bool,
        /// Overwrite different saved loops instead of reporting them as conflicts.
        #[arg(long)]
        replace: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum ServerCommand {
    /// Show health, version, process, uptime, and Discord status.
    Status,
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
