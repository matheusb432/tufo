//! The library RPCs through the typed client, over a real local endpoint.

mod support;

use support::{TEST_TIMEOUT, TestServer, write_wav};
use tufo_application::library::{
    add_files_to_collection::AddFilesToCollectionError, delete_collection::DeleteCollectionError,
};
use tufo_client::{ClientError, Code, error};
use tufo_models::{
    library::{CollectionId, CollectionKind, SoundIcon},
    path::LocalPath,
};
use tufo_wire::library::{
    AddFilesToCollection, CollectionSource, ImportPaths, UpdateCollectionEntry,
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

    assert!(empty.collections.is_empty());
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

    assert!(matches!(into_linked, Err(ClientError::Status(status))
            if status.code() == Code::FailedPrecondition
            && error::reason(&status).as_deref() == Some(AddFilesToCollectionError::LINKED)));
    assert!(matches!(unknown, Err(ClientError::Status(status))
        if status.code() == Code::NotFound
        && error::reason(&status).as_deref() == Some(DeleteCollectionError::NOT_FOUND)
        && status.message() == "the collection does not exist"));
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
