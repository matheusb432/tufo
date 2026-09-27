//! Building the library snapshot: visible collections with their entries in order.

use std::collections::HashMap;

use toasty::Db;
use tufo_models::library::{
    AudioFile, Collection, CollectionEntry, CollectionKind, EntryOrder, EntryOrderKey,
    compare_titles,
};
use tufo_wire::library::{Background, CollectionSource, CollectionView, EntryView};

use super::LibraryStorageError;

pub(super) async fn load_collections(
    database: &mut Db,
) -> Result<Vec<CollectionView>, LibraryStorageError> {
    let collections = Collection::all().exec(database).await?;
    let entries = CollectionEntry::all().exec(database).await?;
    let audio_files = AudioFile::all()
        .exec(database)
        .await?
        .into_iter()
        .map(|file| (file.id, file))
        .collect::<HashMap<_, _>>();
    let mut entries_by_collection = HashMap::<_, Vec<_>>::new();
    for entry in entries {
        let Some(file) = audio_files.get(&entry.audio_file_id) else {
            continue;
        };
        entries_by_collection
            .entry(entry.collection_id)
            .or_default()
            .push(entry_view(&entry, file));
    }
    let mut views = collections
        .into_iter()
        .filter(|collection| collection.hidden_at.is_none())
        .map(|collection| {
            let mut entries = entries_by_collection
                .remove(&collection.id)
                .unwrap_or_default();
            sort_entries(&mut entries, collection.order());
            collection_view(collection, entries)
        })
        .collect::<Vec<_>>();
    views.sort_by(|left, right| {
        kind_rank(left.kind)
            .cmp(&kind_rank(right.kind))
            .then_with(|| compare_titles(left.title.as_str(), right.title.as_str()))
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(views)
}

fn kind_rank(kind: CollectionKind) -> u8 {
    match kind {
        CollectionKind::Playlist => 0,
        CollectionKind::Soundboard => 1,
    }
}

fn entry_view(entry: &CollectionEntry, file: &AudioFile) -> EntryView {
    EntryView {
        id: entry.id,
        audio_file_id: file.id,
        title: file.title.clone(),
        builtin_title: file.builtin_title,
        path: file.file_path.clone(),
        duration: file.duration(),
        added_at: file.created_at,
        missing: file.missing_since.is_some(),
        sound: entry.sound_settings(),
        icon: entry.icon,
    }
}

fn collection_view(collection: Collection, entries: Vec<EntryView>) -> CollectionView {
    let order = collection.order();
    let source = match (collection.folder_path, collection.library_folder_id) {
        (Some(folder), Some(_)) => CollectionSource::Linked {
            folder,
            missing: collection.folder_missing_since.is_some(),
        },
        _ => CollectionSource::Manual,
    };
    CollectionView {
        id: collection.id,
        kind: collection.kind,
        title: collection.title,
        builtin_title: collection.builtin_title,
        background: Background::from_columns(
            collection.background_preset,
            collection.background_image,
        ),
        order,
        source,
        entries,
    }
}

/// Orders entries by the collection's key and direction. Ties fall back to the title, then the
/// entry id, in ascending order, so equal keys never shuffle between snapshots.
pub(super) fn sort_entries(entries: &mut [EntryView], order: EntryOrder) {
    entries.sort_by(|left, right| {
        let by_key = match order.key {
            EntryOrderKey::Title => compare_titles(left.title.as_str(), right.title.as_str()),
            EntryOrderKey::Duration => left.duration.cmp(&right.duration),
            EntryOrderKey::DateAdded => left.added_at.cmp(&right.added_at),
        };
        let by_key = if order.descending {
            by_key.reverse()
        } else {
            by_key
        };
        by_key
            .then_with(|| compare_titles(left.title.as_str(), right.title.as_str()))
            .then_with(|| left.id.cmp(&right.id))
    });
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, time::Duration};

    use jiff::Timestamp;
    use toasty::stmt::Uuid;
    use tufo_models::{
        library::{AudioFileId, CollectionEntryId, SoundIcon, SoundSettings, Title},
        playback::AudioFilePath,
    };

    use super::*;

    fn entry(seed: u128, title: &str, seconds: u64, added_second: i64) -> EntryView {
        EntryView {
            builtin_title: None,
            id: CollectionEntryId::from_uuid(Uuid::from_u128(seed)),
            audio_file_id: AudioFileId::from_uuid(Uuid::from_u128(seed)),
            title: Title::try_new(title.to_owned()).unwrap(),
            path: AudioFilePath::try_new(std::env::temp_dir().join(PathBuf::from(title))).unwrap(),
            duration: Duration::from_secs(seconds),
            added_at: Timestamp::from_second(added_second).unwrap(),
            missing: false,
            sound: SoundSettings::default(),
            icon: SoundIcon::default(),
        }
    }

    fn titles(entries: &[EntryView]) -> Vec<&str> {
        entries.iter().map(|entry| entry.title.as_str()).collect()
    }

    #[test]
    fn titles_sort_case_insensitively_with_numbers_by_value() {
        let mut entries = vec![
            entry(1, "Track 10", 1, 0),
            entry(2, "track 2", 1, 0),
            entry(3, "Ambush", 1, 0),
            entry(4, "Track 02b", 1, 0),
        ];

        sort_entries(&mut entries, EntryOrder::default());

        assert_eq!(
            titles(&entries),
            ["Ambush", "track 2", "Track 02b", "Track 10"]
        );
    }

    #[test]
    fn each_key_sorts_both_ways_and_ties_fall_back_to_the_title() {
        let mut entries = vec![
            entry(1, "Tavern", 180, 30),
            entry(2, "Dragon", 60, 10),
            entry(3, "Ambush", 180, 20),
        ];

        sort_entries(
            &mut entries,
            EntryOrder {
                key: EntryOrderKey::Duration,
                descending: true,
            },
        );
        assert_eq!(titles(&entries), ["Ambush", "Tavern", "Dragon"]);

        sort_entries(
            &mut entries,
            EntryOrder {
                key: EntryOrderKey::DateAdded,
                descending: false,
            },
        );
        assert_eq!(titles(&entries), ["Dragon", "Ambush", "Tavern"]);

        sort_entries(
            &mut entries,
            EntryOrder {
                key: EntryOrderKey::Title,
                descending: true,
            },
        );
        assert_eq!(titles(&entries), ["Tavern", "Dragon", "Ambush"]);
    }
}
