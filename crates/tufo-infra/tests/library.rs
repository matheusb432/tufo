//! The library operations over a real SQLite database and real folders.

use std::{collections::BTreeSet, fs, sync::Arc, time::Duration};

use support::wav_bytes;
use tufo_application::library::{
    Library,
    add_files_to_collection::{self, AddFilesToCollectionError},
    add_songs_to_playlist::{self, AddSongsToPlaylistError},
    create_collection, delete_collection, delete_library_folder,
    delete_song::{self, DeleteSongError},
    folder_watcher, import_paths,
    seed_default_effects::{self, DefaultEffectFile, SeedDefaultEffectsOutcome},
    sync_library, update_collection,
};
use tufo_infra::{
    database::open_database, folder_watcher::NotifyFolderWatcher, library_files::DiskLibraryFiles,
};
use tufo_models::{
    library::{
        AudioFileId, BackgroundPreset, CollectionId, CollectionKind, SoundIcon, Title,
        default_effects::DefaultEffect,
    },
    path::{ImageFilePath, LocalPath},
    playback::AudioFilePath,
};
use tufo_wire::library::{
    AddFilesToCollection, AddSongsToPlaylist, Background, CollectionSource, CollectionView,
    CreateCollection, ImportPaths, LibrarySnapshot, SongSource, SongView, UpdateCollection,
};

mod support;

struct Fixture {
    library: Library<DiskLibraryFiles>,
    music: tempfile::TempDir,
    _database_directory: tempfile::TempDir,
}

impl Fixture {
    async fn new() -> anyhow::Result<Self> {
        let database_directory = tempfile::tempdir()?;
        let database = open_database(&database_directory.path().join("tufo.sqlite3")).await?;
        Ok(Self {
            library: Library::open(database, DiskLibraryFiles).await?,
            music: tempfile::tempdir()?,
            _database_directory: database_directory,
        })
    }

    fn write_track(&self, relative: &str, frequency: f64) -> anyhow::Result<()> {
        let path = self.music.path().join(relative);
        fs::create_dir_all(path.parent().unwrap_or(self.music.path()))?;
        fs::write(path, wav_bytes(1, frequency, None))?;
        Ok(())
    }

    fn local(&self, relative: &str) -> anyhow::Result<LocalPath> {
        Ok(LocalPath::try_new(self.music.path().join(relative))?)
    }

    fn unwritten_effect_file(&self, effect: DefaultEffect) -> anyhow::Result<DefaultEffectFile> {
        let path = self.music.path().join(format!("{}.wav", effect.title()));
        Ok(DefaultEffectFile {
            effect,
            path: AudioFilePath::try_new(path)?,
        })
    }

    fn snapshot(&self) -> Arc<LibrarySnapshot> {
        self.library.watch().borrow().clone()
    }

    async fn import(&self, kind: CollectionKind, relative_paths: &[&str]) -> anyhow::Result<()> {
        let paths = relative_paths
            .iter()
            .map(|relative| self.local(relative))
            .collect::<anyhow::Result<_>>()?;
        import_paths::execute(ImportPaths { kind, paths }, &self.library).await?;
        Ok(())
    }

    async fn create_playlist(&self, title: &str) -> anyhow::Result<CollectionId> {
        let request = CreateCollection {
            kind: CollectionKind::Playlist,
            title: Title::try_new(title.to_owned())?,
            background: Background::Gradient,
        };
        Ok(create_collection::execute(request, &self.library).await?)
    }
}

fn titled<'a>(snapshot: &'a LibrarySnapshot, title: &str) -> Option<&'a CollectionView> {
    snapshot
        .collections
        .iter()
        .find(|collection| collection.title.as_str() == title)
}

fn song_titles(snapshot: &LibrarySnapshot) -> Vec<&str> {
    snapshot
        .songs
        .iter()
        .map(|song| song.title.as_str())
        .collect()
}

fn song<'a>(snapshot: &'a LibrarySnapshot, title: &str) -> Option<&'a SongView> {
    snapshot
        .songs
        .iter()
        .find(|song| song.title.as_str() == title)
}

fn entry_titles(collection: &CollectionView) -> Vec<&str> {
    collection
        .entries
        .iter()
        .map(|entry| entry.title.as_str())
        .collect()
}

