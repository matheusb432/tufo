//! Correlates import inputs with resources in one post-operation snapshot.
//!
//! Prepare a cloned path list with [`resolve_paths`] before the RPC, and send the original
//! request unchanged so the server's ignored paths retain their submitted spelling.
//! Correlation then compares path components without disk access.
//!
//! Matching identities can already exist or come from another import of the same source.
//! The filesystem can change between path preparation and server classification.

use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    path::Path,
    time::Duration,
};

use tufo_models::{
    library::{
        AudioFileId, BuiltinTitle, CollectionEntryId, CollectionId, CollectionKind, LibraryFolderId,
    },
    path::LocalPath,
};

use crate::library::{
    AddFilesToCollection, AddSongsToPlaylist, CollectionSource, EntryView, ImportPaths,
    LibrarySnapshot, SongSource,
};

const PATH_RESOLUTION_TIMEOUT: Duration = Duration::from_secs(30);

/// Resolves symlinks and parent components for snapshot correlation, preserving input order
/// and duplicates. Unresolvable or non-UTF-8 resolutions retain the original path.
/// The detached worker can outlive a timeout or cancellation without delaying runtime shutdown.
///
/// # Errors
/// Returns thread spawn or worker channel errors, or `TimedOut` after 30 seconds.
pub async fn resolve_paths(paths: Vec<LocalPath>) -> io::Result<Vec<LocalPath>> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    drop(
        std::thread::Builder::new()
            .name("tufo-import-paths".to_owned())
            .spawn(move || {
                let resolved = paths
                    .into_iter()
                    .map(|path| {
                        std::fs::canonicalize(path.as_ref())
                            .ok()
                            .and_then(|resolved| LocalPath::try_new(resolved).ok())
                            .unwrap_or(path)
                    })
                    .collect::<Vec<_>>();
                let _ = sender.send(resolved);
            })?,
    );
    tokio::time::timeout(PATH_RESOLUTION_TIMEOUT, receiver)
        .await
        .map_err(|error| io::Error::new(io::ErrorKind::TimedOut, error))?
        .map_err(io::Error::other)
}

/// Matching post-state identities, sorted and deduplicated within each resource kind.
/// These identities do not establish which resources the operation created.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResourceIds {
    pub collection_ids: Vec<CollectionId>,
    pub entry_ids: Vec<CollectionEntryId>,
    pub audio_file_ids: Vec<AudioFileId>,
    pub folder_ids: Vec<LibraryFolderId>,
}

/// Matches loose files exactly in manual collections and folder subtrees in linked ones.
/// Folder references also match enclosing or nested followed roots of the requested kind.
/// A nested folder with no visible folder or song evidence cannot be distinguished from an
/// ignored path, so it contributes no root identity.
#[must_use]
pub fn resources_for_paths(snapshot: &LibrarySnapshot, request: &ImportPaths) -> ResourceIds {
    let folders_by_id = snapshot
        .library_folders
        .iter()
        .filter(|folder| folder.kind == request.kind)
        .map(|folder| (folder.id, folder.folder.as_ref().as_path()))
        .collect::<BTreeMap<_, _>>();
    let folders_requested = folders_requested(snapshot, request, &folders_by_id);
    let mut resources = ResourceIds::default();

    for (id, folder) in &folders_by_id {
        if folders_requested
            .iter()
            .any(|path| folder.starts_with(path) || path.starts_with(folder))
        {
            resources.folder_ids.push(*id);
        }
    }

    for collection in snapshot
        .collections
        .iter()
        .filter(|collection| collection.kind == request.kind)
    {
        match &collection.source {
            CollectionSource::Linked { folder, .. } => {
                if !folders_requested
                    .iter()
                    .any(|path| folder.as_ref().starts_with(path))
                {
                    continue;
                }
                resources.collection_ids.push(collection.id);
                for entry in collection
                    .entries
                    .iter()
                    .filter(|entry| entry.path.as_ref().parent() == Some(folder.as_ref().as_path()))
                {
                    resources.add_entry(collection.id, entry);
                }
            }
            CollectionSource::Manual => {
                if !matches!(
                    collection.builtin_title,
                    None | Some(BuiltinTitle::Imported)
                ) {
                    continue;
                }
                for entry in collection
                    .entries
                    .iter()
                    .filter(|entry| path_requested_exactly(entry.path.as_ref(), &request.paths))
                {
                    resources.add_entry(collection.id, entry);
                }
            }
        }
    }

    if request.kind == CollectionKind::Playlist {
        for song in &snapshot.songs {
            let folder_matches = match song.source {
                SongSource::Folder(id) => {
                    folders_by_id
                        .get(&id)
                        .is_some_and(|folder| song.path.as_ref().starts_with(folder))
                        && folders_requested
                            .iter()
                            .any(|path| song.path.as_ref().starts_with(path))
                }
                SongSource::File => false,
            };
            if folder_matches || path_requested_exactly(song.path.as_ref(), &request.paths) {
                resources.audio_file_ids.push(song.audio_file_id);
            }
        }
    }

    resources.finish()
}

