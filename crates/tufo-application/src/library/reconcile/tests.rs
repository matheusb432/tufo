use std::{collections::HashMap, path::PathBuf, time::Duration};

use toasty::stmt::Uuid;
use tufo_models::{
    library::{
        AudioFileId, AudioFingerprint, CollectionEntryId, CollectionId, CollectionKind,
        FileIdentity, FileStamp, LibraryFolderId, Title,
    },
    path::FolderPath,
    playback::AudioFilePath,
};

use super::*;
use crate::library::library_files::{AudioFacts, FolderScan, ScannedFile, ScannedFolder};

const LIBRARY: &str = "rpg";

fn absolute(relative: &str) -> PathBuf {
    let root = if cfg!(windows) { r"C:\music" } else { "/music" };
    relative
        .split('/')
        .filter(|part| !part.is_empty())
        .fold(PathBuf::from(root), |path, part| path.join(part))
}

fn audio_path(relative: &str) -> AudioFilePath {
    AudioFilePath::try_new(absolute(relative)).unwrap()
}

fn folder_path(relative: &str) -> FolderPath {
    FolderPath::try_new(absolute(relative)).unwrap()
}

fn stamp(identity: &str, size_bytes: u64) -> FileStamp {
    FileStamp {
        identity: FileIdentity::try_new(identity.to_owned()).unwrap(),
        size_bytes,
        modified_ns: 1,
    }
}

fn fingerprint(seed: char) -> AudioFingerprint {
    AudioFingerprint::try_new(seed.to_string().repeat(64)).unwrap()
}

fn facts(seed: char) -> AudioFacts {
    AudioFacts {
        fingerprint: fingerprint(seed),
        duration: Duration::from_secs(60),
    }
}

fn id(seed: u128) -> Uuid {
    Uuid::from_u128(seed)
}

fn library_folder_id() -> LibraryFolderId {
    LibraryFolderId::from_uuid(id(1))
}

fn known_file(seed: u128, relative: &str, identity: &str, fingerprint_seed: char) -> KnownFile {
    KnownFile {
        id: AudioFileId::from_uuid(id(seed)),
        path: audio_path(relative),
        stamp: stamp(identity, 100),
        fingerprint: fingerprint(fingerprint_seed),
        missing: false,
    }
}

fn collection(
    seed: u128,
    relative_folder: &str,
    identity: &str,
    files: &[&KnownFile],
) -> KnownLinkedCollection {
    KnownLinkedCollection {
        id: CollectionId::from_uuid(id(seed)),
        library_folder_id: library_folder_id(),
        folder_path: folder_path(relative_folder),
        folder_identity: FileIdentity::try_new(identity.to_owned()).unwrap(),
        title: linked_collection_title(&folder_path(LIBRARY), &folder_path(relative_folder)),
        hidden: false,
        missing: false,
        entries: files
            .iter()
            .enumerate()
            .map(|(index, file)| KnownEntry {
                id: CollectionEntryId::from_uuid(id(seed * 1000 + index as u128)),
                audio_file_id: file.id,
            })
            .collect(),
    }
}

fn scanned_folder(relative: &str, identity: &str, files: &[(&str, FileStamp)]) -> ScannedFolder {
    ScannedFolder {
        path: folder_path(relative),
        identity: FileIdentity::try_new(identity.to_owned()).unwrap(),
        audio_files: files
            .iter()
            .map(|(relative, stamp)| ScannedFile {
                path: audio_path(relative),
                stamp: stamp.clone(),
            })
            .collect(),
    }
}

fn scanned_library(folders: Vec<ScannedFolder>) -> LibraryFolderScan {
    LibraryFolderScan {
        id: library_folder_id(),
        kind: CollectionKind::Playlist,
        path: folder_path(LIBRARY),
        outcome: ScanOutcome::Scanned(FolderScan {
            folders,
            truncated: false,
        }),
    }
}

/// Runs both phases, answering inspections from `facts_by_path`.
fn reconcile(input: ReconcileInput, facts_by_path: &[(&str, AudioFacts)]) -> ReconcileChanges {
    let pending = plan(input);
    let available = facts_by_path
        .iter()
        .map(|(relative, facts)| (absolute(relative), facts.clone()))
        .collect::<HashMap<_, _>>();
    let inspections = pending
        .inspections_needed()
        .into_iter()
        .map(|path| {
            let facts = available.get(path.as_ref()).cloned();
            (path.as_ref().clone(), facts)
        })
        .collect();
    pending.finish(&inspections)
}

