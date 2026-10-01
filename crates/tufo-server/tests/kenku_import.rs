//! Importing a Kenku FM library through the typed client, over a real local endpoint.

mod support;

use std::path::Path;

use anyhow::Context as _;
use serde_json::json;
use support::{TestServer, write_wav};
use tufo_client::TufoClient;
use tufo_infra::kenku::{
    PLAYBACK_STATE_KEY, PLAYER_STATE_KEY, SETTINGS_STATE_KEY, write_file_origin_items,
};
use tufo_models::{
    kenku_import::{KenkuImportId, KenkuImportState},
    library::{BackgroundPreset, CollectionKind, Volume},
    path::FolderPath,
    playback::queue::RepeatMode,
};
use tufo_wire::library::{
    Background, CollectionSource, ImportKenkuLibrary, KenkuCollectionChoice, KenkuImportStatus,
    LibrarySnapshot,
};

/// A file's URL the way Kenku FM writes it.
fn kenku_url(path: &Path) -> String {
    let text = path
        .to_string_lossy()
        .replace('\\', "/")
        .replace(' ', "%20");
    format!("file://{text}")
}

/// `redux-persist` stores each value as JSON text.
fn persisted(root: &serde_json::Value) -> String {
    let encoded = root
        .as_object()
        .into_iter()
        .flatten()
        .map(|(key, value)| (key.clone(), json!(value.to_string())))
        .collect::<serde_json::Map<_, _>>();
    serde_json::Value::Object(encoded).to_string()
}

/// A one-folder playlist, and a soundboard with a missing file and a web address.
fn write_kenku_folder(kenku: &Path, music: &Path) -> anyhow::Result<()> {
    let player = persisted(&json!({
        "playlists": {
            "playlists": {
                "byId": {"p": {"id": "p", "title": "Combat Music", "background": "war", "tracks": ["a", "b"]}},
                "allIds": ["p"]
            },
            "tracks": {
                "a": {"id": "a", "title": "A", "url": kenku_url(&music.join("combat/a.wav"))},
                "b": {"id": "b", "title": "B", "url": kenku_url(&music.join("combat/b.wav"))}
            }
        },
        "soundboards": {
            "soundboards": {
                "byId": {"s": {"id": "s", "title": "Doors", "background": "veins", "sounds": ["door", "gone", "web"]}},
                "allIds": ["s"]
            },
            "sounds": {
                "door": {"id": "door", "title": "Front Door", "url": kenku_url(&music.join("sfx/door.wav")), "loop": true, "volume": 0.25, "fadeIn": 100, "fadeOut": 100},
                "gone": {"id": "gone", "title": "Gone", "url": kenku_url(&music.join("sfx/gone.wav")), "loop": false, "volume": 1, "fadeIn": 100, "fadeOut": 100},
                "web": {"id": "web", "title": "Radio", "url": "https://example.com/radio.mp3", "loop": false, "volume": 1, "fadeIn": 100, "fadeOut": 100}
            }
        },
        "_persist": {"version": 1, "rehydrated": true}
    }));
    let playback =
        persisted(&json!({"volume": 0.5, "muted": false, "shuffle": true, "repeat": "track"}));
    let settings = persisted(&json!({"settings": {"discordToken": "kenku.bot.token"}}));
    write_file_origin_items(
        kenku,
        &[
            (PLAYER_STATE_KEY, &player),
            (PLAYBACK_STATE_KEY, &playback),
            (SETTINGS_STATE_KEY, &settings),
        ],
    )
}

struct KenkuFixture {
    music: tempfile::TempDir,
    data: tempfile::TempDir,
    request: ImportKenkuLibrary,
}

fn kenku_fixture() -> anyhow::Result<KenkuFixture> {
    let music = tempfile::tempdir()?;
    for (path, tone) in [
        ("combat/a.wav", 440.0),
        ("combat/b.wav", 550.0),
        ("sfx/door.wav", 660.0),
    ] {
        write_wav(&music.path().join(path), tone)?;
    }
    let data = tempfile::tempdir()?;
    write_kenku_folder(data.path(), music.path())?;
    let request = ImportKenkuLibrary {
        kenku_folder: Some(FolderPath::try_new(data.path().to_path_buf())?),
        ..ImportKenkuLibrary::default()
    };
    Ok(KenkuFixture {
        music,
        data,
        request,
    })
}

