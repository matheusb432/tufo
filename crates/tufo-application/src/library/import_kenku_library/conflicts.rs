//! Conflict detection and validation before an import changes any collection.

use std::collections::{HashMap, HashSet};

use serde_json::json;
use tufo_models::library::{
    AudioFile, AudioFileId, Collection, CollectionEntry, CollectionId, Title,
};
use tufo_wire::library::KenkuCollectionChoice;

use super::{
    ImportKenkuLibraryError,
    journal::{Details, StoredConflict},
    plan::PlannedCollection,
};
use crate::library::LibraryStorageError;

pub(super) fn validate_choices(
    details: &Details,
    choices: &[KenkuCollectionChoice],
) -> Result<(), ImportKenkuLibraryError> {
    if choices.len() != details.conflicts.len() {
        return Err(ImportKenkuLibraryError::Choices);
    }
    let mut seen = HashSet::new();
    let mut overwritten = HashSet::new();
    for choice in choices {
        let conflict = details
            .conflicts
            .iter()
            .find(|conflict| {
                conflict.kind == choice.kind.into() && conflict.source_id == choice.source_id
            })
            .ok_or(ImportKenkuLibraryError::Choices)?;
        if !seen.insert((choice.kind, &choice.source_id))
            || (choice.overwrite && !overwritten.insert(&conflict.target))
        {
            return Err(ImportKenkuLibraryError::Choices);
        }
    }
    Ok(())
}

pub(super) fn collect(
    incoming: &[PlannedCollection],
    existing: &[Collection],
    entries: &[CollectionEntry],
    files: &[AudioFile],
    details: &Details,
) -> Result<Vec<StoredConflict>, LibraryStorageError> {
    let mut by_title = HashMap::new();
    let mut by_folder = HashMap::new();
    let mut shown_ids = HashSet::new();
    for collection in existing {
        if let Some(folder) = &collection.folder_path {
            by_folder
                .entry((collection.kind, folder))
                .or_insert(collection);
        }
        if collection.hidden_at.is_none() {
            by_title
                .entry((collection.kind, &collection.title))
                .or_insert(collection);
            shown_ids.insert(collection.id);
        }
    }
    let mappings = details
        .collections
        .iter()
        .map(|mapping| {
            (
                (
                    tufo_models::library::CollectionKind::from(mapping.kind),
                    mapping.source_id.as_str(),
                ),
                mapping,
            )
        })
        .collect::<HashMap<_, _>>();
    let mut listed = HashMap::<CollectionId, Vec<&CollectionEntry>>::new();
    for entry in entries {
        listed.entry(entry.collection_id).or_default().push(entry);
    }
    let mut conflicts = Vec::new();
    let titles = files
        .iter()
        .map(|file| (file.id, &file.title))
        .collect::<HashMap<_, _>>();
    for planned in incoming {
        let prior = mappings.get(&(planned.kind, planned.source_id.as_str()));
        if let Some(prior) = prior {
            match prior.destination()? {
                None => continue,
                Some(id) if shown_ids.contains(&id) => continue,
                Some(_) => {}
            }
        }
        // A matching folder includes a deleted collection: restoring that exact collection
        // still requires a choice, and never restores its deleted descendants.
        let by_folder = planned
            .folder
            .as_ref()
            .and_then(|folder| by_folder.get(&(planned.kind, folder)))
            .copied();
        let by_title = by_title.get(&(planned.kind, &planned.title)).copied();
        let Some(target) = by_folder.or(by_title) else {
            continue;
        };
        conflicts.push(StoredConflict {
            source_id: planned.source_id.clone(),
            kind: planned.kind.into(),
            incoming_title: planned.title.as_str().to_owned(),
            existing_title: target.title.as_str().to_owned(),
            target: target.id.to_string(),
            expected: target_version(
                target,
                listed.get(&target.id).map_or(&[], Vec::as_slice),
                &titles,
            ),
        });
    }
    Ok(conflicts)
}

fn target_version(
    collection: &Collection,
    entries: &[&CollectionEntry],
    titles: &HashMap<AudioFileId, &Title>,
) -> String {
    let mut listed = entries.to_vec();
    listed.sort_by_key(|entry| entry.id);
    let entries = listed
        .into_iter()
        .map(|entry| {
            json!([
                entry.id.to_string(),
                entry.audio_file_id.to_string(),
                titles.get(&entry.audio_file_id).map(|title| title.as_str()),
                entry.looping,
                entry.volume.into_inner(),
                entry.fade_in.into_inner().as_millis(),
                entry.fade_out.into_inner().as_millis(),
                entry.icon.index(),
            ])
        })
        .collect::<Vec<_>>();
    json!([
        collection.title.as_str(),
        collection
            .background_preset
            .map(tufo_models::library::BackgroundPreset::index),
        collection
            .background_image
            .as_ref()
            .map(|image| image.as_ref().to_string_lossy()),
        collection.order_descending,
        match collection.order_key {
            tufo_models::library::EntryOrderKey::Title => 0,
            tufo_models::library::EntryOrderKey::Duration => 1,
            tufo_models::library::EntryOrderKey::DateAdded => 2,
        },
        collection.library_folder_id.map(|id| id.to_string()),
        collection
            .folder_path
            .as_ref()
            .map(|folder| folder.as_ref().to_string_lossy()),
        collection.hidden_at.is_some(),
        entries,
    ])
    .to_string()
}

pub(super) fn overwrite_target(
    planned: &PlannedCollection,
    details: &Details,
    choices: &[KenkuCollectionChoice],
) -> Result<Option<CollectionId>, LibraryStorageError> {
    let overwrites = choices.iter().any(|choice| {
        choice.kind == planned.kind && choice.source_id == planned.source_id && choice.overwrite
    });
    if !overwrites {
        return Ok(None);
    }
    details
        .conflicts
        .iter()
        .find(|conflict| {
            conflict.kind == planned.kind.into() && conflict.source_id == planned.source_id
        })
        .map(StoredConflict::target)
        .transpose()
}

pub(super) fn keep_existing(
    planned: &PlannedCollection,
    details: &Details,
    choices: &[KenkuCollectionChoice],
) -> bool {
    choices.iter().any(|choice| {
        choice.kind == planned.kind && choice.source_id == planned.source_id && !choice.overwrite
    }) || details.collections.iter().any(|mapping| {
        mapping.kind == planned.kind.into()
            && mapping.source_id == planned.source_id
            && mapping.destination.is_none()
    })
}
