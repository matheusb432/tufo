//! Playing a playlist's tracks in its order.

use tufo_models::{
    library::{CollectionEntryId, CollectionId, CollectionKind},
    playback::queue::QueueStart,
};

use super::{
    play_queue::{self, PlayQueueError},
    player::Player,
    playlist_song::queue_entry,
};
use crate::{
    error::ErrorMeta,
    library::{Library, library_files::LibraryFiles},
};

#[derive(Debug, thiserror::Error, ErrorMeta)]
pub enum PlayCollectionError {
    #[error("the playlist does not exist")]
    #[meta(class = NotFound, reason = Self::NOT_FOUND)]
    NotFound,
    #[error("the playlist has no track Tufo can find on disk")]
    #[meta(class = FailedPrecondition, reason = Self::NOTHING_PLAYABLE)]
    NothingPlayable,
    #[error("the track to start from is not in the playlist or its file is missing")]
    #[meta(class = FailedPrecondition, reason = Self::START_NOT_PLAYABLE)]
    StartNotPlayable,
    #[error(transparent)]
    #[meta(transparent)]
    Play(#[from] PlayQueueError),
}

impl PlayCollectionError {
    pub const NOT_FOUND: &'static str = "playback.play_collection.not_found";
    pub const NOTHING_PLAYABLE: &'static str = "playback.play_collection.nothing_playable";
    pub const START_NOT_PLAYABLE: &'static str = "playback.play_collection.start_not_playable";
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
            .map(queue_entry)
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
