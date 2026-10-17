//! The library RPCs through the typed client, over a real local endpoint.

mod support;

use support::{TEST_TIMEOUT, TestServer, write_wav};
use tufo_application::library::{
    add_files_to_collection::AddFilesToCollectionError,
    add_songs_to_playlist::AddSongsToPlaylistError,
    arrange_collection_entries::ArrangeCollectionEntriesError,
    delete_collection::DeleteCollectionError, delete_song::DeleteSongError,
};
use tufo_client::{ClientError, Code, LibrarySnapshots, error};
use tufo_models::{
    library::{
        AudioFileId, CollectionEntryId, CollectionId, CollectionKind, EntryOrderKey, SoundIcon,
        Title,
    },
    path::LocalPath,
};
use tufo_wire::library::{
    AddFilesToCollection, AddSongsToPlaylist, ArrangeCollectionEntries, Background,
    CollectionSource, CollectionView, CreateCollection, EntryArrangement, ImportPaths,
    LibrarySnapshot, SongSource, UpdateCollectionEntry,
};

#[tokio::test]
async fn an_imported_folder_streams_as_a_linked_playlist() {
    let server = TestServer::start().await.unwrap();
    let client = server.client().unwrap();
    let music = tempfile::tempdir().unwrap();
    write_wav(&music.path().join("rpg/boss/dragon.wav"), 440.0).unwrap();
    let mut snapshots = client.watch_library().await.unwrap();
    let empty = snapshots.next().await.unwrap().unwrap();

    let report = client
        .import_paths(&ImportPaths {
            kind: CollectionKind::Playlist,
            paths: vec![LocalPath::try_new(music.path().join("rpg")).unwrap()],
        })
        .await
        .unwrap();
    let imported = tokio::time::timeout(TEST_TIMEOUT, snapshots.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    assert_eq!(
        empty.collections,
        Vec::<tufo_wire::library::CollectionView>::new()
    );
    assert_eq!((report.collections_created, report.files_added), (1, 1));
    assert!(imported.revision > empty.revision);
    let boss = &imported.collections[0];
    assert_eq!(boss.title.as_str(), "boss");
    assert!(matches!(
        boss.source,
        CollectionSource::Linked { missing: false, .. }
    ));
    assert_eq!(boss.entries[0].title.as_str(), "dragon");
    server.stop().await.unwrap();
}

#[tokio::test]
async fn a_followed_folders_songs_join_a_new_playlist() {
    let server = TestServer::start().await.unwrap();
    let client = server.client().unwrap();
    let music = tempfile::tempdir().unwrap();
    write_wav(&music.path().join("rpg/boss/dragon.wav"), 440.0).unwrap();
    client
        .import_paths(&ImportPaths {
            kind: CollectionKind::Playlist,
            paths: vec![LocalPath::try_new(music.path().join("rpg")).unwrap()],
        })
        .await
        .unwrap();
    let session = client
        .create_collection(&CreateCollection {
            kind: CollectionKind::Playlist,
            title: Title::try_new("Session 12".to_owned()).unwrap(),
            background: Background::Gradient,
        })
        .await
        .unwrap();
    let mut snapshots = client.watch_library().await.unwrap();
    let before = snapshots.next().await.unwrap().unwrap();
    let dragon = &before.songs[0];

    let added = client
        .add_songs_to_playlist(&AddSongsToPlaylist {
            playlist_id: session,
            audio_file_ids: vec![dragon.audio_file_id],
        })
        .await
        .unwrap();
    let after = tokio::time::timeout(TEST_TIMEOUT, snapshots.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    assert_eq!(dragon.title.as_str(), "dragon");
    assert_eq!(
        dragon.source,
        SongSource::Folder(before.library_folders[0].id)
    );
    assert_eq!(added, 1);
    let listed = after
        .collections
        .iter()
        .find(|collection| collection.id == session)
        .unwrap();
    assert_eq!(listed.entries[0].audio_file_id, dragon.audio_file_id);
    server.stop().await.unwrap();
}

#[tokio::test]
async fn refusals_carry_their_status_codes() {
    let server = TestServer::start().await.unwrap();
    let client = server.client().unwrap();
    let music = tempfile::tempdir().unwrap();
    write_wav(&music.path().join("sfx/door.wav"), 440.0).unwrap();
    client
        .import_paths(&ImportPaths {
            kind: CollectionKind::Soundboard,
            paths: vec![LocalPath::try_new(music.path().join("sfx")).unwrap()],
        })
        .await
        .unwrap();
    let mut snapshots = client.watch_library().await.unwrap();
    let linked = snapshots.next().await.unwrap().unwrap().collections[0].id;

    let into_linked = client
        .add_files_to_collection(&AddFilesToCollection {
            collection_id: linked,
            paths: vec![LocalPath::try_new(music.path().join("sfx/door.wav")).unwrap()],
        })
        .await;
    let unknown = client
        .delete_collection(CollectionId::from_uuid(uuid::Uuid::from_u128(1)))
        .await;
    let into_soundboard = client
        .add_songs_to_playlist(&AddSongsToPlaylist {
            playlist_id: linked,
            audio_file_ids: vec![AudioFileId::from_uuid(uuid::Uuid::from_u128(2))],
        })
        .await;
    let unknown_song = client
        .delete_song(AudioFileId::from_uuid(uuid::Uuid::from_u128(2)))
        .await;

    assert!(matches!(into_linked, Err(ClientError::Status(status))
            if status.code() == Code::FailedPrecondition
            && error::reason(&status).as_deref() == Some(AddFilesToCollectionError::LINKED)));
    assert!(matches!(unknown, Err(ClientError::Status(status))
        if status.code() == Code::NotFound
        && error::reason(&status).as_deref() == Some(DeleteCollectionError::NOT_FOUND)
        && status.message() == "the collection does not exist"));
    assert!(matches!(into_soundboard, Err(ClientError::Status(status))
        if status.code() == Code::NotFound
        && error::reason(&status).as_deref() == Some(AddSongsToPlaylistError::PLAYLIST_NOT_FOUND)));
    assert!(matches!(unknown_song, Err(ClientError::Status(status))
        if status.code() == Code::NotFound
        && error::reason(&status).as_deref() == Some(DeleteSongError::NOT_FOUND)));
    server.stop().await.unwrap();
}

#[tokio::test]
async fn a_sound_keeps_the_icon_chosen_for_it() {
    let server = TestServer::start().await.unwrap();
    let client = server.client().unwrap();
    let sounds = tempfile::tempdir().unwrap();
    write_wav(&sounds.path().join("weather/rain.wav"), 440.0).unwrap();
    client
        .import_paths(&ImportPaths {
            kind: CollectionKind::Soundboard,
            paths: vec![LocalPath::try_new(sounds.path().join("weather")).unwrap()],
        })
        .await
        .unwrap();
    let mut snapshots = client.watch_library().await.unwrap();
    let imported = snapshots.next().await.unwrap().unwrap();
    let rain = &imported.collections[0].entries[0];

    client
        .update_collection_entry(
            rain.id,
            &UpdateCollectionEntry {
                icon: Some(SoundIcon::CloudRain),
                ..UpdateCollectionEntry::default()
            },
        )
        .await
        .unwrap();
    let updated = tokio::time::timeout(TEST_TIMEOUT, snapshots.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    assert_eq!(rain.icon, SoundIcon::AudioLines);
    let updated_rain = &updated.collections[0].entries[0];
    assert_eq!(updated_rain.icon, SoundIcon::CloudRain);
    assert_eq!(updated_rain.sound, rain.sound);
    server.stop().await.unwrap();
}

#[tokio::test]
async fn a_collection_keeps_its_custom_order_and_new_sounds_join_its_end() {
    let server = TestServer::start().await.unwrap();
    let client = server.client().unwrap();
    let sounds = tempfile::tempdir().unwrap();
    for name in ["ambush", "chase", "dragon", "tavern"] {
        write_wav(&sounds.path().join(format!("sfx/{name}.wav")), 440.0).unwrap();
    }
    client
        .import_paths(&ImportPaths {
            kind: CollectionKind::Soundboard,
            paths: vec![LocalPath::try_new(sounds.path().join("sfx")).unwrap()],
        })
        .await
        .unwrap();
    let mut snapshots = client.watch_library().await.unwrap();
    let imported = snapshots.next().await.unwrap().unwrap();
    let collection_id = imported.collections[0].id;
    let entry_id = |title: &str| entry_id_by_title(&imported.collections[0], title).unwrap();
    let arrange = |titles: &[&str]| ArrangeCollectionEntries {
        collection_id,
        entry_ids: EntryArrangement::try_new(titles.iter().map(|title| entry_id(title)).collect())
            .unwrap(),
    };

    client
        .arrange_collection_entries(&arrange(&["dragon", "tavern", "ambush", "chase"]))
        .await
        .unwrap();
    let arranged = next_snapshot_where(&mut snapshots, |snapshot| {
        snapshot.collections[0].order.key == EntryOrderKey::Custom
    })
    .await
    .unwrap();
    write_wav(&sounds.path().join("sfx/bandits.wav"), 440.0).unwrap();
    client.rescan_library().await.unwrap();
    next_snapshot_where(&mut snapshots, |snapshot| {
        snapshot.collections[0].entries.len() == 5
    })
    .await
    .unwrap();
    client
        .arrange_collection_entries(&arrange(&["chase"]))
        .await
        .unwrap();
    let rearranged = next_snapshot_where(&mut snapshots, |snapshot| {
        snapshot.collections[0].entries[0].title.as_str() == "chase"
    })
    .await
    .unwrap();
    let unknown_entry = client
        .arrange_collection_entries(&ArrangeCollectionEntries {
            collection_id,
            entry_ids: EntryArrangement::try_new(vec![CollectionEntryId::from_uuid(
                uuid::Uuid::from_u128(1),
            )])
            .unwrap(),
        })
        .await;
    let unknown_collection = client
        .arrange_collection_entries(&ArrangeCollectionEntries {
            collection_id: CollectionId::from_uuid(uuid::Uuid::from_u128(1)),
            entry_ids: EntryArrangement::default(),
        })
        .await;

    assert_eq!(
        titles(&arranged.collections[0]),
        ["dragon", "tavern", "ambush", "chase"]
    );
    assert_eq!(
        titles(&rearranged.collections[0]),
        ["chase", "dragon", "tavern", "ambush", "bandits"]
    );
    assert!(matches!(unknown_entry, Err(ClientError::Status(status))
        if status.code() == Code::NotFound
        && error::reason(&status).as_deref() == Some(ArrangeCollectionEntriesError::ENTRY_NOT_FOUND)));
    assert!(
        matches!(unknown_collection, Err(ClientError::Status(status))
        if status.code() == Code::NotFound
        && error::reason(&status).as_deref() == Some(ArrangeCollectionEntriesError::NOT_FOUND))
    );
    server.stop().await.unwrap();
}

fn entry_id_by_title(collection: &CollectionView, title: &str) -> Option<CollectionEntryId> {
    collection
        .entries
        .iter()
        .find(|entry| entry.title.as_str() == title)
        .map(|entry| entry.id)
}

fn titles(collection: &CollectionView) -> Vec<&str> {
    collection
        .entries
        .iter()
        .map(|entry| entry.title.as_str())
        .collect()
}

async fn next_snapshot_where(
    snapshots: &mut LibrarySnapshots,
    wanted: impl Fn(&LibrarySnapshot) -> bool,
) -> anyhow::Result<LibrarySnapshot> {
    loop {
        let snapshot = tokio::time::timeout(TEST_TIMEOUT, snapshots.next())
            .await?
            .ok_or_else(|| anyhow::anyhow!("the library stream ended"))??;
        if wanted(&snapshot) {
            return Ok(snapshot);
        }
    }
}
