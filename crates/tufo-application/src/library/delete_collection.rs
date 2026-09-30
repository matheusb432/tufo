//! Deleting a collection. Audio files on disk are never touched.
//!
//! A manual collection is deleted outright. A linked collection keeps a hidden row, so Tufo
//! stops following its folder instead of recreating it on the next scan; dropping the folder
//! again follows it once more.

use jiff::Timestamp;
use toasty_core::driver::operation::TransactionMode;
use tufo_models::library::{Collection, CollectionEntry, CollectionId};

use super::{Library, LibraryStorageError, library_files::LibraryFiles, store};
use crate::error::ErrorMeta;

#[derive(Debug, thiserror::Error, ErrorMeta)]
pub enum DeleteCollectionError {
    #[error("the collection does not exist")]
    #[meta(class = NotFound, reason = Self::NOT_FOUND)]
    NotFound,
    #[error(transparent)]
    #[meta(transparent)]
    Storage(#[from] LibraryStorageError),
}

impl DeleteCollectionError {
    pub const NOT_FOUND: &'static str = "library.delete_collection.not_found";
}

pub async fn execute<F: LibraryFiles>(
    id: CollectionId,
    library: &Library<F>,
) -> Result<(), DeleteCollectionError> {
    let _change = library.begin_change().await;
    let mut database = library.database();
    let mut transaction = database
        .transaction_builder()
        .mode(TransactionMode::Immediate)
        .begin()
        .await
        .map_err(LibraryStorageError::from)?;
    let collection = store::find_visible_collection(&mut transaction, id)
        .await?
        .ok_or(DeleteCollectionError::NotFound)?;
    if collection.library_folder_id.is_some() {
        CollectionEntry::filter_by_collection_id(id)
            .delete()
            .exec(&mut transaction)
            .await
            .map_err(LibraryStorageError::from)?;
        Collection::update_by_id(id)
            .hidden_at(Some(Timestamp::now()))
            .exec(&mut transaction)
            .await
            .map_err(LibraryStorageError::from)?;
    } else {
        Collection::filter_by_id(id)
            .delete()
            .exec(&mut transaction)
            .await
            .map_err(LibraryStorageError::from)?;
    }
    store::delete_unlisted_files(&mut transaction).await?;
    transaction
        .commit()
        .await
        .map_err(LibraryStorageError::from)?;
    library.publish().await?;
    Ok(())
}
