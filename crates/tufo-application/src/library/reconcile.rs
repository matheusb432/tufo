//! Reconciliation: the changes that make the stored library match what the filesystem shows now.
//!
//! This module is pure. The sync operation gathers a [`ReconcileInput`], calls [`plan`], inspects
//! the files the plan asks for, and applies what [`PendingReconcile::finish`] returns in one
//! transaction.
//!
//! A stored file is matched to what's on disk in this order:
//!
//! 1. the same file identity at its stored path, which is the file itself;
//! 2. its file identity at another scanned path, which is a rename or move within a volume;
//! 3. another file at its stored path, which is a rewrite in place, as taggers often do;
//! 4. its audio fingerprint at another scanned path, which is a move across volumes or a copy
//!    followed by a delete.
//!
//! A file that matches nothing is missing. Only an authoritative observation marks a file or
//! folder missing: a complete scan of a folder that covers it, or a stat that found nothing.
//! Files and folders under an unreadable or truncated scan keep their stored state.

mod collections;
mod files;
mod titles;

use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
};

pub use titles::{audio_file_title, linked_collection_title};
use tufo_models::{
    library::{
        AudioFileId, AudioFingerprint, CollectionEntryId, CollectionId, CollectionKind,
        FileIdentity, FileStamp, LibraryFolderId, Title,
    },
    path::FolderPath,
    playback::AudioFilePath,
};

use super::library_files::{AudioFacts, FolderScan};

/// A stored audio file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownFile {
    pub id: AudioFileId,
    pub path: AudioFilePath,
    pub stamp: FileStamp,
    pub fingerprint: AudioFingerprint,
    pub missing: bool,
}

/// A stored library folder with what scanning it produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryFolderScan {
    pub id: LibraryFolderId,
    pub kind: CollectionKind,
    pub path: FolderPath,
    pub outcome: ScanOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanOutcome {
    Scanned(FolderScan),
    /// Nothing is at the folder's path any more.
    Gone,
    /// The folder exists but couldn't be read, so nothing under it is judged.
    Unreadable,
}

/// A stored collection linked to a folder under a library folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownLinkedCollection {
    pub id: CollectionId,
    pub library_folder_id: LibraryFolderId,
    pub folder_path: FolderPath,
    pub folder_identity: FileIdentity,
    pub title: Title,
    pub hidden: bool,
    pub missing: bool,
    pub entries: Vec<KnownEntry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KnownEntry {
    pub id: CollectionEntryId,
    pub audio_file_id: AudioFileId,
}

/// The stored library and what the filesystem shows now.
#[derive(Debug, Clone, Default)]
pub struct ReconcileInput {
    pub files: Vec<KnownFile>,
    pub library_folders: Vec<LibraryFolderScan>,
    pub collections: Vec<KnownLinkedCollection>,
    /// Stamps read one by one for the files [`files_to_stat`] named: `None` when nothing is at
    /// the file's path. A file whose stat failed is absent from the map, and keeps its state.
    pub stat_results: HashMap<AudioFileId, Option<FileStamp>>,
}

/// The stored files no complete scan covers, which the sync operation stats one by one.
#[must_use]
pub fn files_to_stat(
    files: &[KnownFile],
    library_folders: &[LibraryFolderScan],
) -> Vec<(AudioFileId, AudioFilePath)> {
    let roots = authoritative_roots(library_folders);
    files
        .iter()
        .filter(|file| !is_under_any(file.path.as_ref(), &roots))
        .map(|file| (file.id, file.path.clone()))
        .collect()
}

/// A reconciliation waiting for the audio facts of [`PendingReconcile::inspections_needed`].
#[derive(Debug)]
pub struct PendingReconcile {
    input: ReconcileInput,
    matched: files::MatchedFiles,
}

/// Matches every stored file against the scans and stats.
#[must_use]
pub fn plan(input: ReconcileInput) -> PendingReconcile {
    let matched = files::match_files(&input);
    PendingReconcile { input, matched }
}

