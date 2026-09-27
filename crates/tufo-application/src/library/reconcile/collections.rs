//! Keeping linked collections on their folders and their entries equal to the folders' files.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use tufo_models::library::{FileIdentity, LibraryFolderId};

use super::{
    CollectionChange, CollectionRef, FileRef, KnownEntry, KnownLinkedCollection, LibraryFolderScan,
    NewLinkedCollection, ReconcileChanges, ReconcileInput, ScanOutcome,
    titles::linked_collection_title, unique_by,
};
use crate::library::library_files::{FolderScan, ScannedFolder};

type FilesByFolder<'a> = BTreeMap<&'a Path, Vec<FileRef>>;

pub(super) fn reconcile_collections(
    input: &ReconcileInput,
    present: &BTreeMap<PathBuf, FileRef>,
    changes: &mut ReconcileChanges,
) {
    let mut files_by_folder = FilesByFolder::new();
    for (path, file) in present {
        if let Some(folder) = path.parent() {
            files_by_folder.entry(folder).or_default().push(*file);
        }
    }
    for library_folder in &input.library_folders {
        match &library_folder.outcome {
            ScanOutcome::Scanned(scan) => {
                let scanned = ScannedLibraryFolder {
                    library_folder,
                    scan,
                    files_by_folder: &files_by_folder,
                };
                scanned.reconcile(input, changes);
            }
            ScanOutcome::Gone => mark_collections_missing(input, library_folder.id, changes),
            ScanOutcome::Unreadable => {}
        }
    }
}

fn collections_of(
    input: &ReconcileInput,
    library_folder_id: LibraryFolderId,
) -> impl Iterator<Item = &KnownLinkedCollection> {
    input
        .collections
        .iter()
        .filter(move |collection| collection.library_folder_id == library_folder_id)
}

fn mark_collections_missing(
    input: &ReconcileInput,
    library_folder_id: LibraryFolderId,
    changes: &mut ReconcileChanges,
) {
    let newly_missing = collections_of(input, library_folder_id)
        .filter(|collection| !collection.missing)
        .map(|collection| CollectionChange::Missing { id: collection.id });
    changes.collection_changes.extend(newly_missing);
}

struct ScannedLibraryFolder<'a> {
    library_folder: &'a LibraryFolderScan,
    scan: &'a FolderScan,
    files_by_folder: &'a FilesByFolder<'a>,
}

/// A visible collection with its folder once the changes apply.
struct LinkedFolder<'a> {
    collection: CollectionRef,
    path: &'a Path,
    entries: &'a [KnownEntry],
}

impl<'a> ScannedLibraryFolder<'a> {
    /// Only a complete scan can say that a folder or file is gone.
    fn authoritative(&self) -> bool {
        !self.scan.truncated
    }

    fn reconcile(&self, input: &'a ReconcileInput, changes: &mut ReconcileChanges) {
        let (mut linked, claimed) = self.follow_collections(input, changes);
        linked.extend(self.create_collections(&claimed, changes));
        for folder in &linked {
            self.update_entries(folder, changes);
        }
    }

    /// Follows each stored collection to its folder: the folder itself, then the folder it was
    /// renamed or moved to, then a folder recreated at its path.
    fn follow_collections(
        &self,
        input: &'a ReconcileInput,
        changes: &mut ReconcileChanges,
    ) -> (Vec<LinkedFolder<'a>>, BTreeSet<&'a Path>) {
        let by_path = self
            .scan
            .folders
            .iter()
            .map(|folder| (folder.path.as_ref().as_path(), folder))
            .collect::<BTreeMap<_, _>>();
        let by_identity = unique_by(
            self.scan
                .folders
                .iter()
                .map(|folder| (folder.identity.clone(), folder)),
        );
        let mut claimed = BTreeSet::new();
        let mut linked = Vec::new();
        for collection in collections_of(input, self.library_folder.id) {
            let followed = follow(collection, &by_path, &by_identity)
                .filter(|folder| !claimed.contains(folder.path.as_ref().as_path()));
            let Some(folder) = followed else {
                let newly_missing = self.authoritative() && !collection.missing;
                changes.collection_changes.extend(
                    newly_missing.then_some(CollectionChange::Missing { id: collection.id }),
                );
                continue;
            };
            claimed.insert(folder.path.as_ref().as_path());
            changes
                .collection_changes
                .extend(self.relink_change(collection, folder));
            if !collection.hidden {
                linked.push(LinkedFolder {
                    collection: CollectionRef::Existing(collection.id),
                    path: folder.path.as_ref(),
                    entries: &collection.entries,
                });
            }
        }
        (linked, claimed)
    }

