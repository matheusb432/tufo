//! Importing a Kenku FM library through the typed client, over a real local endpoint.

mod support;

use std::path::Path;

use serde_json::json;
use support::{TestServer, write_wav};
use tufo_client::{ClientError, Code};
use tufo_infra::kenku::{
    PLAYBACK_STATE_KEY, PLAYER_STATE_KEY, SETTINGS_STATE_KEY, write_file_origin_items,
};
use tufo_models::{
    library::{BackgroundPreset, CollectionKind, Volume},
    path::FolderPath,
    playback::queue::RepeatMode,
};
use tufo_wire::library::{Background, CollectionSource, ImportKenkuLibrary};

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

#[tokio::test]
async fn a_kenku_library_arrives_once_with_its_folders_sounds_settings_and_token() {
    let server = TestServer::start().await.unwrap();
    let client = server.client().unwrap();
    let music = tempfile::tempdir().unwrap();
    write_wav(&music.path().join("combat/a.wav"), 440.0).unwrap();
    write_wav(&music.path().join("combat/b.wav"), 550.0).unwrap();
    write_wav(&music.path().join("sfx/door.wav"), 660.0).unwrap();
    let kenku = tempfile::tempdir().unwrap();
    write_kenku_folder(kenku.path(), music.path()).unwrap();
    let request = ImportKenkuLibrary {
        kenku_folder: Some(FolderPath::try_new(kenku.path().to_path_buf()).unwrap()),
    };

    let first = client.import_kenku_library(&request).await.unwrap();
    let again = client.import_kenku_library(&request).await.unwrap();
    let mut snapshots = client.watch_library().await.unwrap();
    let library = snapshots.next().await.unwrap().unwrap();
    let mut players = client.watch_player().await.unwrap();
    let player = players.next().await.unwrap().unwrap();

    assert_eq!((first.collections_imported, first.files_imported), (2, 3));
    assert_eq!(first.unreadable_files.len(), 1);
    assert_eq!(first.unsupported_sources, ["https://example.com/radio.mp3"]);
    assert!(first.discord_bot_token_imported);
    assert_eq!(again.collections_imported, 0);
    assert_eq!(again.skipped_collections.len(), 2);
    assert!(!again.discord_bot_token_imported);
    assert!(server.discord_credentials_path().exists());

    let playlist = &library.collections[0];
    assert_eq!(
        (playlist.kind, playlist.title.as_str()),
        (CollectionKind::Playlist, "Combat Music")
    );
    assert!(matches!(
        &playlist.source,
        CollectionSource::Linked { folder, .. } if folder.as_ref() == &music.path().join("combat")
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
        })
        .await;

    assert!(matches!(result, Err(ClientError::Status(status)) if status.code() == Code::NotFound));
    server.stop().await.unwrap();
}
