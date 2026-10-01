//! Kenku FM's data folder, whose `redux-persist` state lives in Chromium's `localStorage`.

mod file_urls;
mod local_storage;
mod persisted_state;

use std::path::{Path, PathBuf};

use anyhow::anyhow;
use tufo_application::library::import_kenku_library::{
    KenkuLibrary,
    kenku_data::{KenkuData, KenkuReader, ReadKenkuDataError},
};
use tufo_models::path::FolderPath;

#[cfg(any(test, feature = "test-support"))]
pub use self::local_storage::write_file_origin_items;
use self::local_storage::{LocalStorageCopy, OpenLocalStorageError};

const KENKU_FOLDER_NAME: &str = "Kenku FM";
/// Collections, music settings, and settings.
pub const PLAYER_STATE_KEY: &str = "persist:player";
pub const PLAYBACK_STATE_KEY: &str = "persist:playback";
pub const SETTINGS_STATE_KEY: &str = "persist:root";

/// `Kenku FM` in the system's config folder, such as `%APPDATA%` on Windows.
#[must_use]
pub fn default_kenku_folder() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|folders| folders.config_dir().join(KENKU_FOLDER_NAME))
}

/// Blocks; settings that don't parse are left out.
pub fn read_kenku_data(folder: &Path) -> Result<KenkuData, ReadKenkuDataError> {
    let mut storage = LocalStorageCopy::open(folder).map_err(|error| match error {
        OpenLocalStorageError::NotFound => ReadKenkuDataError::NotFound,
        OpenLocalStorageError::Running => ReadKenkuDataError::Running,
        other => ReadKenkuDataError::Unreadable(anyhow!(other)),
    })?;
    let player_state = storage
        .file_origin_item(PLAYER_STATE_KEY)
        .ok_or(ReadKenkuDataError::NotFound)?;
    let collections = persisted_state::collections(&player_state)
        .map_err(|error| ReadKenkuDataError::Malformed(anyhow!(error)))?;
    let player_settings = storage
        .file_origin_item(PLAYBACK_STATE_KEY)
        .and_then(|state| {
            persisted_state::player_settings(&state)
                .inspect_err(|error| tracing::warn!(%error, "skipping Kenku FM's music settings"))
                .ok()
        })
        .unwrap_or_default();
    let discord_bot_token = storage
        .file_origin_item(SETTINGS_STATE_KEY)
        .and_then(|state| {
            persisted_state::discord_bot_token(&state)
                .inspect_err(|error| tracing::warn!(%error, "skipping Kenku FM's settings"))
                .ok()
        })
        .flatten();
    Ok(KenkuData {
        library: KenkuLibrary {
            collections,
            player_settings,
        },
        discord_bot_token,
    })
}

#[derive(Debug, Clone, Copy)]
pub struct DiskKenkuReader;

impl KenkuReader for DiskKenkuReader {
    async fn read(&self, folder: Option<FolderPath>) -> Result<KenkuData, ReadKenkuDataError> {
        let folder = folder
            .map(|folder| folder.as_ref().clone())
            .or_else(default_kenku_folder)
            .ok_or(ReadKenkuDataError::NotFound)?;
        tokio::task::spawn_blocking(move || read_kenku_data(&folder))
            .await
            .map_err(|error| ReadKenkuDataError::Internal(error.into()))?
    }
}