    fn relink_change(
        &self,
        collection: &KnownLinkedCollection,
        folder: &ScannedFolder,
    ) -> Option<CollectionChange> {
        if folder.path == collection.folder_path && folder.identity == collection.folder_identity {
            return collection
                .missing
                .then_some(CollectionChange::Found { id: collection.id });
        }
        let library_path = &self.library_folder.path;
        let derived = linked_collection_title(library_path, &collection.folder_path);
        Some(CollectionChange::Relinked {
            id: collection.id,
            path: folder.path.clone(),
            identity: folder.identity.clone(),
            title: (collection.title == derived)
                .then(|| linked_collection_title(library_path, &folder.path)),
        })
    }

    /// Every unclaimed folder that directly holds a present file becomes a collection.
    fn create_collections(
        &self,
        claimed: &BTreeSet<&Path>,
        changes: &mut ReconcileChanges,
    ) -> Vec<LinkedFolder<'a>> {
        let mut created = Vec::new();
        for folder in &self.scan.folders {
            let path = folder.path.as_ref().as_path();
            if claimed.contains(path) || !self.files_by_folder.contains_key(path) {
                continue;
            }
            created.push(LinkedFolder {
                collection: CollectionRef::New(changes.new_collections.len()),
                path,
                entries: &[],
            });
            changes.new_collections.push(NewLinkedCollection {
                library_folder_id: self.library_folder.id,
                kind: self.library_folder.kind,
                path: folder.path.clone(),
                identity: folder.identity.clone(),
                title: linked_collection_title(&self.library_folder.path, &folder.path),
            });
        }
        created
    }

    /// Makes the collection's entries the present files its folder holds directly. An
    /// incomplete scan only adds.
    fn update_entries(&self, folder: &LinkedFolder<'_>, changes: &mut ReconcileChanges) {
        let desired = self
            .files_by_folder
            .get(folder.path)
            .map_or(&[][..], Vec::as_slice);
        if self.authoritative() {
            let stale = folder
                .entries
                .iter()
                .filter(|entry| !desired.contains(&FileRef::Existing(entry.audio_file_id)))
                .map(|entry| entry.id);
            changes.entries_removed.extend(stale);
        }
        let unlisted = desired
            .iter()
            .filter(|file| !is_listed(folder.entries, **file))
            .map(|file| (folder.collection, *file));
        changes.entries_added.extend(unlisted);
    }
}

fn follow<'s>(
    collection: &KnownLinkedCollection,
    by_path: &BTreeMap<&Path, &'s ScannedFolder>,
    by_identity: &BTreeMap<FileIdentity, &'s ScannedFolder>,
) -> Option<&'s ScannedFolder> {
    let at_path = by_path
        .get(collection.folder_path.as_ref().as_path())
        .copied();
    match at_path {
        Some(folder) if folder.identity == collection.folder_identity => Some(folder),
        _ => by_identity
            .get(&collection.folder_identity)
            .copied()
            .or(at_path),
    }
}

fn is_listed(entries: &[KnownEntry], file: FileRef) -> bool {
    match file {
        FileRef::Existing(id) => entries.iter().any(|entry| entry.audio_file_id == id),
        FileRef::New(_) => false,
    }
}