#[tokio::test]
async fn a_kenku_library_imports_its_folders_sounds_settings_and_token() {
    let server = TestServer::start().await.unwrap();
    let client = server.client().unwrap();
    let fixture = kenku_fixture().unwrap();

    let first_id = client.import_kenku_library(&fixture.request).await.unwrap();
    let first = wait_import(&client, first_id).await.unwrap().report;
    let again_id = client.import_kenku_library(&fixture.request).await.unwrap();
    let conflicts = wait_import(&client, again_id).await.unwrap();
    assert_ne!(first_id, again_id);
    assert_eq!(conflicts.state, KenkuImportState::AwaitingChoices);
    assert_eq!(conflicts.conflicts.len(), 2);
    let mut snapshots = client.watch_library().await.unwrap();
    let library = snapshots.next().await.unwrap().unwrap();
    let mut players = client.watch_player().await.unwrap();
    let player = players.next().await.unwrap().unwrap();

    assert_eq!((first.collections_imported, first.files_imported), (2, 3));
    assert_eq!(first.unreadable_files.len(), 1);
    assert_eq!(first.unsupported_sources, ["https://example.com/radio.mp3"]);
    assert!(first.discord_bot_token_imported);
    assert!(server.discord_credentials_path().exists());

    let playlist = &library.collections[0];
    assert_eq!(
        (playlist.kind, playlist.title.as_str()),
        (CollectionKind::Playlist, "Combat Music")
    );
    assert!(matches!(
        &playlist.source,
        CollectionSource::Linked { folder, .. } if folder.as_ref() == &std::fs::canonicalize(fixture.music.path().join("combat")).unwrap()
    ));
    assert_eq!(
        playlist.background,
        Background::Preset(BackgroundPreset::Battle)
    );
    assert_eq!(playlist.entries.len(), 2);

    let soundboard = &library.collections[1];
    assert_eq!(soundboard.source, CollectionSource::Manual);
    let door = &soundboard.entries[0];
    assert_eq!(door.title.as_str(), "Front Door");
    assert!(door.sound.looping);
    assert_eq!(door.sound.volume, Volume::try_new(0.25).unwrap());

    assert_eq!(player.volume, Volume::try_new(0.5).unwrap());
    assert!(player.modes.shuffle);
    assert_eq!(player.modes.repeat, RepeatMode::Track);
    server.stop().await.unwrap();
}

#[tokio::test]
async fn a_folder_without_a_kenku_library_is_not_found() {
    let server = TestServer::start().await.unwrap();
    let client = server.client().unwrap();
    let empty = tempfile::tempdir().unwrap();

    let result = client
        .import_kenku_library(&ImportKenkuLibrary {
            kenku_folder: Some(FolderPath::try_new(empty.path().to_path_buf()).unwrap()),
            ..ImportKenkuLibrary::default()
        })
        .await;

    let status = wait_import(&client, result.unwrap()).await.unwrap();
    assert_eq!(status.state, KenkuImportState::Failed);
    assert_eq!(status.error_reason.as_deref(), Some(tufo_application::library::import_kenku_library::kenku_data::ReadKenkuDataError::NOT_FOUND));
    server.stop().await.unwrap();
}

async fn wait_import(client: &TufoClient, id: KenkuImportId) -> anyhow::Result<KenkuImportStatus> {
    tokio::time::timeout(support::TEST_TIMEOUT, async {
        let mut updates = client.watch_kenku_import().await?;
        loop {
            if let Some(status) = updates.next().await.context("the import stream ended")??
                && status.id == id
                && !status.state.is_running()
            {
                return Ok(status);
            }
        }
    })
    .await?
}

async fn snapshot(client: &TufoClient) -> anyhow::Result<LibrarySnapshot> {
    Ok(client
        .watch_library()
        .await?
        .next()
        .await
        .context("the library stream ended")??)
}