#[test]
fn an_unchanged_library_changes_nothing() {
    let tavern = known_file(10, "rpg/ambient/tavern.mp3", "i:10", 'a');
    let input = ReconcileInput {
        collections: vec![collection(20, "rpg/ambient", "d:20", &[&tavern])],
        library_folders: vec![scanned_library(vec![
            scanned_folder("rpg", "d:1", &[]),
            scanned_folder(
                "rpg/ambient",
                "d:20",
                &[("rpg/ambient/tavern.mp3", stamp("i:10", 100))],
            ),
        ])],
        files: vec![tavern],
        ..ReconcileInput::default()
    };

    let pending = plan(input);

    assert!(pending.inspections_needed().is_empty());
    assert!(pending.finish(&HashMap::new()).is_empty());
}

#[test]
fn a_renamed_file_keeps_its_row_and_entry_without_rereading_it() {
    let tavern = known_file(10, "rpg/ambient/tavern.mp3", "i:10", 'a');
    let input = ReconcileInput {
        collections: vec![collection(20, "rpg/ambient", "d:20", &[&tavern])],
        library_folders: vec![scanned_library(vec![scanned_folder(
            "rpg/ambient",
            "d:20",
            &[("rpg/ambient/old tavern.mp3", stamp("i:10", 100))],
        )])],
        files: vec![tavern.clone()],
        ..ReconcileInput::default()
    };

    let changes = reconcile(input, &[]);

    assert_eq!(
        changes.file_changes,
        [FileChange::Updated {
            id: tavern.id,
            path: audio_path("rpg/ambient/old tavern.mp3"),
            stamp: stamp("i:10", 100),
            facts: None,
        }]
    );
    assert!(changes.entries_added.is_empty());
    assert!(changes.entries_removed.is_empty());
}

#[test]
fn a_file_moved_between_linked_folders_moves_its_entry() {
    let dragon = known_file(10, "rpg/ambient/dragon.mp3", "i:10", 'a');
    let ambient = collection(20, "rpg/ambient", "d:20", &[&dragon]);
    let boss = collection(30, "rpg/boss", "d:30", &[]);
    let removed_entry = ambient.entries[0].id;
    let input = ReconcileInput {
        collections: vec![ambient, boss],
        library_folders: vec![scanned_library(vec![
            scanned_folder("rpg/ambient", "d:20", &[]),
            scanned_folder(
                "rpg/boss",
                "d:30",
                &[("rpg/boss/dragon.mp3", stamp("i:10", 100))],
            ),
        ])],
        files: vec![dragon.clone()],
        ..ReconcileInput::default()
    };

    let changes = reconcile(input, &[]);

    assert_eq!(changes.entries_removed, [removed_entry]);
    assert_eq!(
        changes.entries_added,
        [(
            CollectionRef::Existing(CollectionId::from_uuid(id(30))),
            FileRef::Existing(dragon.id)
        )]
    );
}

#[test]
fn a_file_a_tagger_rewrote_in_place_is_read_again() {
    let tavern = known_file(10, "rpg/ambient/tavern.mp3", "i:10", 'a');
    let input = ReconcileInput {
        collections: vec![collection(20, "rpg/ambient", "d:20", &[&tavern])],
        library_folders: vec![scanned_library(vec![scanned_folder(
            "rpg/ambient",
            "d:20",
            &[("rpg/ambient/tavern.mp3", stamp("i:99", 120))],
        )])],
        files: vec![tavern.clone()],
        ..ReconcileInput::default()
    };

    let changes = reconcile(input, &[("rpg/ambient/tavern.mp3", facts('a'))]);

    assert_eq!(
        changes.file_changes,
        [FileChange::Updated {
            id: tavern.id,
            path: tavern.path.clone(),
            stamp: stamp("i:99", 120),
            facts: Some(facts('a')),
        }]
    );
    assert!(changes.new_files.is_empty());
}

#[test]
fn a_file_copied_to_another_volume_is_relinked_by_its_fingerprint() {
    let tavern = known_file(10, "rpg/ambient/tavern.mp3", "i:10", 'a');
    let input = ReconcileInput {
        collections: vec![collection(20, "rpg/ambient", "d:20", &[&tavern])],
        library_folders: vec![scanned_library(vec![scanned_folder(
            "rpg/ambient",
            "d:20",
            &[
                ("rpg/ambient/Tavern (remastered).mp3", stamp("i:55", 100)),
                ("rpg/ambient/rain.mp3", stamp("i:56", 100)),
            ],
        )])],
        files: vec![tavern.clone()],
        ..ReconcileInput::default()
    };

    let changes = reconcile(
        input,
        &[
            ("rpg/ambient/Tavern (remastered).mp3", facts('a')),
            ("rpg/ambient/rain.mp3", facts('b')),
        ],
    );

    assert_eq!(
        changes.file_changes,
        [FileChange::Updated {
            id: tavern.id,
            path: audio_path("rpg/ambient/Tavern (remastered).mp3"),
            stamp: stamp("i:55", 100),
            facts: Some(facts('a')),
        }]
    );
    assert_eq!(changes.new_files.len(), 1);
    assert_eq!(changes.new_files[0].title.as_str(), "rain");
}