/// Matches files or folder descendants only within the requested manual collection.
/// Entries that already list a requested file are included; followed folders are not added.
#[must_use]
pub fn resources_for_collection_files(
    snapshot: &LibrarySnapshot,
    request: &AddFilesToCollection,
) -> ResourceIds {
    let mut resources = ResourceIds::default();
    if let Some(collection) = snapshot.collections.iter().find(|collection| {
        collection.id == request.collection_id && collection.source == CollectionSource::Manual
    }) {
        for entry in &collection.entries {
            if request
                .paths
                .iter()
                .any(|path| entry.path.as_ref().starts_with(path.as_ref()))
            {
                resources.add_entry(collection.id, entry);
            }
        }
    }
    resources.finish()
}

/// Matches requested audio identities only through entries in the target manual playlist.
/// Existing memberships are included, regardless of the operation's added count.
#[must_use]
pub fn resources_for_playlist_songs(
    snapshot: &LibrarySnapshot,
    request: &AddSongsToPlaylist,
) -> ResourceIds {
    let audio_file_ids = request
        .audio_file_ids
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    let mut resources = ResourceIds::default();
    if let Some(playlist) = snapshot.collections.iter().find(|collection| {
        collection.id == request.playlist_id
            && collection.kind == CollectionKind::Playlist
            && collection.source == CollectionSource::Manual
    }) {
        for entry in &playlist.entries {
            if audio_file_ids.contains(&entry.audio_file_id) {
                resources.add_entry(playlist.id, entry);
            }
        }
    }
    resources.finish()
}

fn folders_requested<'a>(
    snapshot: &LibrarySnapshot,
    request: &'a ImportPaths,
    folders_by_id: &BTreeMap<LibraryFolderId, &Path>,
) -> BTreeSet<&'a Path> {
    let mut folders_known = folders_by_id.values().copied().collect::<BTreeSet<_>>();
    for collection in &snapshot.collections {
        if collection.kind == request.kind
            && let CollectionSource::Linked { folder, .. } = &collection.source
        {
            folders_known.insert(folder.as_ref().as_path());
        }
    }
    if request.kind == CollectionKind::Playlist {
        for song in &snapshot.songs {
            if let SongSource::Folder(id) = song.source
                && let Some(folder) = folders_by_id.get(&id)
                && song.path.as_ref().starts_with(folder)
                && let Some(parent) = song.path.as_ref().parent()
            {
                folders_known.insert(parent);
            }
        }
    }
    request
        .paths
        .iter()
        .map(|path| path.as_ref().as_path())
        .filter(|path| folders_known.iter().any(|folder| folder.starts_with(path)))
        .collect()
}

fn path_requested_exactly(path: &Path, requested: &[LocalPath]) -> bool {
    requested.iter().any(|requested| path == requested.as_ref())
}

