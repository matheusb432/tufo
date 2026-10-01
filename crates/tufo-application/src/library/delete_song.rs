//! Removing a song added by its own path from the songs. Playlists that list it keep it, and
//! the file on disk is never touched.

use toasty_core::driver::operation::TransactionMode;
use tufo_models::{
    library::{AudioFile, AudioFileId, CollectionKind, LibraryFolder},
    path::FolderPath,
};

use super::{Library, LibraryStorageError, library_files::LibraryFiles, store};
use crate::error::ErrorMeta;

#[derive(Debug, thiserror::Error, ErrorMeta)]
pub enum DeleteSongError {
    #[error("the song does not exist")]
    #[meta(class = NotFound, reason = Self::NOT_FOUND)]
    NotFound,
    /// A followed folder keeps every file it holds.
    #[error("a followed folder holds the song; stop following the folder instead")]
    #[meta(class = FailedPrecondition, reason = Self::IN_FOLLOWED_FOLDER)]
    InFollowedFolder { folder: FolderPath },
    #[error(transparent)]
    #[meta(transparent)]
    Storage(#[from] LibraryStorageError),
}

impl DeleteSongError {
    pub const NOT_FOUND: &'static str = "library.delete_song.not_found";
    pub const IN_FOLLOWED_FOLDER: &'static str = "library.delete_song.in_followed_folder";
}

pub async fn execute<F: LibraryFiles>(
    id: AudioFileId,
    library: &Library<F>,
) -> Result<(), DeleteSongError> {
    let _change = library.begin_change().await;
    let mut database = library.database();
    let mut transaction = database
        .transaction_builder()
        .mode(TransactionMode::Immediate)
        .begin()
        .await
        .map_err(LibraryStorageError::from)?;
    let file = AudioFile::filter_by_id(id)
        .first()
        .exec(&mut transaction)
        .await
        .map_err(LibraryStorageError::from)?
        .ok_or(DeleteSongError::NotFound)?;
    let music_folders = LibraryFolder::all()
        .exec(&mut transaction)
        .await
        .map_err(LibraryStorageError::from)?
        .into_iter()
        .filter(|folder| folder.kind == CollectionKind::Playlist)
        .collect::<Vec<_>>();
    if file.missing_since.is_none()
        && let Some(folder) = store::holding_folder(&file.file_path, &music_folders)
    {
        return Err(DeleteSongError::InFollowedFolder {
            folder: folder.folder_path.clone(),
        });
    }
    if !file.song_source {
        return Err(DeleteSongError::NotFound);
    }
    AudioFile::update_by_id(id)
        .song_source(false)
        .exec(&mut transaction)
        .await
        .map_err(LibraryStorageError::from)?;
    store::delete_unkept_files(&mut transaction).await?;
    transaction
        .commit()
        .await
        .map_err(LibraryStorageError::from)?;
    library.publish().await?;
    Ok(())
}
