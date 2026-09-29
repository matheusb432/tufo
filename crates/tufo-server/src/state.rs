use std::{path::PathBuf, sync::Arc, time::Instant};

use tokio::sync::Notify;
use tufo_application::library::Library;
use tufo_infra::{
    discord::{SharedDiscord, bot_credentials::FileBotCredentials},
    library_files::DiskLibraryFiles,
    player::{EnginePlayer, outputs::DeviceOutputs},
};

/// Cloneable composition shared by the gRPC service adapters.
#[derive(Clone)]
pub struct AppState {
    pub(crate) player: Arc<EnginePlayer<DeviceOutputs>>,
    pub(crate) discord: SharedDiscord,
    pub(crate) library: Arc<Library<DiskLibraryFiles>>,
    /// Where the server reads its Discord bot token at startup and saves a new one.
    pub(crate) discord_credentials: FileBotCredentials,
    /// Notified when a client asks the server to stop.
    pub(crate) stop_requested: Arc<Notify>,
    pub(crate) started_at: Instant,
}

impl AppState {
    #[must_use]
    pub fn new(
        player: Arc<EnginePlayer<DeviceOutputs>>,
        discord: SharedDiscord,
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