#[tokio::test]
async fn unavailable_and_corrupt_files_retry_into_the_same_collections_after_a_restart() {
    let server = TestServer::start().await.unwrap();
    let client = server.client().unwrap();
    let fixture = kenku_fixture().unwrap();
    std::fs::write(fixture.music.path().join("combat/b.wav"), b"invalid audio").unwrap();
    let id = client.import_kenku_library(&fixture.request).await.unwrap();
    let first = wait_import(&client, id).await.unwrap();
    let before = snapshot(&client).await.unwrap();
    let doors = before
        .collections
        .iter()
        .find(|collection| collection.title.as_str() == "Doors")
        .unwrap()
        .id;
    assert_eq!(first.state, KenkuImportState::Succeeded);
    assert_eq!(first.report.files_imported, 2);
    assert_eq!(first.report.unreadable_files.len(), 2);
    assert_eq!(before.collections[0].entries.len(), 1);
    let server = server.restart().await.unwrap();
    let client = server.client().unwrap();
    assert_eq!(wait_import(&client, id).await.unwrap(), first);
    write_wav(&fixture.music.path().join("combat/b.wav"), 550.0).unwrap();
    write_wav(&fixture.music.path().join("sfx/gone.wav"), 770.0).unwrap();
    client
        .import_kenku_library(&ImportKenkuLibrary {
            import_id: Some(id),
            ..ImportKenkuLibrary::default()
        })
        .await
        .unwrap();
    let retried = wait_import(&client, id).await.unwrap();
    assert_eq!(retried.state, KenkuImportState::Succeeded);
    assert_eq!(retried.report.unreadable_files.len(), 0);
    assert_eq!(retried.report.files_imported, 4);
    let after = snapshot(&client).await.unwrap();
    assert_eq!(after.collections.len(), 2);
    assert_eq!(
        after
            .collections
            .iter()
            .find(|collection| collection.id == doors)
            .unwrap()
            .entries
            .len(),
        2
    );
    server.stop().await.unwrap();
}

#[tokio::test]
async fn each_conflict_can_keep_or_overwrite_and_stale_choices_are_reoffered() {
    use tufo_models::library::Title;
    use tufo_wire::library::{CreateCollection, UpdateCollection};
    let server = TestServer::start().await.unwrap();
    let client = server.client().unwrap();
    let fixture = kenku_fixture().unwrap();
    let mut existing = Vec::new();
    for (kind, title) in [
        (CollectionKind::Playlist, "Combat Music"),
        (CollectionKind::Soundboard, "Doors"),
    ] {
        existing.push(
            client
                .create_collection(&CreateCollection {
                    kind,
                    title: Title::try_new(title.to_owned()).unwrap(),
                    background: Background::Gradient,
                })
                .await
                .unwrap(),
        );
    }
    let id = client.import_kenku_library(&fixture.request).await.unwrap();
    let conflict = wait_import(&client, id).await.unwrap();
    assert_eq!(conflict.conflicts.len(), 2);
    let (server, client) = restart_before_choices(server, id).await.unwrap();
    assert!(
        snapshot(&client)
            .await
            .unwrap()
            .collections
            .iter()
            .all(|collection| collection.entries.is_empty())
    );
    let request = ImportKenkuLibrary {
        import_id: Some(id),
        choices: conflict
            .conflicts
            .iter()
            .map(|conflict| KenkuCollectionChoice {
                source_id: conflict.source_id.clone(),
                kind: conflict.kind,
                overwrite: conflict.kind == CollectionKind::Playlist,
            })
            .collect(),
        ..ImportKenkuLibrary::default()
    };
    client
        .update_collection(
            existing[0],
            &UpdateCollection {
                background: Some(Background::Preset(BackgroundPreset::Night)),
                ..UpdateCollection::default()
            },
        )
        .await
        .unwrap();
    client.import_kenku_library(&request).await.unwrap();
    let stale = wait_import(&client, id).await.unwrap();
    assert_eq!(stale.state, KenkuImportState::AwaitingChoices);
    assert_eq!(
        snapshot(&client).await.unwrap().collections[0].background,
        Background::Preset(BackgroundPreset::Night)
    );
    client.import_kenku_library(&request).await.unwrap();
    let result = wait_import(&client, id).await.unwrap();
    assert_eq!(result.state, KenkuImportState::Succeeded);
    assert_eq!(result.report.collections_imported, 1);
    assert_eq!(result.report.skipped_collections.len(), 1);
    let after = snapshot(&client).await.unwrap();
    assert_eq!(after.collections.len(), 2);
    let playlist = after
        .collections
        .iter()
        .find(|collection| collection.id == existing[0])
        .unwrap();
    assert_eq!(playlist.entries.len(), 2);
    assert_eq!(
        playlist.background,
        Background::Preset(BackgroundPreset::Battle)
    );
    let soundboard = after
        .collections
        .iter()
        .find(|collection| collection.id == existing[1])
        .unwrap();
    assert_eq!(soundboard.entries.len(), 0);
    assert_eq!(soundboard.background, Background::Gradient);
    server.stop().await.unwrap();
}

