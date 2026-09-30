//! Queuing a playlist song to play after the current track and the songs queued before it.

use tufo_models::{library::CollectionEntryId, playback::queue::QueuedEntry};

use super::{
    play_collection::{self, PlayCollectionError},
    player::Player,
    playlist_song::{find_playlist_song, queue_entry},
};
use crate::{
    error::ErrorMeta,
    library::{Library, library_files::LibraryFiles},
};

/// Why the player didn't queue a song.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum QueueSongError {
    #[error("no music is playing")]
    NothingPlaying,
    #[error("songs can't be queued while one track repeats")]
    RepeatingTrack,
}

#[derive(Debug, thiserror::Error, ErrorMeta)]
pub enum AddToQueueError {
    #[error("the playlist song does not exist")]
    #[meta(class = NotFound, reason = Self::NOT_FOUND)]
    NotFound,
    #[error("the song's audio file is missing")]
    #[meta(class = FailedPrecondition, reason = Self::MISSING)]
    Missing,
    #[error("songs can't be queued while one track repeats")]
    #[meta(class = FailedPrecondition, reason = Self::REPEATING_TRACK)]
    RepeatingTrack,
    #[error(transparent)]
    #[meta(transparent)]
    Play(#[from] PlayCollectionError),
}

impl AddToQueueError {
    pub const NOT_FOUND: &'static str = "playback.add_to_queue.not_found";
    pub const MISSING: &'static str = "playback.add_to_queue.missing";
    pub const REPEATING_TRACK: &'static str = "playback.add_to_queue.repeating_track";
}

/// With nothing playing, the song starts now and its playlist follows it. Queuing a queued song
/// leaves it in place.
pub async fn execute<F: LibraryFiles>(
    entry_id: CollectionEntryId,
    library: &Library<F>,
    player: &impl Player,
) -> Result<(), AddToQueueError> {
    let song = {
        let snapshot = library.watch().borrow().clone();
        let (collection, entry) =
            find_playlist_song(&snapshot, entry_id).ok_or(AddToQueueError::NotFound)?;
        if entry.missing {
            return Err(AddToQueueError::Missing);
        }
        QueuedEntry {
            collection_id: collection.id,
            entry: queue_entry(entry),
        }
    };
    let collection_id = song.collection_id;
    match player.add_to_queue(song).await {
        Ok(()) => Ok(()),
        Err(QueueSongError::RepeatingTrack) => Err(AddToQueueError::RepeatingTrack),
        Err(QueueSongError::NothingPlaying) => {
            play_collection::execute(collection_id, Some(entry_id), library, player).await?;
            Ok(())
        }
    }
}
