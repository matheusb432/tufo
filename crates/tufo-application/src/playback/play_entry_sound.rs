//! Firing one soundboard entry with its own settings.

use tufo_models::{library::CollectionEntryId, playback::player_state::SoundCue};

use super::{
    play_sound,
    player::{PlayAudioError, Player},
    saved_loops,
};
use crate::{
    error::{ApplicationError, ErrorClass, ErrorMetadata},
    library::{Library, library_files::LibraryFiles},
};

#[derive(Debug, thiserror::Error)]
pub enum PlayEntrySoundError {
    #[error("the entry does not exist")]
    NotFound,
    #[error("the entry's audio file is missing")]
    Missing,
    #[error(transparent)]
    Audio(#[from] PlayAudioError),
}

impl PlayEntrySoundError {
    pub const NOT_FOUND: &'static str = "playback.play_entry_sound.not_found";
    pub const MISSING: &'static str = "playback.play_entry_sound.missing";
}

impl ApplicationError for PlayEntrySoundError {
    fn metadata(&self) -> ErrorMetadata {
        match self {
            Self::NotFound => ErrorMetadata {
                class: ErrorClass::NotFound,
                reason: Self::NOT_FOUND,
            },
            Self::Missing => ErrorMetadata {
                class: ErrorClass::FailedPrecondition,
                reason: Self::MISSING,
            },
            Self::Audio(error) => error.metadata(),
        }
    }
}

/// Fires the entry's file with its loop, volume, and fades. Firing a playing entry restarts it.
pub async fn execute<F: LibraryFiles>(
    entry_id: CollectionEntryId,
    library: &Library<F>,
    player: &impl Player,
) -> Result<(), PlayEntrySoundError> {
    let cue = {
        let snapshot = library.watch().borrow().clone();
        let entry = snapshot
            .collections
            .iter()
            .flat_map(|collection| &collection.entries)
            .find(|entry| entry.id == entry_id)
            .ok_or(PlayEntrySoundError::NotFound)?;
        if entry.missing {
            return Err(PlayEntrySoundError::Missing);
        }
        SoundCue {
            entry_id,
            path: entry.path.clone(),
            settings: entry.sound,
        }
    };
    play_sound::execute(cue, player).await?;
    let state = player.watch_player().borrow().clone();
    let mut database = library.database();
    saved_loops::remember_if_looping_locally(&mut database, entry_id, &state).await;
    saved_loops::remember_paused(&mut database, state.sounds_paused).await;
    Ok(())
}