#[tokio::test]
async fn a_failed_collection_transaction_leaves_no_imported_roots_files_or_collections() {
    let server = TestServer::start().await.unwrap();
    let mut database = tufo_infra::database::open_database(&server.database_path())
        .await
        .unwrap();
    toasty::sql::statement("CREATE TRIGGER fail_import BEFORE INSERT ON collection_entries BEGIN SELECT RAISE(ABORT, 'injected failure'); END").exec(&mut database).await.unwrap();
    let client = server.client().unwrap();
    let fixture = kenku_fixture().unwrap();
    let id = client.import_kenku_library(&fixture.request).await.unwrap();
    assert_eq!(
        wait_import(&client, id).await.unwrap().state,
        KenkuImportState::Failed
    );
    let unchanged = snapshot(&client).await.unwrap();
    assert!(
        unchanged.collections.is_empty()
            && unchanged.songs.is_empty()
            && unchanged.library_folders.is_empty()
    );
    toasty::sql::statement("DROP TRIGGER fail_import")
        .exec(&mut database)
        .await
        .unwrap();
    client
        .import_kenku_library(&ImportKenkuLibrary {
            import_id: Some(id),
            ..ImportKenkuLibrary::default()
        })
        .await
        .unwrap();
    assert_eq!(
        wait_import(&client, id).await.unwrap().state,
        KenkuImportState::Succeeded
    );
    assert_eq!(snapshot(&client).await.unwrap().collections.len(), 2);
    server.stop().await.unwrap();
}

#[tokio::test]
async fn resolved_kenku_paths_reuse_the_folder_and_do_not_restore_deleted_descendants() {
    use tufo_models::path::LocalPath;
    use tufo_wire::library::ImportPaths;
    let server = TestServer::start().await.unwrap();
    let client = server.client().unwrap();
    let fixture = kenku_fixture().unwrap();
    write_wav(&fixture.music.path().join("combat/deleted/c.wav"), 600.0).unwrap();
    client
        .import_paths(&ImportPaths {
            kind: CollectionKind::Playlist,
            paths: vec![LocalPath::try_new(fixture.music.path().join("combat")).unwrap()],
        })
        .await
        .unwrap();
    let before = snapshot(&client).await.unwrap();
    let original = before
        .collections
        .iter()
        .find(|collection| collection.entries.len() == 2)
        .unwrap();
    let original_files = original
        .entries
        .iter()
        .map(|entry| entry.audio_file_id)
        .collect::<std::collections::HashSet<_>>();
    let deleted = before
        .collections
        .iter()
        .find(|collection| collection.entries.len() == 1)
        .unwrap()
        .id;
    client.delete_collection(deleted).await.unwrap();
    let source_music = fixture.music.path().to_path_buf();
    #[cfg(unix)]
    let source_music = {
        let alias = fixture.data.path().join("music-alias");
        std::os::unix::fs::symlink(source_music, &alias).unwrap();
        alias
    };
    write_kenku_folder(fixture.data.path(), &source_music).unwrap();
    let id = client.import_kenku_library(&fixture.request).await.unwrap();
    let conflicts = wait_import(&client, id).await.unwrap();
    assert_eq!(conflicts.conflicts.len(), 1);
    client
        .import_kenku_library(&ImportKenkuLibrary {
            import_id: Some(id),
            choices: vec![KenkuCollectionChoice {
                source_id: "p".to_owned(),
                kind: CollectionKind::Playlist,
                overwrite: true,
            }],
            ..ImportKenkuLibrary::default()
        })
        .await
        .unwrap();
    assert_eq!(
        wait_import(&client, id).await.unwrap().state,
        KenkuImportState::Succeeded
    );
    client.rescan_library().await.unwrap();
    let after = snapshot(&client).await.unwrap();
    assert_eq!(after.library_folders.len(), 1);
    assert_eq!(after.collections.len(), 2);
    assert!(
        after
            .collections
            .iter()
            .all(|collection| collection.id != deleted)
    );
    let playlist = after
        .collections
        .iter()
        .find(|collection| collection.id == original.id)
        .unwrap();
    assert_eq!(playlist.title.as_str(), "Combat Music");
    assert_eq!(
        playlist
            .entries
            .iter()
            .map(|entry| entry.audio_file_id)
            .collect::<std::collections::HashSet<_>>(),
        original_files
    );
    server.stop().await.unwrap();
}

