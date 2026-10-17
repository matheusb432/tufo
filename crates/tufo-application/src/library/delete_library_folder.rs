//! Stopping following a folder. Audio files on disk are never touched.
//!
//! The folder's linked collections go, deleted ones included. Its files stay only while
//! something else keeps them: a hand-picked collection that lists them, their own path as song
//! sources, or another followed folder.

use toasty_core::driver::operation::TransactionMode;
use tufo_models::library::{Collection, LibraryFolder, LibraryFolderId};

use super::{Library, LibraryStorageError, library_files::LibraryFiles, store};
use crate::error::ErrorMeta;

#[derive(Debug, thiserror::Error, ErrorMeta)]
pub enum DeleteLibraryFolderError {
    #[error("that folder is not followed")]
    #[meta(class = NotFound, reason = Self::NOT_FOUND)]
    NotFound,
    #[error(transparent)]
    #[meta(transparent)]
    Storage(#[from] LibraryStorageError),
}

impl DeleteLibraryFolderError {
    pub const NOT_FOUND: &'static str = "library.delete_library_folder.not_found";
}

pub async fn execute<F: LibraryFiles>(
    id: LibraryFolderId,
    library: &Library<F>,
) -> Result<(), DeleteLibraryFolderError> {
    let _change = library.begin_change().await;
    let mut database = library.database();
    let mut transaction = database
        .transaction_builder()
        .mode(TransactionMode::Immediate)
        .begin()
        .await
        .map_err(LibraryStorageError::from)?;
    LibraryFolder::filter_by_id(id)
        .first()
        .exec(&mut transaction)
        .await
        .map_err(LibraryStorageError::from)?
        .ok_or(DeleteLibraryFolderError::NotFound)?;
    // Toasty 0.10 can't serialize a filter on an optional UUID column, so this filters in memory.
    let linked = Collection::all()
        .exec(&mut transaction)
        .await
        .map_err(LibraryStorageError::from)?
        .into_iter()
        .filter(|collection| collection.library_folder_id == Some(id));
    for collection in linked {
        Collection::filter_by_id(collection.id)
            .delete()
            .exec(&mut transaction)
            .await
            .map_err(LibraryStorageError::from)?;
    }
    LibraryFolder::filter_by_id(id)
        .delete()
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
