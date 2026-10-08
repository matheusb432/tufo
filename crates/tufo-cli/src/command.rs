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
    /// Open or close the browser whose pages play through Tufo.
    Browser {
        #[command(subcommand)]
        command: crate::player_command::BrowserCommand,
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
        command: crate::server::ServerCommand,
    },
    /// Open the desktop window.
    Gui {
        #[command(subcommand)]
        command: crate::gui::GuiCommand,
    },
    /// Export or import the database and portable settings for machine handoff.
    Data {
        #[command(subcommand)]
        command: crate::data::DataCommand,
    },
    /// Export or import songs' saved playback starts and loop regions as JSON.
    SongLoops {
        #[command(subcommand)]
        command: crate::song_loops::SongLoopsCommand,
    },
}
