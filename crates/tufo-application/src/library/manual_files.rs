//! Adding audio files to a manual collection, reading each file Tufo doesn't know yet.

use jiff::Timestamp;
use toasty::Db;
use toasty_core::driver::operation::TransactionMode;
use tufo_models::{
    library::{
        AudioFile, AudioFileId, Collection, CollectionEntry, CollectionKind, SoundIcon,
        SoundSettings,
    },
    path::LocalPath,
    playback::AudioFilePath,
};

use super::{
    Library, LibraryStorageError,
    library_files::{ClassifiedPath, LibraryFiles},
    reconcile::{NewFile, audio_file_title},
    store,
    sync_library::inspect_files,
};

/// Dropped paths sorted into audio files, expanding folders recursively, and everything else.
#[derive(Debug, Default)]
pub(super) struct DroppedFiles {
    pub(super) audio_files: Vec<AudioFilePath>,
    pub(super) ignored: Vec<LocalPath>,
}

pub(super) async fn collect_audio_files(
    files: &impl LibraryFiles,
    paths: Vec<LocalPath>,
) -> DroppedFiles {
    let mut dropped = DroppedFiles::default();
    for path in paths {
        match files.classify_path(path.clone()).await {
            Ok(ClassifiedPath::AudioFile(file)) => dropped.audio_files.push(file),
            Ok(ClassifiedPath::Folder(folder)) => match files.scan_folder(folder).await {
                Ok(scan) => dropped.audio_files.extend(
                    scan.folders
                        .into_iter()
                        .flat_map(|folder| folder.audio_files)
                        .map(|file| file.path),
                ),
                Err(_) => dropped.ignored.push(path),
            },
            Ok(ClassifiedPath::Other) | Err(_) => dropped.ignored.push(path),
        }
    }
    dropped
        .audio_files
        .sort_by(|left, right| left.as_ref().cmp(right.as_ref()));
    dropped.audio_files.dedup();
    dropped
}

/// What adding files to a collection did.
#[derive(Debug, Default)]
pub(super) struct AddedFiles {
    pub(super) files_added: usize,
    pub(super) unreadable: Vec<AudioFilePath>,
}

/// Lists each file in the collection, creating the files Tufo doesn't know. Files the
/// collection already lists are skipped. Files added to a playlist become song sources. The
/// caller holds the write gate.
pub(super) async fn add_files<F: LibraryFiles>(
    library: &Library<F>,
    collection: &Collection,
    audio_files: Vec<AudioFilePath>,
) -> Result<AddedFiles, LibraryStorageError> {
    let mut database = library.database();
    let (known, unknown) = partition_known(&mut database, audio_files).await?;
    let ReadFiles {
        readable: new_files,
        unreadable,
    } = read_files(library.files(), unknown).await;
    let mut added = AddedFiles {
        files_added: 0,
        unreadable,
    };

    let now = Timestamp::now();
    let mut transaction = database
        .transaction_builder()
        .mode(TransactionMode::Immediate)
        .begin()
        .await?;
    let listed = CollectionEntry::filter_by_collection_id(collection.id)
        .exec(&mut transaction)
        .await?
        .into_iter()
        .map(|entry| entry.audio_file_id)
        .collect::<Vec<_>>();
    let mut file_ids = known;
    for file in &new_files {
        file_ids.push(
            store::create_audio_file(&mut transaction, file, now, None)
                .await?
                .id,
        );
    }
    for audio_file_id in file_ids {
        if collection.kind == CollectionKind::Playlist {
            AudioFile::update_by_id(audio_file_id)
                .song_source(true)
                .exec(&mut transaction)
                .await?;
        }
        if !listed.contains(&audio_file_id) {
            store::create_entry(
                &mut transaction,
                collection.id,
                audio_file_id,
                now,
                SoundSettings::default(),
                SoundIcon::default(),
            )
            .await?;
            added.files_added += 1;
        }
    }
    transaction.commit().await?;
    Ok(added)
}

pub(super) struct ReadFiles {
    pub(super) readable: Vec<NewFile>,
    pub(super) unreadable: Vec<AudioFilePath>,
}

pub(super) async fn read_files(files: &impl LibraryFiles, paths: Vec<AudioFilePath>) -> ReadFiles {
    let inspections = inspect_files(files, paths.clone()).await;
    let mut read = ReadFiles {
        readable: Vec::new(),
        unreadable: Vec::new(),
    };
    for path in paths {
        let facts = inspections.get(path.as_ref()).cloned().flatten();
        let stamp = files.stat_file(path.clone()).await.ok().flatten();
        match (facts, stamp) {
            (Some(facts), Some(stamp)) => read.readable.push(NewFile {
                title: audio_file_title(&path),
                path,
                stamp,
                facts,
            }),
            _ => read.unreadable.push(path),
        }
    }
    read
}

/// Splits paths into the ids of files Tufo knows at that path and the paths it doesn't.
async fn partition_known(
    database: &mut Db,
    audio_files: Vec<AudioFilePath>,
) -> Result<(Vec<AudioFileId>, Vec<AudioFilePath>), LibraryStorageError> {
    let mut known = Vec::new();
    let mut unknown = Vec::new();
    for path in audio_files {
        let stored = AudioFile::filter_by_file_path(path.clone())
            .first()
            .exec(database)
            .await?;
        match stored {
            Some(file) => known.push(file.id),
            None => unknown.push(path),
        }
    }
    Ok((known, unknown))
}