async fn restart_before_choices(
    server: TestServer,
    id: KenkuImportId,
) -> anyhow::Result<(TestServer, TufoClient)> {
    // Simulate a crash after saving conflicts but before publishing AwaitingChoices.
    let mut database = tufo_infra::database::open_database(&server.database_path()).await?;
    tufo_models::kenku_import::KenkuImport::update_by_id(id)
        .state(KenkuImportState::Reading)
        .exec(&mut database)
        .await?;
    let server = server.restart().await?;
    let client = server.client()?;
    anyhow::ensure!(wait_import(&client, id).await?.state == KenkuImportState::Interrupted);
    client
        .import_kenku_library(&ImportKenkuLibrary {
            import_id: Some(id),
            ..ImportKenkuLibrary::default()
        })
        .await?;
    anyhow::ensure!(wait_import(&client, id).await?.state == KenkuImportState::AwaitingChoices);
    Ok((server, client))
}

#[tokio::test]
async fn legacy_file_aliases_keep_their_ids_when_kenku_imports_the_canonical_path() {
    use tufo_models::playback::AudioFilePath;
    let server = TestServer::start().await.unwrap();
    let client = server.client().unwrap();
    let fixture = kenku_fixture().unwrap();
    std::fs::hard_link(
        fixture.music.path().join("sfx/door.wav"),
        fixture.music.path().join("sfx/door-hardlink.wav"),
    )
    .unwrap();
    create_soundboard(
        &client,
        "Original sounds",
        vec![
            fixture.music.path().join("sfx/door.wav"),
            fixture.music.path().join("sfx/door-hardlink.wav"),
        ],
    )
    .await
    .unwrap();
    let before = snapshot(&client).await.unwrap();
    let original_file = before.collections[0]
        .entries
        .iter()
        .find(|entry| entry.path.as_ref().ends_with("door.wav"))
        .unwrap()
        .audio_file_id;
    #[cfg(windows)]
    let alias = fixture.music.path().to_path_buf();
    #[cfg(unix)]
    let alias = {
        let alias = fixture.data.path().join("legacy-music");
        std::os::unix::fs::symlink(fixture.music.path(), &alias).unwrap();
        alias
    };
    let mut database = tufo_infra::database::open_database(&server.database_path())
        .await
        .unwrap();
    let update = tufo_models::library::AudioFile::update_by_id(original_file)
        .file_path(AudioFilePath::try_new(alias.join("sfx/door.wav")).unwrap());
    #[cfg(windows)]
    let update = update.file_identity(
        tufo_models::library::FileIdentity::try_new("replaced-legacy-file".to_owned()).unwrap(),
    );
    update.exec(&mut database).await.unwrap();
    let id = client.import_kenku_library(&fixture.request).await.unwrap();
    assert_eq!(
        wait_import(&client, id).await.unwrap().state,
        KenkuImportState::Succeeded
    );
    let after = snapshot(&client).await.unwrap();
    let doors = after
        .collections
        .iter()
        .find(|collection| collection.title.as_str() == "Doors")
        .unwrap();
    assert_eq!(doors.entries[0].audio_file_id, original_file);
    let files = tufo_models::library::AudioFile::all()
        .exec(&mut database)
        .await
        .unwrap();
    assert_eq!(files.len(), 4);
    assert_eq!(
        files
            .iter()
            .find(|file| file.id == original_file)
            .unwrap()
            .file_path
            .as_ref(),
        &std::fs::canonicalize(fixture.music.path().join("sfx/door.wav")).unwrap()
    );
    server.stop().await.unwrap();
}

