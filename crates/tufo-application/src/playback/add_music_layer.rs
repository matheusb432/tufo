//! Playing one playlist song alongside the music already playing.

use tufo_models::{
    library::{CollectionEntryId, CollectionId, CollectionKind},
    playback::queue::QueueEntry,
};

use super::{
    play_queue::PlayQueueError,
    player::{PlayAudioError, Player},
};
use crate::{
    error::ErrorMeta,
    library::{Library, library_files::LibraryFiles},
};

#[derive(Debug, Clone)]
pub struct MusicLayerCue {
    pub collection_id: CollectionId,
    pub entry: QueueEntry,
}

#[derive(Debug, thiserror::Error, ErrorMeta)]
pub enum AddMusicLayerError {
    #[error("the playlist song does not exist")]
    #[meta(class = NotFound, reason = Self::NOT_FOUND)]
    NotFound,
    #[error("the song's audio file is missing")]
    #[meta(class = FailedPrecondition, reason = Self::MISSING)]
    Missing,
    #[error("the music mix already has three layers")]
    #[meta(class = FailedPrecondition, reason = Self::FULL)]
    Full,
    #[error(transparent)]
    #[meta(transparent)]
    Audio(#[from] PlayAudioError),
    #[error(transparent)]
    #[meta(transparent)]
    Queue(#[from] PlayQueueError),
}

impl AddMusicLayerError {
    pub const NOT_FOUND: &'static str = "playback.add_music_layer.not_found";
    pub const MISSING: &'static str = "playback.add_music_layer.missing";
    pub const FULL: &'static str = "playback.add_music_layer.full";
}

/// Adding a song already in the mix succeeds without restarting it.
pub async fn execute<F: LibraryFiles>(
    entry_id: CollectionEntryId,
    library: &Library<F>,
    player: &impl Player,
) -> Result<(), AddMusicLayerError> {
    let cue = {
        let snapshot = library.watch().borrow().clone();
        let (collection, entry) = snapshot
            .collections
            .iter()
            .filter(|collection| collection.kind == CollectionKind::Playlist)
            .find_map(|collection| {
                collection
                    .entries
                    .iter()
                    .find(|entry| entry.id == entry_id)
                    .map(|entry| (collection, entry))
            })
            .ok_or(AddMusicLayerError::NotFound)?;
        if entry.missing {
            return Err(AddMusicLayerError::Missing);
        }
        MusicLayerCue {
            collection_id: collection.id,
            entry: QueueEntry {
                entry_id,
                path: entry.path.clone(),
                title: entry.title.as_str().to_owned(),
                duration: Some(entry.duration),
            },
        }
    };
    player.add_music_layer(cue).await
}