impl PendingReconcile {
    /// The files whose audio must be read before [`Self::finish`]: changed files and every
    /// scanned file no stored file claimed.
    #[must_use]
    pub fn inspections_needed(&self) -> Vec<AudioFilePath> {
        self.matched.inspections_needed()
    }

    /// Completes the reconciliation with the audio facts read for
    /// [`Self::inspections_needed`]. A path absent from `inspections` or mapped to `None`
    /// couldn't be read.
    #[must_use]
    pub fn finish(self, inspections: &HashMap<PathBuf, Option<AudioFacts>>) -> ReconcileChanges {
        let settled = self.matched.settle(&self.input.files, inspections);
        let mut changes = settled.changes;
        collections::reconcile_collections(&self.input, &settled.present, &mut changes);
        changes
    }
}

/// What reconciliation changes, ready to apply in one transaction.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReconcileChanges {
    pub file_changes: Vec<FileChange>,
    /// Files seen for the first time. [`FileRef::New`] indexes this list.
    pub new_files: Vec<NewFile>,
    pub collection_changes: Vec<CollectionChange>,
    /// Folders that hold audio but had no collection. [`CollectionRef::New`] indexes this list.
    pub new_collections: Vec<NewLinkedCollection>,
    pub entries_added: Vec<(CollectionRef, FileRef)>,
    pub entries_removed: Vec<CollectionEntryId>,
    /// Scanned files whose audio couldn't be read, left out of the library.
    pub unreadable_files: Vec<AudioFilePath>,
}

impl ReconcileChanges {
    /// Whether applying the changes would leave the store as it is.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.file_changes.is_empty()
            && self.new_files.is_empty()
            && self.collection_changes.is_empty()
            && self.new_collections.is_empty()
            && self.entries_added.is_empty()
            && self.entries_removed.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum FileChange {
    /// The file is back where it was stored, unchanged.
    Found {
        id: AudioFileId,
    },
    /// The file is at `path` now, with `stamp`, and no longer missing. `facts` is present
    /// when its audio was read again.
    Updated {
        id: AudioFileId,
        path: AudioFilePath,
        stamp: FileStamp,
        facts: Option<AudioFacts>,
    },
    Missing {
        id: AudioFileId,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct NewFile {
    pub path: AudioFilePath,
    pub stamp: FileStamp,
    pub facts: AudioFacts,
    pub title: Title,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CollectionChange {
    Found {
        id: CollectionId,
    },
    /// The linked folder is at `path` now, or was recreated there with a new identity.
    Relinked {
        id: CollectionId,
        path: FolderPath,
        identity: FileIdentity,
        /// The new title, when the old one was derived from the old path.
        title: Option<Title>,
    },
    Missing {
        id: CollectionId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewLinkedCollection {
    pub library_folder_id: LibraryFolderId,
    pub kind: CollectionKind,
    pub path: FolderPath,
    pub identity: FileIdentity,
    pub title: Title,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CollectionRef {
    Existing(CollectionId),
    New(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FileRef {
    Existing(AudioFileId),
    New(usize),
}

/// The roots of complete scans, which settle whether anything under them exists.
fn authoritative_roots(library_folders: &[LibraryFolderScan]) -> Vec<&Path> {
    library_folders
        .iter()
        .filter(|folder| matches!(&folder.outcome, ScanOutcome::Scanned(scan) if !scan.truncated))
        .map(|folder| folder.path.as_ref().as_path())
        .collect()
}

fn is_under_any(path: &Path, roots: &[&Path]) -> bool {
    roots.iter().any(|root| path.starts_with(root))
}

/// Maps each key to its value when exactly one value has that key.
fn unique_by<K: Ord, V>(pairs: impl Iterator<Item = (K, V)>) -> BTreeMap<K, V> {
    let mut seen = BTreeMap::<K, Option<V>>::new();
    for (key, value) in pairs {
        seen.entry(key)
            .and_modify(|slot| *slot = None)
            .or_insert(Some(value));
    }
    seen.into_iter()
        .filter_map(|(key, value)| Some((key, value?)))
        .collect()
}

#[cfg(test)]
mod tests;