#[tokio::test]
async fn changed_entry_titles_reoffer_choices_and_overwrites_remove_unkept_sound_files() {
    use tufo_models::library::{AudioFile, Title};
    use tufo_wire::library::UpdateCollectionEntry;
    let server = TestServer::start().await.unwrap();
    let client = server.client().unwrap();
    let fixture = kenku_fixture().unwrap();
    write_wav(&fixture.music.path().join("old/unused.wav"), 770.0).unwrap();
    create_soundboard(
        &client,
        "Doors",
        vec![
            fixture.music.path().join("sfx/door.wav"),
            fixture.music.path().join("old/unused.wav"),
        ],
    )
    .await
    .unwrap();
    let before = snapshot(&client).await.unwrap();
    let original = &before.collections[0];
    let removed_file = original
        .entries
        .iter()
        .find(|entry| entry.path.as_ref().ends_with("unused.wav"))
        .unwrap()
        .audio_file_id;
    let edited = original
        .entries
        .iter()
        .find(|entry| entry.path.as_ref().ends_with("door.wav"))
        .unwrap();
    let id = client.import_kenku_library(&fixture.request).await.unwrap();
    let conflict = wait_import(&client, id).await.unwrap();
    assert_eq!(conflict.state, KenkuImportState::AwaitingChoices);
    client
        .update_collection_entry(
            edited.id,
            &UpdateCollectionEntry {
                title: Some(Title::try_new("User renamed this sound".to_owned()).unwrap()),
                ..UpdateCollectionEntry::default()
            },
        )
        .await
        .unwrap();
    let request = ImportKenkuLibrary {
        import_id: Some(id),
        choices: vec![KenkuCollectionChoice {
            source_id: "s".to_owned(),
            kind: CollectionKind::Soundboard,
            overwrite: true,
        }],
        ..ImportKenkuLibrary::default()
    };
    client.import_kenku_library(&request).await.unwrap();
    let reoffered = wait_import(&client, id).await.unwrap();
    assert_eq!(reoffered.state, KenkuImportState::AwaitingChoices);
    assert_eq!(reoffered.conflicts, conflict.conflicts);
    client.import_kenku_library(&request).await.unwrap();
    assert_eq!(
        wait_import(&client, id).await.unwrap().state,
        KenkuImportState::Succeeded
    );
    let after = snapshot(&client).await.unwrap();
    let overwritten = after
        .collections
        .iter()
        .find(|collection| collection.id == original.id)
        .unwrap();
    assert_eq!(overwritten.entries.len(), 1);
    assert_eq!(overwritten.entries[0].audio_file_id, edited.audio_file_id);
    assert_eq!(overwritten.entries[0].title.as_str(), "Front Door");
    let mut database = tufo_infra::database::open_database(&server.database_path())
        .await
        .unwrap();
    assert!(
        AudioFile::filter_by_id(removed_file)
            .first()
            .exec(&mut database)
            .await
            .unwrap()
            .is_none()
    );
    server.stop().await.unwrap();
}

async fn create_soundboard(
    client: &TufoClient,
    title: &str,
    paths: Vec<std::path::PathBuf>,
) -> anyhow::Result<()> {
    use tufo_models::{library::Title, path::LocalPath};
    use tufo_wire::library::{AddFilesToCollection, CreateCollection};
    let collection_id = client
        .create_collection(&CreateCollection {
            kind: CollectionKind::Soundboard,
            title: Title::try_new(title.to_owned())?,
            background: Background::Gradient,
        })
        .await?;
    client
        .add_files_to_collection(&AddFilesToCollection {
            collection_id,
            paths: paths
                .into_iter()
                .map(LocalPath::try_new)
                .collect::<Result<_, _>>()?,
        })
        .await?;
    Ok(())
}
