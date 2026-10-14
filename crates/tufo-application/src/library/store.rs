//! Shared library queries and writes for imports, edits, and reconciliation.

use std::collections::{HashMap, HashSet};

use anyhow::Context as _;
use jiff::Timestamp;
use toasty::{Db, db::Executor};
use toasty_core::driver::operation::TransactionMode;
use tufo_models::{
    library::{
        AudioFile, AudioFileId, AudioFingerprint, BackgroundPresetChooser, BuiltinTitle,
        Collection, CollectionEntry, CollectionId, CollectionKind, EntryOrderKey, LibraryFolder,
        SoundIcon, SoundSettings, Title, milliseconds_saturating,
        song_loop::{LoopRegion, SongLoop},
    },
    playback::AudioFilePath,
};
use tufo_wire::library::Background;

use super::{
    LibraryStorageError,
    library_files::AudioFacts,
    reconcile::{
        CollectionChange, CollectionRef, FileChange, FileRef, KnownEntry, KnownFile,
        KnownLinkedCollection, NewFile, ReconcileChanges,
    },
};

/// The stored side of a reconciliation.
pub(super) struct StoredLibrary {
    pub(super) files: Vec<KnownFile>,
    pub(super) library_folders: Vec<LibraryFolder>,
    pub(super) collections: Vec<KnownLinkedCollection>,
}

pub(super) async fn load_stored_library(
    database: &mut Db,
) -> Result<StoredLibrary, LibraryStorageError> {
    let files = AudioFile::all()
        .exec(database)
        .await?
        .into_iter()
        .map(|file| KnownFile {
            id: file.id,
            stamp: file.stamp(),
            missing: file.missing_since.is_some(),
            fingerprint: file.fingerprint,
            path: file.file_path,
        })
        .collect();
    let library_folders = LibraryFolder::all().exec(database).await?;
    let mut entries_by_collection = HashMap::<CollectionId, Vec<KnownEntry>>::new();
    for entry in CollectionEntry::all().exec(database).await? {
        entries_by_collection
            .entry(entry.collection_id)
            .or_default()
            .push(KnownEntry {
                id: entry.id,
                audio_file_id: entry.audio_file_id,
            });
    }
    let collections = Collection::all()
        .exec(database)
        .await?
        .into_iter()
        .filter_map(|collection| {
            Some(KnownLinkedCollection {
                library_folder_id: collection.library_folder_id?,
                folder_path: collection.folder_path?,
                folder_identity: collection.folder_identity?,
                entries: entries_by_collection
                    .remove(&collection.id)
                    .unwrap_or_default(),
                id: collection.id,
                title: collection.title,
                hidden: collection.hidden_at.is_some(),
                missing: collection.folder_missing_since.is_some(),
            })
        })
        .collect();
    Ok(StoredLibrary {
        files,
        library_folders,
        collections,
    })
}

/// A chooser for new collections' presets that counts the presets the collections show now.
pub(super) async fn background_preset_chooser(
    database: &mut Db,
) -> Result<BackgroundPresetChooser, LibraryStorageError> {
    let shown = Collection::all()
        .exec(database)
        .await?
        .into_iter()
        .filter(|collection| collection.hidden_at.is_none())
        .filter_map(|collection| collection.background_preset);
    Ok(BackgroundPresetChooser::new(shown, fastrand::Rng::new()))
}

/// Applies a reconciliation in one transaction, then drops the files nothing keeps.
pub(super) async fn apply_changes(
    database: &mut Db,
    changes: &ReconcileChanges,
) -> Result<(), LibraryStorageError> {
    let now = Timestamp::now();
    let mut presets = background_preset_chooser(database).await?;
    let mut transaction = database
        .transaction_builder()
        .mode(TransactionMode::Immediate)
        .begin()
        .await?;
    apply_file_changes(&mut transaction, &changes.file_changes, now).await?;
    let mut new_file_ids = Vec::with_capacity(changes.new_files.len());
    for file in &changes.new_files {
        new_file_ids.push(
            create_audio_file(&mut transaction, file, now, None)
                .await?
                .id,
        );
    }
    apply_collection_changes(&mut transaction, &changes.collection_changes, now).await?;
    let mut new_collection_ids = Vec::with_capacity(changes.new_collections.len());
    for collection in &changes.new_collections {
        let created = toasty::create!(Collection {
            kind: collection.kind,
            title: collection.title.clone(),
            background_preset: Some(presets.choose()),
            background_image: None,
            order_key: EntryOrderKey::default(),
            order_descending: false,
            library_folder_id: Some(collection.library_folder_id),
            folder_path: Some(collection.path.clone()),
            folder_identity: Some(collection.identity.clone()),
            folder_missing_since: None,
            hidden_at: None,
            created_at: now,
            pinned: true,
        })
        .exec(&mut transaction)
        .await?;
        new_collection_ids.push(created.id);
    }
    for entry_id in &changes.entries_removed {
        CollectionEntry::filter_by_id(*entry_id)
            .delete()
            .exec(&mut transaction)
            .await?;
    }
    for (collection, file) in &changes.entries_added {
        let collection_id = match collection {
            CollectionRef::Existing(id) => *id,
            CollectionRef::New(index) => new_collection_ids[*index],
        };
        let audio_file_id = match file {
            FileRef::Existing(id) => *id,
            FileRef::New(index) => new_file_ids[*index],
        };
        create_entry(
            &mut transaction,
            collection_id,
            audio_file_id,
            now,
            SoundSettings::default(),
            SoundIcon::default(),
        )
        .await?;
    }
    delete_unkept_files(&mut transaction).await?;
    transaction.commit().await?;
    Ok(())
}