#[test]
fn a_deleted_file_is_missing_and_leaves_its_linked_collection() {
    let tavern = known_file(10, "rpg/ambient/tavern.mp3", "i:10", 'a');
    let ambient = collection(20, "rpg/ambient", "d:20", &[&tavern]);
    let entry = ambient.entries[0].id;
    let input = ReconcileInput {
        collections: vec![ambient],
        library_folders: vec![scanned_library(vec![scanned_folder(
            "rpg/ambient",
            "d:20",
            &[],
        )])],
        files: vec![tavern.clone()],
        ..ReconcileInput::default()
    };

    let changes = reconcile(input, &[]);

    assert_eq!(
        changes.file_changes,
        [FileChange::Missing { id: tavern.id }]
    );
    assert_eq!(changes.entries_removed, [entry]);
}

#[test]
fn new_folders_with_audio_become_collections_titled_by_their_path() {
    let input = ReconcileInput {
        library_folders: vec![scanned_library(vec![
            scanned_folder("rpg", "d:1", &[]),
            scanned_folder(
                "rpg/FROSTMARCH/ash/boss",
                "d:2",
                &[("rpg/FROSTMARCH/ash/boss/ash.mp3", stamp("i:1", 100))],
            ),
            scanned_folder("rpg/empty", "d:3", &[]),
        ])],
        ..ReconcileInput::default()
    };

    let changes = reconcile(input, &[("rpg/FROSTMARCH/ash/boss/ash.mp3", facts('a'))]);

    let titles = changes
        .new_collections
        .iter()
        .map(|collection| collection.title.as_str())
        .collect::<Vec<_>>();
    assert_eq!(titles, ["FROSTMARCH / ash / boss"]);
    assert_eq!(
        changes.entries_added,
        [(CollectionRef::New(0), FileRef::New(0))]
    );
}

#[test]
fn a_renamed_folder_keeps_its_collection_and_retitles_a_derived_title() {
    let tavern = known_file(10, "rpg/ambient/tavern.mp3", "i:10", 'a');
    let input = ReconcileInput {
        collections: vec![collection(20, "rpg/ambient", "d:20", &[&tavern])],
        library_folders: vec![scanned_library(vec![scanned_folder(
            "rpg/calm",
            "d:20",
            &[("rpg/calm/tavern.mp3", stamp("i:10", 100))],
        )])],
        files: vec![tavern],
        ..ReconcileInput::default()
    };

    let changes = reconcile(input, &[]);

    assert_eq!(
        changes.collection_changes,
        [CollectionChange::Relinked {
            id: CollectionId::from_uuid(id(20)),
            path: folder_path("rpg/calm"),
            identity: FileIdentity::try_new("d:20".to_owned()).unwrap(),
            title: Some(Title::try_new("calm".to_owned()).unwrap()),
        }]
    );
    assert!(changes.new_collections.is_empty());
    assert!(changes.entries_added.is_empty() && changes.entries_removed.is_empty());
}

#[test]
fn a_vanished_library_folder_marks_everything_missing_but_keeps_entries() {
    let tavern = known_file(10, "rpg/ambient/tavern.mp3", "i:10", 'a');
    let input = ReconcileInput {
        collections: vec![collection(20, "rpg/ambient", "d:20", &[&tavern])],
        library_folders: vec![LibraryFolderScan {
            outcome: ScanOutcome::Gone,
            ..scanned_library(Vec::new())
        }],
        files: vec![tavern.clone()],
        stat_results: HashMap::from([(tavern.id, None)]),
    };

    let changes = reconcile(input, &[]);

    assert_eq!(
        changes.file_changes,
        [FileChange::Missing { id: tavern.id }]
    );
    assert_eq!(
        changes.collection_changes,
        [CollectionChange::Missing {
            id: CollectionId::from_uuid(id(20))
        }]
    );
    assert!(changes.entries_removed.is_empty());
}

#[test]
fn an_unreadable_library_folder_leaves_its_files_alone() {
    let tavern = known_file(10, "rpg/ambient/tavern.mp3", "i:10", 'a');
    let input = ReconcileInput {
        collections: vec![collection(20, "rpg/ambient", "d:20", &[&tavern])],
        library_folders: vec![LibraryFolderScan {
            outcome: ScanOutcome::Unreadable,
            ..scanned_library(Vec::new())
        }],
        files: vec![tavern],
        ..ReconcileInput::default()
    };

    assert!(reconcile(input, &[]).is_empty());
}

