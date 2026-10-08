//! Changing the player's volumes, mutes, shuffle, repeat, and crossfade, which survive restarts.

use toasty::Db;
use tufo_models::playback::player_settings::{PLAYER_SETTINGS_ID, SavedPlayerSettings};
use tufo_wire::player::UpdatePlayerSettings;

use super::{
    control_browser::BrowserCommand,
    control_music::{self, ControlMusicError, MusicCommand},
    control_sounds::SoundsCommand,
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
            .effects_volume(state.effects_volume)
            .muted(state.muted)
            .shuffle(state.modes.shuffle)
            .repeat(state.modes.repeat)
            .crossfade(state.crossfade)
            .browser_volume(state.browser.volume)
            .browser_muted(state.browser.muted)
            .exec(&mut database)
            .await
    } else {
        toasty::create!(SavedPlayerSettings {
            id: PLAYER_SETTINGS_ID,
            volume: state.volume,
            effects_volume: state.effects_volume,
            muted: state.muted,
            shuffle: state.modes.shuffle,
            repeat: state.modes.repeat,
            crossfade: state.crossfade,
            browser_volume: state.browser.volume,
            browser_muted: state.browser.muted,
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
            effects_volume: Some(saved.effects_volume),
            muted: Some(saved.muted),
            shuffle: Some(saved.shuffle),
            repeat: Some(saved.repeat),
            crossfade: Some(saved.crossfade),
            browser_volume: Some(saved.browser_volume),
            browser_muted: Some(saved.browser_muted),
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
        effects_volume,
        muted,
        shuffle,
        repeat,
        crossfade,
        browser_volume,
        browser_muted,
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
    if let Some(volume) = effects_volume {
        player
            .control_sounds(SoundsCommand::SetVolume(volume))
            .await;
    }
    let browser_commands = [
        browser_volume.map(BrowserCommand::SetVolume),
        browser_muted.map(BrowserCommand::SetMuted),
    ];
    for command in browser_commands.into_iter().flatten() {
        player.control_browser(command).await;
    }
    Ok(())
}
