//! Creating an empty manual collection.

use tufo_models::library::CollectionId;
use tufo_wire::library::CreateCollection;

use super::{Library, LibraryStorageError, library_files::LibraryFiles, store};

pub async fn execute<F: LibraryFiles>(
    request: CreateCollection,
    library: &Library<F>,
) -> Result<CollectionId, LibraryStorageError> {
    let _change = library.begin_change().await;
    let mut database = library.database();
    let collection = store::create_manual_collection(
        &mut database,
        request.kind,
        request.title,
        request.background,
        None,
    )
    .await?;
    library.publish().await?;
    Ok(collection.id)
}
