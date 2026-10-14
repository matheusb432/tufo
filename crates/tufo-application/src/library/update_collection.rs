//! Retitling a collection, changing its background, reordering its entries, or pinning it to the
//! sidebar.

use tufo_models::library::{BuiltinTitle, Collection, CollectionId, EntryOrder};
use tufo_wire::library::{EntryOrderPatch, UpdateCollection};

use super::{Library, LibraryStorageError, library_files::LibraryFiles, store};
use crate::error::ErrorMeta;

#[derive(Debug, thiserror::Error, ErrorMeta)]
pub enum UpdateCollectionError {
    #[error("order and order_patch cannot both be supplied")]
    #[meta(class = InvalidArgument, reason = Self::CONFLICTING_ORDER_INPUTS)]
    ConflictingOrderInputs,
    #[error("the collection does not exist")]
    #[meta(class = NotFound, reason = Self::NOT_FOUND)]
    NotFound,
    #[error(transparent)]
    #[meta(transparent)]
    Storage(#[from] LibraryStorageError),
}

impl UpdateCollectionError {
    pub const CONFLICTING_ORDER_INPUTS: &'static str =
        "library.update_collection.conflicting_order_inputs";
    pub const NOT_FOUND: &'static str = "library.update_collection.not_found";
}

pub async fn execute<F: LibraryFiles>(
    id: CollectionId,
    request: UpdateCollection,
    library: &Library<F>,
) -> Result<(), UpdateCollectionError> {
    if request.order.is_some() && request.order_patch.is_some() {
        return Err(UpdateCollectionError::ConflictingOrderInputs);
    }
    let _change = library.begin_change().await;
    let mut database = library.database();
    let current = store::find_visible_collection(&mut database, id)
        .await?
        .ok_or(UpdateCollectionError::NotFound)?;
    let UpdateCollection {
        title,
        background,
        order,
        order_patch,
        pinned,
    } = request;
    let order =
        order.or_else(|| order_patch.and_then(|patch| apply_order_patch(current.order(), patch)));
    if title.is_none() && background.is_none() && order.is_none() && pinned.is_none() {
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
    if let Some(pinned) = pinned {
        update = update.pinned(pinned);
    }
    update
        .exec(&mut database)
        .await
        .map_err(LibraryStorageError::from)?;
    library.publish().await?;
    Ok(())
}

fn apply_order_patch(current: EntryOrder, patch: EntryOrderPatch) -> Option<EntryOrder> {
    if patch.key.is_none() && patch.descending.is_none() {
        return None;
    }
    Some(EntryOrder {
        key: patch.key.unwrap_or(current.key),
        descending: patch.descending.unwrap_or(current.descending),
    })
}

#[cfg(test)]
mod tests {
    use tufo_models::library::EntryOrderKey;

    use super::*;

    #[test]
    fn ordering_patches_preserve_unselected_current_properties() {
        let current = EntryOrder {
            key: EntryOrderKey::DateAdded,
            descending: true,
        };
        assert_eq!(
            apply_order_patch(
                current,
                EntryOrderPatch {
                    descending: Some(false),
                    ..EntryOrderPatch::default()
                }
            ),
            Some(EntryOrder {
                key: EntryOrderKey::DateAdded,
                descending: false
            })
        );
        assert_eq!(
            apply_order_patch(
                current,
                EntryOrderPatch {
                    key: Some(EntryOrderKey::Duration),
                    ..EntryOrderPatch::default()
                }
            ),
            Some(EntryOrder {
                key: EntryOrderKey::Duration,
                descending: true
            })
        );
        assert_eq!(apply_order_patch(current, EntryOrderPatch::default()), None);
    }
}
