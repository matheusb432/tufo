//! Finding a song in the playlists and describing it for the player.

use tufo_models::{
    library::{CollectionEntryId, CollectionKind},
    playback::queue::QueueEntry,
};
use tufo_wire::library::{CollectionView, EntryView, LibrarySnapshot};

/// The playlist holding `entry_id`, and the entry.
pub(super) fn find_playlist_song(
    snapshot: &LibrarySnapshot,
    entry_id: CollectionEntryId,
) -> Option<(&CollectionView, &EntryView)> {
    snapshot
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
}

pub(super) fn queue_entry(entry: &EntryView) -> QueueEntry {
    QueueEntry {
        entry_id: entry.id,
        audio_file_id: entry.audio_file_id,
        path: entry.path.clone(),
        title: entry.title.as_str().to_owned(),
        duration: Some(entry.duration),
    }
}