async fn apply_file_changes(
    executor: &mut dyn Executor,
    changes: &[FileChange],
    now: Timestamp,
) -> Result<(), LibraryStorageError> {
    // Two files can trade paths, and paths are unique, so moved files first step aside to a
    // path no file can have.
    for change in changes {
        if let FileChange::Updated { id, path, .. } = change {
            let parked = AudioFilePath::try_new(
                path.as_ref()
                    .with_extension(format!("tufo-moving-{}", id.into_uuid().simple())),
            )
            .context("parking a moved file's path")?;
            AudioFile::update_by_id(*id)
                .file_path(parked)
                .exec(executor)
                .await?;
        }
    }
    for change in changes {
        match change {
            FileChange::Found { id } => {
                AudioFile::update_by_id(*id)
                    .missing_since(None::<Timestamp>)
                    .exec(executor)
                    .await?;
            }
            FileChange::Missing { id } => {
                AudioFile::update_by_id(*id)
                    .missing_since(Some(now))
                    .exec(executor)
                    .await?;
            }
            FileChange::Updated {
                id,
                path,
                stamp,
                facts,
            } => {
                let mut update = AudioFile::update_by_id(*id)
                    .file_path(path.clone())
                    .file_identity(stamp.identity.clone())
                    .size_bytes(i64::try_from(stamp.size_bytes).unwrap_or(i64::MAX))
                    .modified_ns(stamp.modified_ns)
                    .missing_since(None::<Timestamp>);
                if let Some(AudioFacts {
                    fingerprint,
                    duration,
                }) = facts
                {
                    update = update
                        .fingerprint(fingerprint.clone())
                        .duration_ms(milliseconds_saturating(*duration));
                }
                update.exec(executor).await?;
            }
        }
    }
    Ok(())
}

async fn apply_collection_changes(
    executor: &mut dyn Executor,
    changes: &[CollectionChange],
    now: Timestamp,
) -> Result<(), LibraryStorageError> {
    for change in changes {
        match change {
            CollectionChange::Found { id } => {
                Collection::update_by_id(*id)
                    .folder_missing_since(None::<Timestamp>)
                    .exec(executor)
                    .await?;
            }
            CollectionChange::Missing { id } => {
                Collection::update_by_id(*id)
                    .folder_missing_since(Some(now))
                    .exec(executor)
                    .await?;
            }
            CollectionChange::Relinked {
                id,
                path,
                identity,
                title,
            } => {
                let mut update = Collection::update_by_id(*id)
                    .folder_path(Some(path.clone()))
                    .folder_identity(Some(identity.clone()))
                    .folder_missing_since(None::<Timestamp>);
                if let Some(title) = title {
                    update = update.title(title.clone());
                }
                update.exec(executor).await?;
            }
        }
    }
    Ok(())
}

pub(super) async fn create_audio_file(
    executor: &mut dyn Executor,
    file: &NewFile,
    now: Timestamp,
    builtin_title: Option<BuiltinTitle>,
) -> Result<AudioFile, LibraryStorageError> {
    Ok(toasty::create!(AudioFile {
        file_path: file.path.clone(),
        file_identity: file.stamp.identity.clone(),
        size_bytes: i64::try_from(file.stamp.size_bytes).unwrap_or(i64::MAX),
        modified_ns: file.stamp.modified_ns,
        fingerprint: file.facts.fingerprint.clone(),
        duration_ms: milliseconds_saturating(file.facts.duration),
        title: file.title.clone(),
        builtin_title,
        missing_since: None,
        created_at: now,
        song_source: false,
    })
    .exec(executor)
    .await?)
}