#[tokio::test]
async fn a_dropped_folder_becomes_one_sorted_playlist_per_folder_with_audio() {
    let fixture = Fixture::new().await.unwrap();
    fixture.write_track("rpg/boss/Track 10.wav", 440.0).unwrap();
    fixture.write_track("rpg/boss/track 2.wav", 450.0).unwrap();
    fixture
        .write_track("rpg/FROSTMARCH/ash/boss/ash.wav", 460.0)
        .unwrap();
    fs::write(fixture.music.path().join("rpg/boss/cover.jpg"), b"jpeg").unwrap();

    let report = import_paths::execute(
        ImportPaths {
            kind: CollectionKind::Playlist,
            paths: vec![fixture.local("rpg").unwrap()],
        },
        &fixture.library,
    )
    .await
    .unwrap();

    assert_eq!(report.collections_created, 2);
    assert_eq!(report.files_added, 3);
    let snapshot = fixture.snapshot();
    let titles = snapshot
        .collections
        .iter()
        .map(|collection| collection.title.as_str())
        .collect::<Vec<_>>();
    assert_eq!(titles, ["boss", "FROSTMARCH / ash / boss"]);
    assert_eq!(
        entry_titles(titled(&snapshot, "boss").unwrap()),
        ["track 2", "Track 10"]
    );
}

