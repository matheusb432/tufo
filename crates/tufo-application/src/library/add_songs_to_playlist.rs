//! Adding songs the library already holds to a hand-picked playlist.

use std::collections::HashSet;

use jiff::Timestamp;
use toasty_core::driver::operation::TransactionMode;
use tufo_models::{
    library::{AudioFile, CollectionEntry, CollectionKind, SoundIcon, SoundSettings},
    path::FolderPath,
};
use tufo_wire::library::AddSongsToPlaylist;

use super::{Library, LibraryStorageError, library_files::LibraryFiles, store};
use crate::error::ErrorMeta;

#[derive(Debug, thiserror::Error, ErrorMeta)]
pub enum AddSongsToPlaylistError {
    #[error("the playlist does not exist")]
    #[meta(class = NotFound, reason = Self::PLAYLIST_NOT_FOUND)]
    PlaylistNotFound,
    #[error("a song does not exist")]
    #[meta(class = NotFound, reason = Self::SONG_NOT_FOUND)]
    SongNotFound,
    /// A linked playlist lists exactly its folder's files.
    #[error("the playlist mirrors a folder; add the files to that folder instead")]
    #[meta(class = FailedPrecondition, reason = Self::LINKED)]
    Linked { folder: FolderPath },
    #[error(transparent)]
    #[meta(transparent)]
    Storage(#[from] LibraryStorageError),
}

impl AddSongsToPlaylistError {
    pub const PLAYLIST_NOT_FOUND: &'static str = "library.add_songs_to_playlist.playlist_not_found";
    pub const SONG_NOT_FOUND: &'static str = "library.add_songs_to_playlist.song_not_found";
    pub const LINKED: &'static str = "library.add_songs_to_playlist.linked";
}

/// Lists each song in the playlist, skipping the songs it already lists, and counts the songs
/// it added.
pub async fn execute<F: LibraryFiles>(
    request: AddSongsToPlaylist,
    library: &Library<F>,
) -> Result<usize, AddSongsToPlaylistError> {
    let _change = library.begin_change().await;
    let mut database = library.database();
    let mut transaction = database
        .transaction_builder()
        .mode(TransactionMode::Immediate)
        .begin()
        .await
        .map_err(LibraryStorageError::from)?;
    let playlist = store::find_visible_collection(&mut transaction, request.playlist_id)
        .await?
        .filter(|collection| collection.kind == CollectionKind::Playlist)
        .ok_or(AddSongsToPlaylistError::PlaylistNotFound)?;
    if let Some(folder) = playlist.folder_path {
        return Err(AddSongsToPlaylistError::Linked { folder });
    }
    let mut listed = CollectionEntry::filter_by_collection_id(playlist.id)
        .exec(&mut transaction)
        .await
        .map_err(LibraryStorageError::from)?
        .into_iter()
        .map(|entry| entry.audio_file_id)
        .collect::<HashSet<_>>();
    let now = Timestamp::now();
    let mut songs_added = 0;
    let mut positions = store::EntryPositionsAtEnd::default();
    for audio_file_id in request.audio_file_ids {
        AudioFile::filter_by_id(audio_file_id)
            .first()
            .exec(&mut transaction)
            .await
            .map_err(LibraryStorageError::from)?
            .ok_or(AddSongsToPlaylistError::SongNotFound)?;
        if listed.insert(audio_file_id) {
            store::create_entry(
                &mut transaction,
                &mut positions,
                playlist.id,
                audio_file_id,
                now,
                SoundSettings::default(),
                SoundIcon::default(),
            )
            .await?;
            songs_added += 1;
        }
    }
    transaction
        .commit()
        .await
        .map_err(LibraryStorageError::from)?;
    library.publish().await?;
    Ok(songs_added)
}
