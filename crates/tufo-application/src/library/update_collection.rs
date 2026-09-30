//! Retitling a collection, changing its background, or reordering its entries.

use tufo_models::library::{BuiltinTitle, Collection, CollectionId};
use tufo_wire::library::UpdateCollection;

use super::{Library, LibraryStorageError, library_files::LibraryFiles, store};
use crate::error::ErrorMeta;

#[derive(Debug, thiserror::Error, ErrorMeta)]
pub enum UpdateCollectionError {
    #[error("the collection does not exist")]
    #[meta(class = NotFound, reason = Self::NOT_FOUND)]
    NotFound,
    #[error(transparent)]
    #[meta(transparent)]
    Storage(#[from] LibraryStorageError),
}

impl UpdateCollectionError {
    pub const NOT_FOUND: &'static str = "library.update_collection.not_found";
}

pub async fn execute<F: LibraryFiles>(
    id: CollectionId,
    request: UpdateCollection,
    library: &Library<F>,
) -> Result<(), UpdateCollectionError> {
    let _change = library.begin_change().await;
    let mut database = library.database();
    store::find_visible_collection(&mut database, id)
        .await?
        .ok_or(UpdateCollectionError::NotFound)?;
    let UpdateCollection {
        title,
        background,
        order,
    } = request;
    if title.is_none() && background.is_none() && order.is_none() {
        return Ok(());
    }
    let mut update = Collection::update_by_id(id);
    if let Some(title) = title {
        update = update.title(title).builtin_title(None::<BuiltinTitle>);
    }
    if let Some(background) = background {
        let (preset, image) = background.into_columns();
        update = update.background_preset(preset).background_image(image);
    }
    if let Some(order) = order {
        update = update
            .order_key(order.key)
            .order_descending(order.descending);
    }
    update
        .exec(&mut database)
        .await
        .map_err(LibraryStorageError::from)?;
    library.publish().await?;
    Ok(())
}