impl ResourceIds {
    fn add_entry(&mut self, collection_id: CollectionId, entry: &EntryView) {
        self.collection_ids.push(collection_id);
        self.entry_ids.push(entry.id);
        self.audio_file_ids.push(entry.audio_file_id);
    }

    fn finish(mut self) -> Self {
        sort_and_deduplicate(&mut self.collection_ids);
        sort_and_deduplicate(&mut self.entry_ids);
        sort_and_deduplicate(&mut self.audio_file_ids);
        sort_and_deduplicate(&mut self.folder_ids);
        self
    }
}

fn sort_and_deduplicate<T: Ord>(ids: &mut Vec<T>) {
    ids.sort_unstable();
    ids.dedup();
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, time::Duration};

    use anyhow::Result;
    use tufo_models::{
        library::{EntryOrder, SoundIcon, SoundSettings, Title},
        path::FolderPath,
        playback::AudioFilePath,
    };

    use super::*;
    use crate::library::{Background, CollectionView, LibraryFolderView, SongView};

    #[tokio::test]
    async fn resolution_canonicalizes_parent_components_for_correlation() -> Result<()> {
        let directory = tempfile::tempdir()?;
        std::fs::create_dir(directory.path().join("child"))?;
        let audio_path = directory.path().join("song.ogg");
        std::fs::write(&audio_path, b"synthetic fixture")?;
        let original = LocalPath::try_new(directory.path().join("child/../song.ogg"))?;
        let canonical = LocalPath::try_new(std::fs::canonicalize(&audio_path)?)?;
        let resolved = resolve_paths(vec![original.clone()]).await?;
        assert_eq!(resolved, vec![canonical.clone()]);

        let mut audio = entry(10, 10, "song.ogg")?;
        audio.path = AudioFilePath::try_new(canonical.as_ref().clone())?;
        let target = collection(
            10,
            CollectionKind::Playlist,
            CollectionSource::Manual,
            vec![audio.clone()],
        )?;
        let snapshot = LibrarySnapshot {
            collections: vec![target.clone()],
            ..LibrarySnapshot::default()
        };
        let original_request = AddFilesToCollection {
            collection_id: target.id,
            paths: vec![original],
        };
        assert_eq!(
            resources_for_collection_files(&snapshot, &original_request),
            ResourceIds::default()
        );
        let resolved_request = AddFilesToCollection {
            collection_id: target.id,
            paths: resolved,
        };
        assert_eq!(
            resources_for_collection_files(&snapshot, &resolved_request),
            ResourceIds {
                collection_ids: vec![target.id],
                entry_ids: vec![audio.id],
                audio_file_ids: vec![audio.audio_file_id],
                folder_ids: Vec::new(),
            }
        );
        assert_eq!(std::fs::read(&audio_path)?, b"synthetic fixture");
        Ok(())
    }

    #[tokio::test]
    async fn resolution_keeps_missing_and_untraversable_paths() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let file_path = directory.path().join("file.ogg");
        std::fs::write(&file_path, b"synthetic fixture")?;
        let missing = LocalPath::try_new(directory.path().join("missing/../song.ogg"))?;
        let untraversable = LocalPath::try_new(file_path.join("song.ogg"))?;
        let original = vec![missing, untraversable];
        assert_eq!(resolve_paths(original.clone()).await?, original);
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn resolution_follows_file_and_folder_symlinks_in_input_order() -> Result<()> {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir()?;
        let folder_path = directory.path().join("music");
        std::fs::create_dir(&folder_path)?;
        let audio_path = folder_path.join("song.ogg");
        std::fs::write(&audio_path, b"synthetic fixture")?;
        let file_alias = directory.path().join("song-link.ogg");
        let folder_alias = directory.path().join("music-link");
        symlink(&audio_path, &file_alias)?;
        symlink(&folder_path, &folder_alias)?;
        let original = vec![
            LocalPath::try_new(file_alias.clone())?,
            LocalPath::try_new(folder_alias)?,
            LocalPath::try_new(file_alias)?,
        ];
        let canonical_file = LocalPath::try_new(std::fs::canonicalize(&audio_path)?)?;
        let canonical_folder = LocalPath::try_new(std::fs::canonicalize(&folder_path)?)?;
        assert_eq!(
            resolve_paths(original).await?,
            vec![canonical_file.clone(), canonical_folder, canonical_file]
        );
        assert_eq!(std::fs::read(&audio_path)?, b"synthetic fixture");
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn resolution_keeps_aliases_with_non_utf8_targets() -> Result<()> {
        use std::{
            ffi::OsString,
            os::unix::{ffi::OsStringExt, fs::symlink},
        };

        let directory = tempfile::tempdir()?;
        let target = directory.path().join(OsString::from_vec(vec![0xff]));
        std::fs::write(&target, b"synthetic fixture")?;
        let alias = directory.path().join("song-link.ogg");
        symlink(&target, &alias)?;
        let original = LocalPath::try_new(alias)?;
        assert_eq!(resolve_paths(vec![original.clone()]).await?, vec![original]);
        Ok(())
    }

    #[test]
    fn folder_import_matches_only_linked_sources_of_requested_kind() -> Result<()> {
        let theme = entry(20, 40, "music/theme.ogg")?;
        let rain = entry(10, 30, "music/ambient/rain.ogg")?;
        let hidden = entry(50, 50, "music/hidden/night.ogg")?;
        let root = folder(70, CollectionKind::Playlist, "")?;
        let wrong_kind_root = folder(71, CollectionKind::Soundboard, "music")?;
        let unrelated_root = folder(72, CollectionKind::Playlist, "music-extra")?;
        let linked_theme = collection(
            20,
            CollectionKind::Playlist,
            linked("music")?,
            vec![theme.clone(), entry(99, 99, "unrelated/stray.ogg")?],
        )?;
        let linked_rain = collection(
            10,
            CollectionKind::Playlist,
            linked("music/ambient")?,
            vec![rain.clone()],
        )?;
        let empty_linked = collection(
            30,
            CollectionKind::Playlist,
            linked("music/empty")?,
            Vec::new(),
        )?;
        let snapshot = LibrarySnapshot {
            collections: vec![
                collection(
                    91,
                    CollectionKind::Playlist,
                    CollectionSource::Manual,
                    vec![entry(91, 40, "music/theme.ogg")?],
                )?,
                collection(
                    92,
                    CollectionKind::Soundboard,
                    linked("music")?,
                    vec![entry(92, 92, "music/effect.ogg")?],
                )?,
                collection(
                    93,
                    CollectionKind::Playlist,
                    linked("music-extra")?,
                    vec![entry(93, 93, "music-extra/theme.ogg")?],
                )?,
                collection(
                    94,
                    CollectionKind::Playlist,
                    linked("unrelated")?,
                    vec![entry(94, 40, "music/theme.ogg")?],
                )?,
                linked_theme.clone(),
                empty_linked.clone(),
                linked_rain.clone(),
            ],
            songs: vec![
                song(&theme, SongSource::Folder(root.id)),
                song(&hidden, SongSource::Folder(root.id)),
                song(&entry(95, 95, "music/manual.ogg")?, SongSource::File),
                song(
                    &entry(96, 96, "music-extra/song.ogg")?,
                    SongSource::Folder(unrelated_root.id),
                ),
            ],
            library_folders: vec![wrong_kind_root, unrelated_root, root.clone()],
            ..LibrarySnapshot::default()
        };
        let request = ImportPaths {
            kind: CollectionKind::Playlist,
            paths: paths(&["music", "readme.txt", "music"])?,
        };
        assert_eq!(
            resources_for_paths(&snapshot, &request),
            ResourceIds {
                collection_ids: vec![linked_rain.id, linked_theme.id, empty_linked.id],
                entry_ids: vec![rain.id, theme.id],
                audio_file_ids: vec![
                    rain.audio_file_id,
                    theme.audio_file_id,
                    hidden.audio_file_id
                ],
                folder_ids: vec![root.id],
            }
        );
        Ok(())
    }

    #[test]
    fn loose_file_import_matches_existing_manual_entries() -> Result<()> {
        let rain = entry(10, 20, "effects/rain.ogg")?;
        let manual = collection(
            10,
            CollectionKind::Soundboard,
            CollectionSource::Manual,
            vec![
                entry(99, 99, "effects/rain.ogg.backup")?,
                rain.clone(),
                rain.clone(),
            ],
        )?;
        let mut imported = collection(
            20,
            CollectionKind::Soundboard,
            CollectionSource::Manual,
            vec![entry(20, 20, "effects/rain.ogg")?],
        )?;
        imported.builtin_title = Some(BuiltinTitle::Imported);
        let mut seeded = collection(
            30,
            CollectionKind::Soundboard,
            CollectionSource::Manual,
            vec![entry(30, 20, "effects/rain.ogg")?],
        )?;
        seeded.builtin_title = Some(BuiltinTitle::Effects);
        let snapshot = LibrarySnapshot {
            collections: vec![
                seeded,
                imported.clone(),
                manual.clone(),
                collection(
                    40,
                    CollectionKind::Soundboard,
                    linked("effects")?,
                    vec![rain.clone()],
                )?,
                collection(
                    50,
                    CollectionKind::Playlist,
                    CollectionSource::Manual,
                    vec![rain.clone()],
                )?,
            ],
            songs: vec![song(&entry(60, 60, "effects/rain.ogg")?, SongSource::File)],
            library_folders: vec![folder(70, CollectionKind::Soundboard, "effects")?],
            ..LibrarySnapshot::default()
        };
        let request = ImportPaths {
            kind: CollectionKind::Soundboard,
            paths: paths(&["effects/rain.ogg", "effects/ignored.txt"])?,
        };
        let expected = ResourceIds {
            collection_ids: vec![manual.id, imported.id],
            entry_ids: vec![rain.id, imported.entries[0].id],
            audio_file_ids: vec![rain.audio_file_id],
            folder_ids: Vec::new(),
        };
        assert_eq!(resources_for_paths(&snapshot, &request), expected);
        let mut reordered = snapshot;
        reordered.collections.reverse();
        for collection in &mut reordered.collections {
            collection.entries.reverse();
        }
        assert_eq!(resources_for_paths(&reordered, &request), expected);
        Ok(())
    }

    #[test]
    fn ignored_paths_skip_enclosing_roots_and_empty_followed_roots_match() -> Result<()> {
        let root = folder(10, CollectionKind::Playlist, "music")?;
        let nested = folder(20, CollectionKind::Playlist, "other/ambient")?;
        let snapshot = LibrarySnapshot {
            library_folders: vec![nested.clone(), root.clone()],
            ..LibrarySnapshot::default()
        };
        let request = ImportPaths {
            kind: CollectionKind::Playlist,
            paths: paths(&["music/readme.txt", "music-extra", "missing"])?,
        };
        assert_eq!(
            resources_for_paths(&snapshot, &request),
            ResourceIds::default()
        );
        let folders = ImportPaths {
            kind: CollectionKind::Playlist,
            paths: paths(&["other", "music", "music"])?,
        };
        assert_eq!(
            resources_for_paths(&snapshot, &folders),
            ResourceIds {
                folder_ids: vec![root.id, nested.id],
                ..ResourceIds::default()
            }
        );
        Ok(())
    }

    #[test]
    fn music_folder_songs_include_hidden_collections() -> Result<()> {
        let root = folder(10, CollectionKind::Playlist, "music")?;
        let hidden = entry(20, 20, "music/hidden/song.ogg")?;
        let snapshot = LibrarySnapshot {
            songs: vec![song(&hidden, SongSource::Folder(root.id))],
            library_folders: vec![root.clone()],
            ..LibrarySnapshot::default()
        };
        let request = ImportPaths {
            kind: CollectionKind::Playlist,
            paths: paths(&["music/hidden"])?,
        };
        assert_eq!(
            resources_for_paths(&snapshot, &request),
            ResourceIds {
                audio_file_ids: vec![hidden.audio_file_id],
                folder_ids: vec![root.id],
                ..ResourceIds::default()
            }
        );
        Ok(())
    }

    #[test]
    fn collection_files_match_only_target_paths() -> Result<()> {
        let nested = entry(10, 20, "effects/ambient/rain.ogg")?;
        let exact = entry(20, 10, "music/theme.ogg")?;
        let target = collection(
            10,
            CollectionKind::Soundboard,
            CollectionSource::Manual,
            vec![
                exact.clone(),
                entry(99, 99, "effects-extra/wind.ogg")?,
                entry(98, 98, "music/theme.ogg.backup")?,
                nested.clone(),
            ],
        )?;
        let snapshot = LibrarySnapshot {
            collections: vec![
                collection(
                    20,
                    CollectionKind::Soundboard,
                    CollectionSource::Manual,
                    vec![
                        entry(30, 20, "effects/ambient/rain.ogg")?,
                        entry(40, 40, "effects/unrelated.ogg")?,
                    ],
                )?,
                target.clone(),
            ],
            songs: vec![song(&entry(50, 50, "effects/song.ogg")?, SongSource::File)],
            library_folders: vec![folder(60, CollectionKind::Soundboard, "effects")?],
            ..LibrarySnapshot::default()
        };
        let request = AddFilesToCollection {
            collection_id: target.id,
            paths: paths(&["effects", "music/theme.ogg", "ignored.txt", "effects"])?,
        };
        let expected = ResourceIds {
            collection_ids: vec![target.id],
            entry_ids: vec![nested.id, exact.id],
            audio_file_ids: vec![exact.audio_file_id, nested.audio_file_id],
            folder_ids: Vec::new(),
        };
        assert_eq!(
            resources_for_collection_files(&snapshot, &request),
            expected
        );
        let ignored = AddFilesToCollection {
            collection_id: target.id,
            paths: paths(&["ignored.txt"])?,
        };
        assert_eq!(
            resources_for_collection_files(&snapshot, &ignored),
            ResourceIds::default()
        );
        Ok(())
    }

    #[test]
    fn playlist_songs_preserve_existing_identities_under_concurrent_additions() -> Result<()> {
        let first = entry(10, 20, "music/first.ogg")?;
        let second = entry(20, 10, "music/second.ogg")?;
        let other = entry(99, 99, "music/other.ogg")?;
        let target = collection(
            10,
            CollectionKind::Playlist,
            CollectionSource::Manual,
            vec![second.clone(), first.clone(), first.clone()],
        )?;
        let unlisted = entry(30, 30, "music/unlisted.ogg")?;
        let mut snapshot = LibrarySnapshot {
            collections: vec![target.clone()],
            ..LibrarySnapshot::default()
        };
        let request = AddSongsToPlaylist {
            playlist_id: target.id,
            audio_file_ids: vec![
                first.audio_file_id,
                second.audio_file_id,
                first.audio_file_id,
                unlisted.audio_file_id,
            ],
        };
        let expected = ResourceIds {
            collection_ids: vec![target.id],
            entry_ids: vec![first.id, second.id],
            audio_file_ids: vec![second.audio_file_id, first.audio_file_id],
            folder_ids: Vec::new(),
        };
        assert_eq!(resources_for_playlist_songs(&snapshot, &request), expected);
        snapshot.collections[0].entries.push(other.clone());
        snapshot.collections.push(collection(
            20,
            CollectionKind::Playlist,
            CollectionSource::Manual,
            vec![entry(40, 20, "music/first.ogg")?, unlisted.clone()],
        )?);
        snapshot.songs = vec![
            song(&unlisted, SongSource::File),
            song(&other, SongSource::File),
        ];
        assert_eq!(resources_for_playlist_songs(&snapshot, &request), expected);
        let mut reordered = snapshot;
        reordered.collections.reverse();
        for collection in &mut reordered.collections {
            collection.entries.reverse();
        }
        assert_eq!(resources_for_playlist_songs(&reordered, &request), expected);
        Ok(())
    }

    #[test]
    fn targeted_additions_require_the_operation_collection_kind_and_source() -> Result<()> {
        let audio = entry(10, 10, "music/song.ogg")?;
        let linked_playlist = collection(
            10,
            CollectionKind::Playlist,
            linked("music")?,
            vec![audio.clone()],
        )?;
        let soundboard = collection(
            20,
            CollectionKind::Soundboard,
            CollectionSource::Manual,
            vec![audio.clone()],
        )?;
        let snapshot = LibrarySnapshot {
            collections: vec![linked_playlist.clone(), soundboard.clone()],
            ..LibrarySnapshot::default()
        };
        for playlist_id in [
            linked_playlist.id,
            soundboard.id,
            CollectionId::from_uuid(id_text(99).parse()?),
        ] {
            assert_eq!(
                resources_for_playlist_songs(
                    &snapshot,
                    &AddSongsToPlaylist {
                        playlist_id,
                        audio_file_ids: vec![audio.audio_file_id],
                    }
                ),
                ResourceIds::default()
            );
        }
        assert_eq!(
            resources_for_collection_files(
                &snapshot,
                &AddFilesToCollection {
                    collection_id: linked_playlist.id,
                    paths: paths(&["music"])?,
                }
            ),
            ResourceIds::default()
        );
        Ok(())
    }

    fn path(relative: &str) -> PathBuf {
        let root = if cfg!(windows) {
            r"C:\tufo-import-resources"
        } else {
            "/tufo-import-resources"
        };
        Path::new(root).join(relative)
    }

    fn paths(relative: &[&str]) -> Result<Vec<LocalPath>> {
        relative
            .iter()
            .map(|relative| Ok(LocalPath::try_new(path(relative))?))
            .collect()
    }

    fn id_text(number: u32) -> String {
        format!("00000000-0000-0000-0000-{number:012x}")
    }

    fn entry(id: u32, audio_file_id: u32, relative: &str) -> Result<EntryView> {
        Ok(EntryView {
            id: CollectionEntryId::from_uuid(id_text(id).parse()?),
            audio_file_id: AudioFileId::from_uuid(id_text(audio_file_id).parse()?),
            title: Title::from_name_lossy(relative),
            builtin_title: None,
            path: AudioFilePath::try_new(path(relative))?,
            duration: Duration::from_secs(1),
            added_at: "1970-01-01T00:00:00Z".parse()?,
            missing: false,
            sound: SoundSettings::default(),
            icon: SoundIcon::default(),
            saved_loop: None,
        })
    }

    fn collection(
        id: u32,
        kind: CollectionKind,
        source: CollectionSource,
        entries: Vec<EntryView>,
    ) -> Result<CollectionView> {
        Ok(CollectionView {
            id: CollectionId::from_uuid(id_text(id).parse()?),
            kind,
            title: Title::from_name_lossy("Collection"),
            builtin_title: None,
            background: Background::default(),
            order: EntryOrder::default(),
            source,
            entries,
            pinned: true,
        })
    }

    fn linked(relative: &str) -> Result<CollectionSource> {
        Ok(CollectionSource::Linked {
            folder: FolderPath::try_new(path(relative))?,
            missing: false,
        })
    }

    fn folder(id: u32, kind: CollectionKind, relative: &str) -> Result<LibraryFolderView> {
        Ok(LibraryFolderView {
            id: LibraryFolderId::from_uuid(id_text(id).parse()?),
            kind,
            folder: FolderPath::try_new(path(relative))?,
        })
    }

    fn song(entry: &EntryView, source: SongSource) -> SongView {
        SongView {
            audio_file_id: entry.audio_file_id,
            title: entry.title.clone(),
            builtin_title: entry.builtin_title,
            path: entry.path.clone(),
            duration: entry.duration,
            added_at: entry.added_at,
            missing: entry.missing,
            source,
            saved_loop: entry.saved_loop,
        }
    }
}
