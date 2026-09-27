//! Deleting an entry from a manual collection. The file on disk is never touched.

use toasty_core::driver::operation::TransactionMode;
use tufo_models::{
    library::{Collection, CollectionEntry, CollectionEntryId},
    path::FolderPath,
};

use super::{Library, LibraryStorageError, library_files::LibraryFiles, store};
use crate::error::{ApplicationError, ErrorClass, ErrorMetadata};

#[derive(Debug, thiserror::Error)]
pub enum DeleteCollectionEntryError {
    #[error("the entry does not exist")]
    NotFound,
    /// A linked collection lists exactly its folder's files.
    #[error("the collection mirrors a folder; remove the file from that folder instead")]
    Linked { folder: FolderPath },
    #[error(transparent)]
    Storage(#[from] LibraryStorageError),
}

impl DeleteCollectionEntryError {
    pub const NOT_FOUND: &'static str = "library.delete_collection_entry.not_found";
    pub const LINKED: &'static str = "library.delete_collection_entry.linked";
}

impl ApplicationError for DeleteCollectionEntryError {
    fn metadata(&self) -> ErrorMetadata {
        match self {
            Self::NotFound => ErrorMetadata {
                class: ErrorClass::NotFound,
                reason: Self::NOT_FOUND,
            },
            Self::Linked { .. } => ErrorMetadata {
                class: ErrorClass::FailedPrecondition,
                reason: Self::LINKED,
            },
            Self::Storage(error) => error.metadata(),
        }
    }
}

pub async fn execute<F: LibraryFiles>(
    id: CollectionEntryId,
    library: &Library<F>,
) -> Result<(), DeleteCollectionEntryError> {
    let _change = library.begin_change().await;
    let mut database = library.database();
    let mut transaction = database
        .transaction_builder()
        .mode(TransactionMode::Immediate)
        .begin()
        .await
        .map_err(LibraryStorageError::from)?;
    let entry = CollectionEntry::filter_by_id(id)
        .first()
        .exec(&mut transaction)
        .await
        .map_err(LibraryStorageError::from)?
        .ok_or(DeleteCollectionEntryError::NotFound)?;
    let collection = Collection::filter_by_id(entry.collection_id)
        .first()
        .exec(&mut transaction)
        .await
        .map_err(LibraryStorageError::from)?
        .ok_or(DeleteCollectionEntryError::NotFound)?;
    if let Some(folder) = collection.folder_path {
        return Err(DeleteCollectionEntryError::Linked { folder });
    }
    CollectionEntry::filter_by_id(id)
        .delete()
        .exec(&mut transaction)
        .await
        .map_err(LibraryStorageError::from)?;
    store::delete_unlisted_files(&mut transaction).await?;
    transaction
        .commit()
        .await
        .map_err(LibraryStorageError::from)?;
    library.publish().await?;
    Ok(())
}
