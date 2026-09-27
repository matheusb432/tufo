//! Playing a playlist's tracks in its order.

use tufo_models::{
    library::{CollectionEntryId, CollectionId, CollectionKind},
    playback::queue::{QueueEntry, QueueStart},
};

use super::{
    play_queue::{self, PlayQueueError},
    player::Player,
};
use crate::{
    error::{ApplicationError, ErrorClass, ErrorMetadata},
    library::{Library, library_files::LibraryFiles},
};

#[derive(Debug, thiserror::Error)]
pub enum PlayCollectionError {
    #[error("the playlist does not exist")]
    NotFound,
    #[error("the playlist has no track Tufo can find on disk")]
    NothingPlayable,
    #[error("the track to start from is not in the playlist or its file is missing")]
    StartNotPlayable,
    #[error(transparent)]
    Play(#[from] PlayQueueError),
}

impl PlayCollectionError {
    pub const NOT_FOUND: &'static str = "playback.play_collection.not_found";
    pub const NOTHING_PLAYABLE: &'static str = "playback.play_collection.nothing_playable";
    pub const START_NOT_PLAYABLE: &'static str = "playback.play_collection.start_not_playable";
}

impl ApplicationError for PlayCollectionError {
    fn metadata(&self) -> ErrorMetadata {
        match self {
            Self::NotFound => ErrorMetadata {
                class: ErrorClass::NotFound,
                reason: Self::NOT_FOUND,
            },
            Self::NothingPlayable => ErrorMetadata {
                class: ErrorClass::FailedPrecondition,
                reason: Self::NOTHING_PLAYABLE,
            },
            Self::StartNotPlayable => ErrorMetadata {
                class: ErrorClass::FailedPrecondition,
                reason: Self::START_NOT_PLAYABLE,
            },
            Self::Play(error) => error.metadata(),
        }
    }
}

/// Replaces the music with the playlist's tracks, skipping missing files, starting at
/// `start` or at the first playable track.
pub async fn execute<F: LibraryFiles>(
    collection_id: CollectionId,
    start: Option<CollectionEntryId>,
    library: &Library<F>,
    player: &impl Player,
) -> Result<(), PlayCollectionError> {
    let queue = {
        let snapshot = library.watch().borrow().clone();
        let collection = snapshot
            .collections
            .iter()
            .find(|collection| {
                collection.id == collection_id && collection.kind == CollectionKind::Playlist
            })
            .ok_or(PlayCollectionError::NotFound)?;
        let entries = collection
            .entries
            .iter()
            .filter(|entry| !entry.missing)
            .map(|entry| QueueEntry {
                entry_id: entry.id,
                path: entry.path.clone(),
                title: entry.title.as_str().to_owned(),
                duration: Some(entry.duration),
            })
            .collect::<Vec<_>>();
        if entries.is_empty() {
            return Err(PlayCollectionError::NothingPlayable);
        }
        let start_index = match start {
            None => 0,
            Some(start) => entries
                .iter()
                .position(|entry| entry.entry_id == start)
                .ok_or(PlayCollectionError::StartNotPlayable)?,
        };
        QueueStart {
            collection_id,
            entries,
            start_index,
        }
    };
    play_queue::execute(queue, player).await?;
    Ok(())
}