/// `replaces_saved` updates the song's loop row instead of creating one.
pub(super) async fn save_song_loop(
    database: &mut Db,
    audio_file_id: AudioFileId,
    fingerprint: &AudioFingerprint,
    region: LoopRegion,
    replaces_saved: bool,
) -> Result<(), LibraryStorageError> {
    let timing = region.timing();
    let markers = region.markers();
    if replaces_saved {
        SongLoop::update_by_audio_file_id(audio_file_id)
            .fingerprint(fingerprint.clone())
            .sample_rate(timing.sample_rate.get())
            .frame_count(timing.frame_count.0)
            .playback_start_frame(markers.playback_start.0)
            .loop_start_frame(markers.loop_start.0)
            .loop_end_frame(markers.loop_end.0)
            .crossfade_frames(markers.crossfade.0)
            .updated_at(Timestamp::now())
            .exec(database)
            .await?;
    } else {
        toasty::create!(SongLoop {
            audio_file_id,
            fingerprint: fingerprint.clone(),
            sample_rate: timing.sample_rate.get(),
            frame_count: timing.frame_count.0,
            playback_start_frame: markers.playback_start.0,
            loop_start_frame: markers.loop_start.0,
            loop_end_frame: markers.loop_end.0,
            crossfade_frames: markers.crossfade.0,
            updated_at: Timestamp::now(),
        })
        .exec(database)
        .await?;
    }
    Ok(())
}

pub(super) async fn find_visible_collection(
    executor: &mut dyn Executor,
    id: CollectionId,
) -> Result<Option<Collection>, LibraryStorageError> {
    Ok(Collection::filter_by_id(id)
        .first()
        .exec(executor)
        .await?
        .filter(|collection| collection.hidden_at.is_none()))
}

pub(super) async fn create_manual_collection(
    executor: &mut dyn Executor,
    kind: CollectionKind,
    title: Title,
    background: Background,
    builtin_title: Option<BuiltinTitle>,
) -> Result<Collection, LibraryStorageError> {
    let (background_preset, background_image) = background.into_columns();
    Ok(toasty::create!(Collection {
        kind,
        title,
        builtin_title,
        background_preset,
        background_image,
        order_key: EntryOrderKey::default(),
        order_descending: false,
        library_folder_id: None,
        folder_path: None,
        folder_identity: None,
        folder_missing_since: None,
        hidden_at: None,
        created_at: Timestamp::now(),
        pinned: true,
    })
    .exec(executor)
    .await?)
}

pub(super) async fn create_entry(
    executor: &mut dyn Executor,
    collection_id: CollectionId,
    audio_file_id: AudioFileId,
    now: Timestamp,
    sound: SoundSettings,
    icon: SoundIcon,
) -> Result<CollectionEntry, LibraryStorageError> {
    let SoundSettings {
        looping,
        volume,
        fade_in,
        fade_out,
    } = sound;
    Ok(toasty::create!(CollectionEntry {
        collection_id,
        audio_file_id,
        added_at: now,
        looping,
        volume,
        fade_in,
        fade_out,
        icon,
    })
    .exec(executor)
    .await?)
}

/// Deletes the files nothing keeps in the library. A file stays while a collection lists it,
/// while it's a song source, or while a library folder holds it and it isn't missing.
pub(super) async fn delete_unkept_files(
    executor: &mut dyn Executor,
) -> Result<(), LibraryStorageError> {
    let listed = CollectionEntry::all()
        .exec(executor)
        .await?
        .into_iter()
        .map(|entry| entry.audio_file_id)
        .collect::<HashSet<_>>();
    let library_folders = LibraryFolder::all().exec(executor).await?;
    let unkept = AudioFile::all()
        .exec(executor)
        .await?
        .into_iter()
        .filter(|file| {
            let held_by_folder = file.missing_since.is_none()
                && holding_folder(&file.file_path, &library_folders).is_some();
            let kept = listed.contains(&file.id) || file.song_source || held_by_folder;
            !kept
        })
        .map(|file| file.id)
        .collect::<Vec<_>>();
    for id in unkept {
        AudioFile::filter_by_id(id).delete().exec(executor).await?;
    }
    Ok(())
}

/// The library folder that holds `path`, the innermost when folders nest.
pub(super) fn holding_folder<'a>(
    path: &AudioFilePath,
    library_folders: impl IntoIterator<Item = &'a LibraryFolder>,
) -> Option<&'a LibraryFolder> {
    library_folders
        .into_iter()
        .filter(|folder| path.as_ref().starts_with(folder.folder_path.as_ref()))
        .max_by_key(|folder| folder.folder_path.as_ref().components().count())
}