#[test]
fn a_hidden_collection_stays_empty_while_its_folder_keeps_adding_files() {
    let mut deleted = collection(20, "rpg/boss", "d:20", &[]);
    deleted.hidden = true;
    let input = ReconcileInput {
        collections: vec![deleted],
        library_folders: vec![scanned_library(vec![scanned_folder(
            "rpg/boss",
            "d:20",
            &[("rpg/boss/dragon.mp3", stamp("i:1", 100))],
        )])],
        ..ReconcileInput::default()
    };

    let changes = reconcile(input, &[("rpg/boss/dragon.mp3", facts('a'))]);

    assert_eq!(changes.new_files.len(), 1);
    assert!(changes.new_collections.is_empty());
    assert!(changes.entries_added.is_empty());
}

#[test]
fn unreadable_audio_is_reported_and_left_out() {
    let input = ReconcileInput {
        library_folders: vec![scanned_library(vec![scanned_folder(
            "rpg/boss",
            "d:20",
            &[("rpg/boss/broken.mp3", stamp("i:1", 100))],
        )])],
        ..ReconcileInput::default()
    };

    let changes = reconcile(input, &[]);

    assert_eq!(
        changes.unreadable_files,
        [audio_path("rpg/boss/broken.mp3")]
    );
    assert!(changes.new_files.is_empty() && changes.new_collections.is_empty());
}

#[test]
fn files_outside_library_folders_are_judged_by_their_stat() {
    let loose = known_file(10, "downloads/thunder.wav", "i:10", 'a');
    let still_there = known_file(11, "downloads/rain.wav", "i:11", 'b');
    let files = vec![loose.clone(), still_there.clone()];

    assert_eq!(files_to_stat(&files, &[]).len(), 2);
    let changes = reconcile(
        ReconcileInput {
            stat_results: HashMap::from([
                (loose.id, None),
                (still_there.id, Some(still_there.stamp.clone())),
            ]),
            files,
            ..ReconcileInput::default()
        },
        &[],
    );

    assert_eq!(changes.file_changes, [FileChange::Missing { id: loose.id }]);
}

#[test]
fn two_files_that_swapped_names_swap_paths() {
    let left = known_file(10, "rpg/boss/left.mp3", "i:10", 'a');
    let right = known_file(11, "rpg/boss/right.mp3", "i:11", 'b');
    let input = ReconcileInput {
        collections: vec![collection(20, "rpg/boss", "d:20", &[&left, &right])],
        library_folders: vec![scanned_library(vec![scanned_folder(
            "rpg/boss",
            "d:20",
            &[
                ("rpg/boss/left.mp3", stamp("i:11", 100)),
                ("rpg/boss/right.mp3", stamp("i:10", 100)),
            ],
        )])],
        files: vec![left.clone(), right.clone()],
        ..ReconcileInput::default()
    };

    let changes = reconcile(input, &[]);

    let moved = changes
        .file_changes
        .iter()
        .filter_map(|change| match change {
            FileChange::Updated { id, path, .. } => Some((*id, path.clone())),
            FileChange::Found { .. } | FileChange::Missing { .. } => None,
        })
        .collect::<HashMap<_, _>>();
    assert_eq!(moved.len(), changes.file_changes.len());
    assert_eq!(moved[&left.id], right.path);
    assert_eq!(moved[&right.id], left.path);
}

#[test]
fn a_hidden_soundboard_leaves_the_same_folders_playlist_following() {
    let soundboards = LibraryFolderId::from_uuid(id(2));
    let mut hidden = collection(30, "rpg/boss", "d:20", &[]);
    hidden.library_folder_id = soundboards;
    hidden.hidden = true;
    let files = [("rpg/boss/dragon.mp3", stamp("i:1", 100))];
    let input = ReconcileInput {
        collections: vec![hidden, collection(20, "rpg/boss", "d:20", &[])],
        library_folders: vec![
            scanned_library(vec![scanned_folder("rpg/boss", "d:20", &files)]),
            LibraryFolderScan {
                id: soundboards,
                kind: CollectionKind::Soundboard,
                ..scanned_library(vec![scanned_folder("rpg/boss", "d:20", &files)])
            },
        ],
        ..ReconcileInput::default()
    };

    let changes = reconcile(input, &[("rpg/boss/dragon.mp3", facts('a'))]);

    assert_eq!(changes.new_files.len(), 1);
    assert_eq!(
        changes.entries_added,
        [(
            CollectionRef::Existing(CollectionId::from_uuid(id(20))),
            FileRef::New(0)
        )]
    );
}
