//! Reconciling the whole library with the disk.

use std::{collections::HashMap, path::PathBuf};

use futures_util::{StreamExt as _, stream};
use tufo_models::{
    library::{AudioFileId, FileStamp, LibraryFolder},
    playback::AudioFilePath,
};

use super::{
    Library, LibraryStorageError,
    library_files::{AudioFacts, LibraryFiles, ScanFolderError},
    reconcile::{self, LibraryFolderScan, ReconcileChanges, ReconcileInput, ScanOutcome},
    store,
};

/// Demuxing is disk- and CPU-bound, so a few files at a time saturate either.
const INSPECTIONS_IN_FLIGHT_MAX: usize = 4;
const STATS_IN_FLIGHT_MAX: usize = 16;

/// What one reconciliation changed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncReport {
    pub changed: bool,
    pub collections_created: usize,
    pub files_added: usize,
    pub unreadable_files: Vec<AudioFilePath>,
}

impl SyncReport {
    fn of(changes: &ReconcileChanges) -> Self {
        Self {
            changed: !changes.is_empty(),
            collections_created: changes.new_collections.len(),
            files_added: changes.new_files.len(),
            unreadable_files: changes.unreadable_files.clone(),
        }
    }
}

/// Scans every library folder, reconciles the stored library with what's on disk, and
/// publishes a snapshot when anything changed.
pub async fn execute<F: LibraryFiles>(
    library: &Library<F>,
) -> Result<SyncReport, LibraryStorageError> {
    let _change = library.begin_change().await;
    let report = sync_while_changing(library).await?;
    if report.changed {
        library.publish().await?;
    }
    Ok(report)
}

/// Reconciles without taking the write gate, for operations that already hold it. The caller
/// publishes.
pub(super) async fn sync_while_changing<F: LibraryFiles>(
    library: &Library<F>,
) -> Result<SyncReport, LibraryStorageError> {
    let mut database = library.database();
    let stored = store::load_stored_library(&mut database).await?;
    let library_folders = scan_library_folders(library.files(), &stored.library_folders).await;
    let to_stat = reconcile::files_to_stat(&stored.files, &library_folders);
    let stat_results = stat_files(library.files(), to_stat).await;
    let pending = reconcile::plan(ReconcileInput {
        files: stored.files,
        library_folders,
        collections: stored.collections,
        stat_results,
    });
    let inspections = inspect_files(library.files(), pending.inspections_needed()).await;
    let changes = pending.finish(&inspections);
    let report = SyncReport::of(&changes);
    if report.changed {
        store::apply_changes(&mut database, &changes).await?;
    }
    Ok(report)
}

async fn scan_library_folders(
    files: &impl LibraryFiles,
    library_folders: &[LibraryFolder],
) -> Vec<LibraryFolderScan> {
    let mut scans = Vec::with_capacity(library_folders.len());
    for library_folder in library_folders {
        let outcome = match files.scan_folder(library_folder.folder_path.clone()).await {
            Ok(scan) => ScanOutcome::Scanned(scan),
            Err(ScanFolderError::NotFound | ScanFolderError::NotAFolder) => ScanOutcome::Gone,
            Err(ScanFolderError::Unreadable(error)) => {
                tracing::warn!(
                    folder = %library_folder.folder_path.as_ref().display(),
                    %error,
                    "cannot read a library folder"
                );
                ScanOutcome::Unreadable
            }
        };
        scans.push(LibraryFolderScan {
            id: library_folder.id,
            kind: library_folder.kind,
            path: library_folder.folder_path.clone(),
            outcome,
        });
    }
    scans
}

/// Stats files one by one. A file whose stat fails is left out, so it keeps its state.
async fn stat_files(
    files: &impl LibraryFiles,
    to_stat: Vec<(AudioFileId, AudioFilePath)>,
) -> HashMap<AudioFileId, Option<FileStamp>> {
    stream::iter(to_stat)
        .map(|(id, path)| async move { (id, files.stat_file(path).await) })
        .buffer_unordered(STATS_IN_FLIGHT_MAX)
        .filter_map(|(id, result)| async move { result.ok().map(|stamp| (id, stamp)) })
        .collect()
        .await
}

/// Reads the audio facts of each file, a few at a time. Unreadable files map to `None`.
pub(super) async fn inspect_files(
    files: &impl LibraryFiles,
    paths: Vec<AudioFilePath>,
) -> HashMap<PathBuf, Option<AudioFacts>> {
    stream::iter(paths)
        .map(|path| async move {
            let result = files.inspect_audio(path.clone()).await;
            if let Err(error) = &result {
                tracing::info!(
                    file = %path.as_ref().display(),
                    error = format!("{error:#}"),
                    "cannot read an audio file"
                );
            }
            (path.as_ref().clone(), result.ok())
        })
        .buffer_unordered(INSPECTIONS_IN_FLIGHT_MAX)
        .collect()
        .await
}
