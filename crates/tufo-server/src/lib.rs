//! Local gRPC process root, served over a user-private local endpoint.

pub mod data;
mod server;
mod services;
mod state;

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::Context as _;
pub use server::serve;
pub use state::AppState;
use tufo_application::{
    library::{
        Library, folder_watcher, record_last_played,
        seed_default_effects::{self, DefaultEffectFile, SeedDefaultEffectsOutcome},
    },
    playback::{update_player_settings, watch_player},
};
use tufo_infra::{
    browser::ProcessBrowserLauncher,
    database, default_effects,
    discord::{DiscordCredentials, SongbirdDiscordClient},
    folder_watcher::NotifyFolderWatcher,
    library_files::DiskLibraryFiles,
    player::EnginePlayer,
};
use tufo_local_transport::{LocalEndpoint, LocalListener};

const SOCKET_PROBE_TIMEOUT: Duration = Duration::from_secs(1);
const SHUTDOWN_GRACE_PERIOD: Duration = Duration::from_secs(5);
const DISCORD_CREDENTIALS_FILE_NAME: &str = "discord.toml";

/// Runs the resident server until it is asked to stop. Discord output needs credentials from
/// `discord_credentials`, or from `discord.toml` in the config directory when that file exists:
/// `$XDG_CONFIG_HOME/tufo` on Linux, `%APPDATA%\tufo\config` on Windows, and
/// `~/Library/Application Support/tufo` on macOS.
pub async fn run(discord_credentials: Option<PathBuf>) -> anyhow::Result<()> {
    let endpoint =
        LocalEndpoint::from_environment().context("resolving the local server endpoint")?;
    let listener = LocalListener::bind(&endpoint, SOCKET_PROBE_TIMEOUT)
        .await
        .context("binding the local gRPC endpoint")?;
    let discord_credentials_path = discord_credentials
        .or_else(default_discord_credentials)
        .context("resolving the config directory for the Discord credentials")?;
    let discord = start_discord(&discord_credentials_path)?;
    let database_path = database::database_path()?;
    let database = database::open_database(&database_path)
        .await
        .with_context(|| format!("opening the library database {}", database_path.display()))?;
    let library = Arc::new(
        Library::open(database, DiskLibraryFiles)
            .await
            .context("loading the library")?,
    );
    let player = EnginePlayer::start(
        discord.clone(),
        library.saved_song_loops(),
        ProcessBrowserLauncher::beside_database(&database_path, log_directory().as_deref()),
    );
    seed_default_effects(&database_path, &library).await;
    if let Err(error) = update_player_settings::restore(player.as_ref(), library.database()).await {
        tracing::warn!(%error, "cannot restore the saved player settings");
    }
    let (stop_following, following_stopped) = tokio::sync::oneshot::channel::<()>();
    let follow_library = tokio::spawn({
        let library = Arc::clone(&library);
        async move {
            let watcher = NotifyFolderWatcher::start();
            folder_watcher::run(&library, &watcher, async {
                let _ = following_stopped.await;
            })
            .await;
        }
    });
    let (stop_recording, recording_stopped) = tokio::sync::oneshot::channel::<()>();
    let record_plays = tokio::spawn({
        let library = Arc::clone(&library);
        let player_states = watch_player::execute(player.as_ref());
        async move {
            record_last_played::run(&library, player_states, async {
                let _ = recording_stopped.await;
            })
            .await;
        }
    });

    tracing::info!(path = %endpoint.path().display(), "tufo-server listening");
    let result = serve(
        listener,
        async {
            if let Err(error) = shutdown_signal().await {
                tracing::error!(%error, "failed while waiting for a shutdown signal");
            }
        },
        SHUTDOWN_GRACE_PERIOD,
        AppState::new(
            Arc::clone(&player),
            discord,
            library,
            discord_credentials_path,
        ),
    )
    .await;
    let _ = stop_following.send(());
    let _ = stop_recording.send(());
    if let Err(error) = follow_library.await {
        tracing::error!(%error, "the library follower stopped abnormally");
    }
    if let Err(error) = record_plays.await {
        tracing::error!(%error, "the last-played recorder stopped abnormally");
    }
    player.shut_down().await;
    tracing::info!("tufo-server stopped");
    result
}

async fn seed_default_effects(database_path: &Path, library: &Library<DiskLibraryFiles>) {
    let Some(files) = write_default_effects(database_path).await else {
        return;
    };
    match seed_default_effects::execute(files, library).await {
        Ok(SeedDefaultEffectsOutcome::Seeded) => tracing::info!("added the Effects soundboard"),
        Ok(SeedDefaultEffectsOutcome::AlreadySeeded) => {}
        Ok(SeedDefaultEffectsOutcome::NothingReadable { unreadable }) => {
            tracing::warn!(?unreadable, "cannot read any default effect");
        }
        Err(error) => tracing::warn!(%error, "cannot add the Effects soundboard"),
    }
}

async fn write_default_effects(database_path: &Path) -> Option<Vec<DefaultEffectFile>> {
    let folder = default_effects::effects_folder_beside(database_path);
    let written =
        tokio::task::spawn_blocking(move || default_effects::write_default_effects(&folder)).await;
    match written {
        Ok(Ok(files)) => Some(files),
        Ok(Err(error)) => {
            tracing::warn!(
                error = format!("{error:#}"),
                "cannot write the default effects"
            );
            None
        }
        Err(error) => {
            tracing::warn!(%error, "writing the default effects stopped abnormally");
            None
        }
    }
}

fn start_discord(path: &Path) -> anyhow::Result<SongbirdDiscordClient> {
    if !path.exists() {
        tracing::info!("no Discord credentials; only local output is available");
        return Ok(SongbirdDiscordClient::not_configured());
    }
    let credentials = DiscordCredentials::load(path)
        .with_context(|| format!("loading Discord credentials from {}", path.display()))?;
    Ok(SongbirdDiscordClient::start(credentials))
}

/// The log folder: the state directory, or the local data directory where the platform has
/// none.
#[must_use]
pub fn log_directory() -> Option<PathBuf> {
    let directories = directories::ProjectDirs::from("", "", "tufo")?;
    Some(
        directories
            .state_dir()
            .unwrap_or_else(|| directories.data_local_dir())
            .to_path_buf(),
    )
}

fn default_discord_credentials() -> Option<PathBuf> {
    Some(
        directories::ProjectDirs::from("", "", "tufo")?
            .config_dir()
            .join(DISCORD_CREDENTIALS_FILE_NAME),
    )
}

#[cfg(unix)]
async fn shutdown_signal() -> anyhow::Result<()> {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .context("installing the SIGTERM handler")?;
    tokio::select! {
        result = tokio::signal::ctrl_c() => result.context("waiting for SIGINT"),
        _ = terminate.recv() => Ok(()),
    }
}

/// Ctrl+C, closing the server's console, and system shutdown all stop the server. Windows ends the
/// process about 5 s after the last two, whether or not draining has finished.
#[cfg(windows)]
async fn shutdown_signal() -> anyhow::Result<()> {
    let mut close =
        tokio::signal::windows::ctrl_close().context("installing the console close handler")?;
    let mut shutdown = tokio::signal::windows::ctrl_shutdown()
        .context("installing the system shutdown handler")?;
    tokio::select! {
        result = tokio::signal::ctrl_c() => result.context("waiting for Ctrl+C"),
        _ = close.recv() => Ok(()),
        _ = shutdown.recv() => Ok(()),
    }
}
