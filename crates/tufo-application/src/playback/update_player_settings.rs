//! Changing the player's volume, mute, shuffle, repeat, and crossfade, which survive restarts.

use toasty::Db;
use tufo_models::playback::player_settings::{PLAYER_SETTINGS_ID, SavedPlayerSettings};
use tufo_wire::player::UpdatePlayerSettings;

use super::{
    control_music::{self, ControlMusicError, MusicCommand},
    player::Player,
};
use crate::{error::ErrorMeta, library::LibraryStorageError};

#[derive(Debug, thiserror::Error, ErrorMeta)]
pub enum UpdatePlayerSettingsError {
    #[error(transparent)]
    #[meta(transparent)]
    Control(#[from] ControlMusicError),
    #[error(transparent)]
    #[meta(transparent)]
    Storage(#[from] LibraryStorageError),
}

/// Applies each setting present in `update`, in a fixed order, then saves the player's settings.
/// Settings need no music.
pub async fn execute(
    update: UpdatePlayerSettings,
    player: &impl Player,
    mut database: Db,
) -> Result<(), UpdatePlayerSettingsError> {
    apply(update, player).await?;
    let state = player.watch_player().borrow().clone();
    let saved = SavedPlayerSettings::filter_by_id(PLAYER_SETTINGS_ID)
        .first()
        .exec(&mut database)
        .await
        .map_err(LibraryStorageError::from)?;
    let result = if saved.is_some() {
        SavedPlayerSettings::update_by_id(PLAYER_SETTINGS_ID)
            .volume(state.volume)
            .muted(state.muted)
            .shuffle(state.modes.shuffle)
            .repeat(state.modes.repeat)
            .crossfade(state.crossfade)
            .exec(&mut database)
            .await
    } else {
        toasty::create!(SavedPlayerSettings {
            id: PLAYER_SETTINGS_ID,
            volume: state.volume,
            muted: state.muted,
            shuffle: state.modes.shuffle,
            repeat: state.modes.repeat,
            crossfade: state.crossfade,
        })
        .exec(&mut database)
        .await
        .map(drop)
    };
    result.map_err(LibraryStorageError::from)?;
    Ok(())
}

/// Gives the player the settings it had before the server last stopped, if any were saved.
pub async fn restore(
    player: &impl Player,
    mut database: Db,
) -> Result<(), UpdatePlayerSettingsError> {
    let saved = SavedPlayerSettings::filter_by_id(PLAYER_SETTINGS_ID)
        .first()
        .exec(&mut database)
        .await
        .map_err(LibraryStorageError::from)?;
    let Some(saved) = saved else {
        return Ok(());
    };
    apply(
        UpdatePlayerSettings {
            volume: Some(saved.volume),
            muted: Some(saved.muted),
            shuffle: Some(saved.shuffle),
            repeat: Some(saved.repeat),
            crossfade: Some(saved.crossfade),
        },
        player,
    )
    .await?;
    Ok(())
}

async fn apply(
    update: UpdatePlayerSettings,
    player: &impl Player,
) -> Result<(), ControlMusicError> {
    let UpdatePlayerSettings {
        volume,
        muted,
        shuffle,
        repeat,
        crossfade,
    } = update;
    let commands = [
        volume.map(MusicCommand::SetVolume),
        muted.map(MusicCommand::SetMuted),
        shuffle.map(MusicCommand::SetShuffle),
        repeat.map(MusicCommand::SetRepeat),
        crossfade.map(MusicCommand::SetCrossfade),
    ];
    for command in commands.into_iter().flatten() {
        control_music::execute(command, player).await?;
    }
    Ok(())
}
