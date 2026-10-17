//! Saving a collection's custom order and switching the collection to it.

use std::{cmp::Ordering, collections::HashMap};

use toasty_core::driver::operation::TransactionMode;
use tufo_models::library::{
    AudioFile, Collection, CollectionEntry, CollectionEntryId, EntryOrderKey, Title, compare_titles,
};
use tufo_wire::library::ArrangeCollectionEntries;

use super::{Library, LibraryStorageError, library_files::LibraryFiles, store};
use crate::error::ErrorMeta;

#[derive(Debug, thiserror::Error, ErrorMeta)]
pub enum ArrangeCollectionEntriesError {
    #[error("the collection does not exist")]
    #[meta(class = NotFound, reason = Self::NOT_FOUND)]
    NotFound,
    #[error("the collection doesn't list the entry {0}")]
    #[meta(class = NotFound, reason = Self::ENTRY_NOT_FOUND)]
    EntryNotFound(CollectionEntryId),
    #[error(transparent)]
    #[meta(transparent)]
    Storage(#[from] LibraryStorageError),
}

impl ArrangeCollectionEntriesError {
    pub const NOT_FOUND: &'static str = "library.arrange_collection_entries.not_found";
    pub const ENTRY_NOT_FOUND: &'static str = "library.arrange_collection_entries.entry_not_found";
}

pub async fn execute<F: LibraryFiles>(
    request: ArrangeCollectionEntries,
    library: &Library<F>,
) -> Result<(), ArrangeCollectionEntriesError> {
    let _change = library.begin_change().await;
    let mut database = library.database();
    let mut transaction = database
        .transaction_builder()
        .mode(TransactionMode::Immediate)
        .begin()
        .await
        .map_err(LibraryStorageError::from)?;
    let collection = store::find_visible_collection(&mut transaction, request.collection_id)
        .await?
        .ok_or(ArrangeCollectionEntriesError::NotFound)?;
    let mut entries = CollectionEntry::filter_by_collection_id(collection.id)
        .exec(&mut transaction)
        .await
        .map_err(LibraryStorageError::from)?
        .into_iter()
        .map(|entry| (entry.id, entry))
        .collect::<HashMap<_, _>>();
    let mut arranged = Vec::with_capacity(entries.len());
    for id in request.entry_ids.as_slice() {
        let entry = entries
            .remove(id)
            .ok_or(ArrangeCollectionEntriesError::EntryNotFound(*id))?;
        arranged.push(entry);
    }
    let mut left_out = Vec::with_capacity(entries.len());
    for entry in entries.into_values() {
        let file = AudioFile::filter_by_id(entry.audio_file_id)
            .get(&mut transaction)
            .await
            .map_err(LibraryStorageError::from)?;
        left_out.push((entry, file.title));
    }
    left_out.sort_by(|(left, left_title), (right, right_title)| {
        compare_custom_places(left, left_title, right, right_title)
    });
    arranged.extend(left_out.into_iter().map(|(entry, _)| entry));
    for (position, entry) in (0..).zip(&arranged) {
        if entry.position != position {
            CollectionEntry::update_by_id(entry.id)
                .position(position)
                .exec(&mut transaction)
                .await
                .map_err(LibraryStorageError::from)?;
        }
    }
    Collection::update_by_id(collection.id)
        .order_key(EntryOrderKey::Custom)
        .order_descending(false)
        .exec(&mut transaction)
        .await
        .map_err(LibraryStorageError::from)?;
    transaction
        .commit()
        .await
        .map_err(LibraryStorageError::from)?;
    library.publish().await?;
    Ok(())
}

fn compare_custom_places(
    left: &CollectionEntry,
    left_title: &Title,
    right: &CollectionEntry,
    right_title: &Title,
) -> Ordering {
    left.position
        .cmp(&right.position)
        .then_with(|| compare_titles(left_title.as_str(), right_title.as_str()))
        .then_with(|| left.id.cmp(&right.id))
}
