use std::{path::PathBuf, sync::Arc, time::Instant};

use tokio::sync::Notify;
use tufo_application::library::Library;
use tufo_infra::{
    discord::{SongbirdDiscordClient, bot_credentials::FileBotCredentials},
    library_files::DiskLibraryFiles,
    player::{
        EnginePlayer,
        outputs::{AudioOutputs, DeviceOutputs},
    },
};

/// Cloneable composition shared by the gRPC service adapters.
pub struct AppState<O: AudioOutputs = DeviceOutputs> {
    pub(crate) player: Arc<EnginePlayer<O>>,
    pub(crate) discord: SongbirdDiscordClient,
    pub(crate) library: Arc<Library<DiskLibraryFiles>>,
    /// Where the server reads its Discord bot token at startup and saves a new one.
    pub(crate) discord_credentials: FileBotCredentials,
    /// Notified when a client asks the server to stop.
    pub(crate) stop_requested: Arc<Notify>,
    pub(crate) started_at: Instant,
}

impl<O: AudioOutputs> Clone for AppState<O> {
    fn clone(&self) -> Self {
        Self {
            player: Arc::clone(&self.player),
            discord: self.discord.clone(),
            library: Arc::clone(&self.library),
            discord_credentials: self.discord_credentials.clone(),
            stop_requested: Arc::clone(&self.stop_requested),
            started_at: self.started_at,
        }
    }
}

impl<O: AudioOutputs> AppState<O> {
    #[must_use]
    pub fn new(
        player: Arc<EnginePlayer<O>>,
        discord: SongbirdDiscordClient,
        library: Arc<Library<DiskLibraryFiles>>,
        discord_credentials_path: PathBuf,
    ) -> Self {
        Self {
            player,
            discord,
            library,
            discord_credentials: FileBotCredentials::new(discord_credentials_path),
            stop_requested: Arc::new(Notify::new()),
            started_at: Instant::now(),
        }
    }

    /// Resolves once a client asks the server to stop.
    pub async fn stop_requested(&self) {
        self.stop_requested.notified().await;
    }
}
