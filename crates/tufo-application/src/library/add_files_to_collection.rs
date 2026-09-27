//! Adding dropped files and folders to one manual collection.

use tufo_models::path::FolderPath;
use tufo_wire::library::{AddFilesToCollection, ImportReport};

use super::{Library, LibraryStorageError, library_files::LibraryFiles, manual_files, store};
use crate::error::{ApplicationError, ErrorClass, ErrorMetadata};

#[derive(Debug, thiserror::Error)]
pub enum AddFilesToCollectionError {
    #[error("the collection does not exist")]
    NotFound,
    /// Tufo never copies files, so a linked collection only changes with its folder.
    #[error("the collection mirrors a folder; add the files to that folder instead")]
    Linked { folder: FolderPath },
    #[error(transparent)]
    Storage(#[from] LibraryStorageError),
}

impl AddFilesToCollectionError {
    pub const NOT_FOUND: &'static str = "library.add_files_to_collection.not_found";
    pub const LINKED: &'static str = "library.add_files_to_collection.linked";
}

impl ApplicationError for AddFilesToCollectionError {
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

/// Adds every audio file among `paths`, and under the folders among them, to the collection.
pub async fn execute<F: LibraryFiles>(
    request: AddFilesToCollection,
    library: &Library<F>,
) -> Result<ImportReport, AddFilesToCollectionError> {
    let _change = library.begin_change().await;
    let mut database = library.database();
    let collection = store::find_visible_collection(&mut database, request.collection_id)
        .await?
        .ok_or(AddFilesToCollectionError::NotFound)?;
    if let Some(folder) = collection.folder_path {
        return Err(AddFilesToCollectionError::Linked { folder });
    }
    let dropped = manual_files::collect_audio_files(library.files(), request.paths).await;
    let added = manual_files::add_files(library, collection.id, dropped.audio_files).await?;
    library.publish().await?;
    Ok(ImportReport {
        collections_created: 0,
        files_added: added.files_added,
        unreadable_files: added.unreadable,
        ignored_paths: dropped.ignored,
    })
}