#[tokio::test]
async fn collections_from_one_drop_show_different_presets() {
    let fixture = Fixture::new().await.unwrap();
    for (index, folder) in ["boss", "tavern", "travel", "town"].into_iter().enumerate() {
        let frequency = 440.0 + 10.0 * f64::from(u8::try_from(index).unwrap());
        fixture
            .write_track(&format!("rpg/{folder}/theme.wav"), frequency)
            .unwrap();
    }
    fixture.write_track("loose/door.wav", 500.0).unwrap();

    import_paths::execute(
        ImportPaths {
            kind: CollectionKind::Playlist,
            paths: vec![
                fixture.local("rpg").unwrap(),
                fixture.local("loose/door.wav").unwrap(),
            ],
        },
        &fixture.library,
    )
    .await
    .unwrap();

    let snapshot = fixture.snapshot();
    let presets = snapshot
        .collections
        .iter()
        .filter_map(|collection| match collection.background {
            Background::Preset(preset) => Some(preset),
            Background::Gradient | Background::Image(_) => None,
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(snapshot.collections.len(), 5);
    assert_eq!(presets.len(), 5);
}

#[tokio::test]
async fn a_collection_shows_one_background_at_a_time() {
    let fixture = Fixture::new().await.unwrap();
    let id = create_collection::execute(
        CreateCollection {
            kind: CollectionKind::Playlist,
            title: Title::try_new("Tavern".to_owned()).unwrap(),
            background: Background::Preset(BackgroundPreset::Tavern),
        },
        &fixture.library,
    )
    .await
    .unwrap();
    let image =
        Background::Image(ImageFilePath::try_new(fixture.music.path().join("tavern.png")).unwrap());
    let background_after = |background: Background| {
        let library = &fixture.library;
        async move {
            let update = UpdateCollection {
                background: Some(background),
                ..UpdateCollection::default()
            };
            update_collection::execute(id, update, library)
                .await
                .unwrap();
            titled(&library.watch().borrow(), "Tavern")
                .unwrap()
                .background
                .clone()
        }
    };

    assert_eq!(
        titled(&fixture.snapshot(), "Tavern").unwrap().background,
        Background::Preset(BackgroundPreset::Tavern)
    );
    assert_eq!(background_after(image.clone()).await, image);
    let storm = Background::Preset(BackgroundPreset::Storm);
    assert_eq!(background_after(storm.clone()).await, storm);
    assert_eq!(
        background_after(Background::Gradient).await,
        Background::Gradient
    );
}

#[tokio::test]
async fn linked_playlists_follow_added_and_renamed_files() {
    let fixture = Fixture::new().await.unwrap();
    fixture.write_track("rpg/boss/dragon.wav", 440.0).unwrap();
    import_paths::execute(
        ImportPaths {
            kind: CollectionKind::Playlist,
            paths: vec![fixture.local("rpg").unwrap()],
        },
        &fixture.library,
    )
    .await
    .unwrap();
    let dragon = titled(&fixture.snapshot(), "boss").unwrap().entries[0].clone();

    fixture.write_track("rpg/boss/lich.wav", 450.0).unwrap();
    let boss = fixture.music.path().join("rpg/boss");
    fs::rename(boss.join("dragon.wav"), boss.join("red dragon.wav")).unwrap();
    sync_library::execute(&fixture.library).await.unwrap();

    let snapshot = fixture.snapshot();
    let playlist = titled(&snapshot, "boss").unwrap();
    assert_eq!(entry_titles(playlist), ["dragon", "lich"]);
    let renamed = &playlist.entries[0];
    assert_eq!(renamed.id, dragon.id);
    assert_eq!(renamed.added_at, dragon.added_at);
    assert!(renamed.path.as_ref().ends_with("red dragon.wav"));
}

#[tokio::test]
async fn a_deleted_linked_playlist_stays_deleted_until_its_folder_is_dropped_again() {
    let fixture = Fixture::new().await.unwrap();
    fixture.write_track("rpg/boss/dragon.wav", 440.0).unwrap();
    let import = || ImportPaths {
        kind: CollectionKind::Playlist,
        paths: vec![fixture.local("rpg/boss").unwrap()],
    };
    import_paths::execute(import(), &fixture.library)
        .await
        .unwrap();
    let boss = titled(&fixture.snapshot(), "boss").unwrap().id;

    delete_collection::execute(boss, &fixture.library)
        .await
        .unwrap();
    fixture.write_track("rpg/boss/lich.wav", 450.0).unwrap();
    sync_library::execute(&fixture.library).await.unwrap();
    let deleted = fixture.snapshot();
    assert!(deleted.collections.is_empty());
    assert_eq!(song_titles(&deleted), ["dragon", "lich"]);

    import_paths::execute(import(), &fixture.library)
        .await
        .unwrap();
    let restored = fixture.snapshot();
    assert_eq!(titled(&restored, "boss").unwrap().id, boss);
    assert_eq!(titled(&restored, "boss").unwrap().entries.len(), 2);
    assert!(fs::exists(fixture.music.path().join("rpg/boss/dragon.wav")).unwrap());
}

#[tokio::test]
async fn a_deleted_hand_picked_playlist_leaves_its_songs() {
    let fixture = Fixture::new().await.unwrap();
    fixture.write_track("loose/ambush.wav", 440.0).unwrap();
    fixture.write_track("loose/chase.wav", 450.0).unwrap();
    fixture
        .import(
            CollectionKind::Playlist,
            &["loose/ambush.wav", "loose/chase.wav"],
        )
        .await
        .unwrap();
    let loose = titled(&fixture.snapshot(), "loose").unwrap().id;

    delete_collection::execute(loose, &fixture.library)
        .await
        .unwrap();

    let snapshot = fixture.snapshot();
    assert!(snapshot.collections.is_empty());
    assert_eq!(song_titles(&snapshot), ["ambush", "chase"]);
    assert_eq!(song(&snapshot, "ambush").unwrap().source, SongSource::File);
}

#[tokio::test]
async fn songs_join_a_hand_picked_playlist_once() {
    let fixture = Fixture::new().await.unwrap();
    fixture.write_track("rpg/boss/dragon.wav", 440.0).unwrap();
    fixture
        .import(CollectionKind::Playlist, &["rpg"])
        .await
        .unwrap();
    let session = fixture.create_playlist("Session 12").await.unwrap();
    let snapshot = fixture.snapshot();
    let boss = titled(&snapshot, "boss").unwrap().id;
    let dragon = song(&snapshot, "dragon").unwrap();
    assert!(matches!(dragon.source, SongSource::Folder(_)));
    let add_to = |playlist_id, audio_file_id| {
        add_songs_to_playlist::execute(
            AddSongsToPlaylist {
                playlist_id,
                audio_file_ids: vec![audio_file_id],
            },
            &fixture.library,
        )
    };

    let first = add_to(session, dragon.audio_file_id).await.unwrap();
    let again = add_to(session, dragon.audio_file_id).await.unwrap();
    let linked = add_to(boss, dragon.audio_file_id).await;
    let unknown = add_to(session, AudioFileId::from_uuid(uuid::Uuid::from_u128(7))).await;

    assert_eq!((first, again), (1, 0));
    assert!(matches!(
        linked,
        Err(AddSongsToPlaylistError::Linked { .. })
    ));
    assert!(matches!(
        unknown,
        Err(AddSongsToPlaylistError::SongNotFound)
    ));
    assert_eq!(
        entry_titles(titled(&fixture.snapshot(), "Session 12").unwrap()),
        ["dragon"]
    );
}

#[tokio::test]
async fn a_removed_song_stays_in_the_playlists_that_list_it() {
    let fixture = Fixture::new().await.unwrap();
    fixture.write_track("loose/ambush.wav", 440.0).unwrap();
    fixture.write_track("rpg/dragon.wav", 450.0).unwrap();
    fixture
        .import(CollectionKind::Playlist, &["loose/ambush.wav"])
        .await
        .unwrap();
    fixture
        .import(CollectionKind::Playlist, &["rpg"])
        .await
        .unwrap();
    let snapshot = fixture.snapshot();
    let ambush = song(&snapshot, "ambush").unwrap().audio_file_id;
    let dragon = song(&snapshot, "dragon").unwrap().audio_file_id;

    delete_song::execute(ambush, &fixture.library)
        .await
        .unwrap();
    let followed = delete_song::execute(dragon, &fixture.library).await;

    let snapshot = fixture.snapshot();
    assert_eq!(song_titles(&snapshot), ["dragon"]);
    assert_eq!(
        entry_titles(titled(&snapshot, "loose").unwrap()),
        ["ambush"]
    );
    assert!(matches!(
        followed,
        Err(DeleteSongError::InFollowedFolder { .. })
    ));
}

#[tokio::test]
async fn an_unfollowed_folder_takes_its_playlists_but_not_hand_picked_entries() {
    let fixture = Fixture::new().await.unwrap();
    fixture.write_track("rpg/boss/dragon.wav", 440.0).unwrap();
    fixture.write_track("rpg/ambient/rain.wav", 450.0).unwrap();
    fixture
        .import(CollectionKind::Playlist, &["rpg"])
        .await
        .unwrap();
    let session = fixture.create_playlist("Session 12").await.unwrap();
    let snapshot = fixture.snapshot();
    add_songs_to_playlist::execute(
        AddSongsToPlaylist {
            playlist_id: session,
            audio_file_ids: vec![song(&snapshot, "dragon").unwrap().audio_file_id],
        },
        &fixture.library,
    )
    .await
    .unwrap();

    delete_library_folder::execute(snapshot.library_folders[0].id, &fixture.library)
        .await
        .unwrap();
    sync_library::execute(&fixture.library).await.unwrap();

    let snapshot = fixture.snapshot();
    let titles = snapshot
        .collections
        .iter()
        .map(|collection| collection.title.as_str())
        .collect::<Vec<_>>();
    assert_eq!(titles, ["Session 12"]);
    assert_eq!(entry_titles(&snapshot.collections[0]), ["dragon"]);
    assert!(snapshot.songs.is_empty());
    assert!(snapshot.library_folders.is_empty());
}

#[tokio::test]
async fn dropping_a_parent_folder_absorbs_a_followed_child() {
    let fixture = Fixture::new().await.unwrap();
    fixture.write_track("rpg/boss/dragon.wav", 440.0).unwrap();
    fixture.write_track("rpg/ambient/rain.wav", 450.0).unwrap();
    for folder in ["rpg/boss", "rpg"] {
        import_paths::execute(
            ImportPaths {
                kind: CollectionKind::Playlist,
                paths: vec![fixture.local(folder).unwrap()],
            },
            &fixture.library,
        )
        .await
        .unwrap();
    }

    let snapshot = fixture.snapshot();
    let titles = snapshot
        .collections
        .iter()
        .map(|collection| collection.title.as_str())
        .collect::<Vec<_>>();
    assert_eq!(titles, ["ambient", "boss"]);
}

#[tokio::test]
async fn manual_soundboards_take_files_but_linked_ones_refuse_them() {
    let fixture = Fixture::new().await.unwrap();
    fixture.write_track("sfx/door.wav", 440.0).unwrap();
    fixture.write_track("elsewhere/thunder.wav", 450.0).unwrap();
    import_paths::execute(
        ImportPaths {
            kind: CollectionKind::Soundboard,
            paths: vec![fixture.local("sfx").unwrap()],
        },
        &fixture.library,
    )
    .await
    .unwrap();
    let linked = titled(&fixture.snapshot(), "sfx").unwrap().id;
    let manual = create_collection::execute(
        CreateCollection {
            kind: CollectionKind::Soundboard,
            title: Title::try_new("Storm".to_owned()).unwrap(),
            background: Background::Gradient,
        },
        &fixture.library,
    )
    .await
    .unwrap();

    let refused = add_files_to_collection::execute(
        AddFilesToCollection {
            collection_id: linked,
            paths: vec![fixture.local("elsewhere/thunder.wav").unwrap()],
        },
        &fixture.library,
    )
    .await;
    let added = add_files_to_collection::execute(
        AddFilesToCollection {
            collection_id: manual,
            paths: vec![
                fixture.local("elsewhere").unwrap(),
                fixture.local("sfx/door.wav").unwrap(),
            ],
        },
        &fixture.library,
    )
    .await
    .unwrap();

    assert!(matches!(
        refused,
        Err(AddFilesToCollectionError::Linked { .. })
    ));
    assert_eq!(added.files_added, 2);
    let snapshot = fixture.snapshot();
    let storm = titled(&snapshot, "Storm").unwrap();
    assert_eq!(storm.source, CollectionSource::Manual);
    assert_eq!(entry_titles(storm), ["door", "thunder"]);
    // The door is one file listed twice, not two files.
    assert_eq!(
        storm.entries[0].audio_file_id,
        titled(&snapshot, "sfx").unwrap().entries[0].audio_file_id
    );
}

#[tokio::test]
async fn the_watcher_adds_a_copied_file_without_a_manual_sync() {
    let fixture = Fixture::new().await.unwrap();
    fixture.write_track("rpg/boss/dragon.wav", 440.0).unwrap();
    import_paths::execute(
        ImportPaths {
            kind: CollectionKind::Playlist,
            paths: vec![fixture.local("rpg").unwrap()],
        },
        &fixture.library,
    )
    .await
    .unwrap();
    let watcher = NotifyFolderWatcher::start();
    let mut snapshots = fixture.library.watch();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let follow = folder_watcher::run(&fixture.library, &watcher, async {
        let _ = stopped.await;
    });
    let copy_then_wait = async {
        // The loop reconciles at start; wait until it watches before copying.
        tokio::time::sleep(Duration::from_millis(300)).await;
        fixture.write_track("rpg/boss/lich.wav", 450.0).unwrap();
        tokio::time::timeout(
            Duration::from_secs(15),
            snapshots.wait_for(|snapshot| {
                titled(snapshot, "boss").is_some_and(|boss| boss.entries.len() == 2)
            }),
        )
        .await
        .map(|result| result.is_ok())
    };

    let ((), copied) = tokio::join!(follow, async {
        let copied = copy_then_wait.await;
        let _ = stop.send(());
        copied
    });

    assert_eq!(copied, Ok(true));
}

#[tokio::test]
async fn dropping_a_folder_as_music_leaves_its_deleted_soundboard_deleted() {
    let fixture = Fixture::new().await.unwrap();
    fixture.write_track("sfx/door.wav", 440.0).unwrap();
    let drop_on = |kind| ImportPaths {
        kind,
        paths: vec![fixture.local("sfx").unwrap()],
    };
    import_paths::execute(drop_on(CollectionKind::Soundboard), &fixture.library)
        .await
        .unwrap();
    let soundboard = titled(&fixture.snapshot(), "sfx").unwrap().id;
    delete_collection::execute(soundboard, &fixture.library)
        .await
        .unwrap();

    import_paths::execute(drop_on(CollectionKind::Playlist), &fixture.library)
        .await
        .unwrap();

    let kinds = fixture
        .snapshot()
        .collections
        .iter()
        .map(|collection| collection.kind)
        .collect::<Vec<_>>();
    assert_eq!(kinds, [CollectionKind::Playlist]);
}

#[tokio::test]
async fn the_effects_soundboard_arrives_once_with_looping_sounds_and_their_icons() {
    let fixture = Fixture::new().await.unwrap();
    let files = [DefaultEffect::SummerNight, DefaultEffect::Rain]
        .into_iter()
        .map(|effect| fixture.unwritten_effect_file(effect).unwrap())
        .collect::<Vec<_>>();
    for file in &files {
        fs::write(file.path.as_ref(), wav_bytes(1, 440.0, None)).unwrap();
    }

    let first = seed_default_effects::execute(files.clone(), &fixture.library)
        .await
        .unwrap();
    let effects = titled(&fixture.snapshot(), "Effects").unwrap().clone();
    delete_collection::execute(effects.id, &fixture.library)
        .await
        .unwrap();
    let second = seed_default_effects::execute(files, &fixture.library)
        .await
        .unwrap();

    assert_eq!(first, SeedDefaultEffectsOutcome::Seeded);
    assert_eq!(effects.kind, CollectionKind::Soundboard);
    assert_eq!(
        effects.builtin_title,
        Some(tufo_models::library::BuiltinTitle::Effects)
    );
    assert_eq!(
        effects.entries[0].builtin_title,
        Some(tufo_models::library::BuiltinTitle::Rain)
    );
    assert_eq!(effects.source, CollectionSource::Manual);
    assert_eq!(
        effects.background,
        Background::Preset(BackgroundPreset::Forest)
    );
    let sounds = effects
        .entries
        .iter()
        .map(|entry| (entry.title.as_str(), entry.icon, entry.sound.looping))
        .collect::<Vec<_>>();
    assert_eq!(
        sounds,
        [
            ("Rain", SoundIcon::CloudRain, true),
            ("Summer Night", SoundIcon::MoonStar, true),
        ]
    );
    assert_eq!(second, SeedDefaultEffectsOutcome::AlreadySeeded);
    assert!(titled(&fixture.snapshot(), "Effects").is_none());
}

#[tokio::test]
async fn unreadable_effects_leave_the_soundboard_for_a_later_start() {
    let fixture = Fixture::new().await.unwrap();
    let rain = fixture.unwritten_effect_file(DefaultEffect::Rain).unwrap();

    let unreadable = seed_default_effects::execute(vec![rain.clone()], &fixture.library)
        .await
        .unwrap();
    let before = fixture.snapshot();
    fs::write(rain.path.as_ref(), wav_bytes(1, 440.0, None)).unwrap();
    let retried = seed_default_effects::execute(vec![rain.clone()], &fixture.library)
        .await
        .unwrap();

    assert_eq!(
        unreadable,
        SeedDefaultEffectsOutcome::NothingReadable {
            unreadable: vec![rain.path]
        }
    );
    assert!(before.collections.is_empty());
    assert_eq!(retried, SeedDefaultEffectsOutcome::Seeded);
    assert!(titled(&fixture.snapshot(), "Effects").is_some());
}

#[tokio::test]
async fn renaming_seeded_content_clears_its_translation_key() {
    use tufo_application::library::update_collection_entry;
    use tufo_wire::library::UpdateCollectionEntry;

    let fixture = Fixture::new().await.unwrap();
    let rain = fixture.unwritten_effect_file(DefaultEffect::Rain).unwrap();
    fs::write(rain.path.as_ref(), wav_bytes(1, 440.0, None)).unwrap();
    seed_default_effects::execute(vec![rain], &fixture.library)
        .await
        .unwrap();
    let player = tufo_infra::player::EnginePlayer::start(
        tufo_infra::discord::SongbirdDiscordClient::not_configured(),
    );
    let effects = titled(&fixture.snapshot(), "Effects").unwrap().clone();

    update_collection::execute(
        effects.id,
        UpdateCollection {
            title: Some(Title::from_name_lossy("Weather")),
            ..UpdateCollection::default()
        },
        &fixture.library,
    )
    .await
    .unwrap();
    update_collection_entry::execute(
        effects.entries[0].id,
        UpdateCollectionEntry {
            title: Some(Title::from_name_lossy("Gentle rain")),
            ..UpdateCollectionEntry::default()
        },
        &fixture.library,
        player.as_ref(),
    )
    .await
    .unwrap();

    player.shut_down().await;

    let snapshot = fixture.snapshot();
    let renamed = titled(&snapshot, "Weather").unwrap();
    assert_eq!(renamed.builtin_title, None);
    assert_eq!(renamed.entries[0].builtin_title, None);
    assert_eq!(renamed.entries[0].title.as_str(), "Gentle rain");
}
