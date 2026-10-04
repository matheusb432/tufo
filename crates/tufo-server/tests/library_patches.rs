//! Atomic nested library updates and compatibility through a real local endpoint.

mod support;

use std::time::Duration;

use support::{TEST_TIMEOUT, TestServer, write_wav};
use tufo_client::{Code, TufoClient};
use tufo_models::{
    library::{
        BackgroundPreset, CollectionId, CollectionKind, EntryOrder, EntryOrderKey, FadeDuration,
        SoundIcon, SoundSettings, Title, Volume,
    },
    path::LocalPath,
};
use tufo_wire::{
    library::{
        AddFilesToCollection, Background, CollectionView, CreateCollection, EntryOrderPatch,
        LibrarySnapshot, SoundSettingsPatch, UpdateCollection, UpdateCollectionEntry,
    },
    pb::{self, library_service_client::LibraryServiceClient},
};

struct Fixture {
    server: TestServer,
    client: TufoClient,
    sounds: tempfile::TempDir,
    collection: CollectionView,
}

async fn fixture() -> anyhow::Result<Fixture> {
    let server = TestServer::start().await?;
    let client = server.client()?;
    let sounds = tempfile::tempdir()?;
    let path = sounds.path().join("rain.wav");
    write_wav(&path, 440.0)?;
    let id = client
        .create_collection(&CreateCollection {
            kind: CollectionKind::Soundboard,
            title: Title::try_new("Weather".to_owned())?,
            background: Background::Preset(BackgroundPreset::Storm),
        })
        .await?;
    client
        .add_files_to_collection(&AddFilesToCollection {
            collection_id: id,
            paths: vec![LocalPath::try_new(path)?],
        })
        .await?;
    let snapshot = latest_snapshot(&client).await?;
    let entry_id = collection(&snapshot, id)?
        .entries
        .first()
        .ok_or_else(|| anyhow::anyhow!("the sound was not added"))?
        .id;
    client
        .update_collection(
            id,
            &UpdateCollection {
                order: Some(EntryOrder {
                    key: EntryOrderKey::DateAdded,
                    descending: false,
                }),
                ..UpdateCollection::default()
            },
        )
        .await?;
    client
        .update_collection_entry(
            entry_id,
            &UpdateCollectionEntry {
                title: Some(Title::try_new("Rain".to_owned())?),
                icon: Some(SoundIcon::CloudRain),
                sound: Some(SoundSettings {
                    looping: true,
                    volume: Volume::try_new(0.8)?,
                    fade_in: FadeDuration::try_new(Duration::from_millis(321))?,
                    fade_out: FadeDuration::try_new(Duration::from_millis(654))?,
                }),
                ..UpdateCollectionEntry::default()
            },
        )
        .await?;
    let snapshot = latest_snapshot(&client).await?;
    Ok(Fixture {
        server,
        client,
        sounds,
        collection: collection(&snapshot, id)?.clone(),
    })
}

async fn latest_snapshot(client: &TufoClient) -> anyhow::Result<LibrarySnapshot> {
    let mut snapshots = tokio::time::timeout(TEST_TIMEOUT, client.watch_library()).await??;
    Ok(tokio::time::timeout(TEST_TIMEOUT, snapshots.next())
        .await?
        .ok_or_else(|| anyhow::anyhow!("the library stream ended"))??)
}

fn collection(snapshot: &LibrarySnapshot, id: CollectionId) -> anyhow::Result<&CollectionView> {
    snapshot
        .collections
        .iter()
        .find(|collection| collection.id == id)
        .ok_or_else(|| anyhow::anyhow!("the collection is missing"))
}

#[tokio::test]
async fn simultaneous_nested_patches_preserve_each_other_and_unrelated_fields() -> anyhow::Result<()>
{
    let Fixture {
        server,
        client,
        sounds: _sounds,
        collection: before,
    } = fixture().await?;
    let id = before.id;
    let entry_id = before.entries[0].id;
    let volume = Volume::try_new(0.3)?;
    let key_update = UpdateCollection {
        order_patch: Some(EntryOrderPatch {
            key: Some(EntryOrderKey::Duration),
            ..EntryOrderPatch::default()
        }),
        ..UpdateCollection::default()
    };
    let direction_update = UpdateCollection {
        order_patch: Some(EntryOrderPatch {
            descending: Some(true),
            ..EntryOrderPatch::default()
        }),
        ..UpdateCollection::default()
    };
    let volume_update = UpdateCollectionEntry {
        sound_patch: Some(SoundSettingsPatch {
            volume: Some(volume),
            ..SoundSettingsPatch::default()
        }),
        ..UpdateCollectionEntry::default()
    };
    let looping_update = UpdateCollectionEntry {
        sound_patch: Some(SoundSettingsPatch {
            looping: Some(false),
            ..SoundSettingsPatch::default()
        }),
        ..UpdateCollectionEntry::default()
    };

    tokio::try_join!(
        client.update_collection(id, &key_update),
        client.update_collection(id, &direction_update),
        client.update_collection_entry(entry_id, &volume_update),
        client.update_collection_entry(entry_id, &looping_update),
    )?;

    let after = latest_snapshot(&client).await?;
    let mut expected = before;
    expected.order = EntryOrder {
        key: EntryOrderKey::Duration,
        descending: true,
    };
    expected.entries[0].sound.volume = volume;
    expected.entries[0].sound.looping = false;
    assert_eq!(collection(&after, id)?, &expected);
    server.stop().await
}

#[tokio::test]
async fn full_order_and_sound_replacements_remain_compatible() -> anyhow::Result<()> {
    let Fixture {
        server,
        client,
        sounds: _sounds,
        collection: mut expected,
    } = fixture().await?;
    let id = expected.id;
    let entry_id = expected.entries[0].id;
    let order = EntryOrder {
        key: EntryOrderKey::Title,
        descending: true,
    };
    let sound = SoundSettings {
        looping: false,
        volume: Volume::try_new(0.1)?,
        fade_in: FadeDuration::try_new(Duration::from_millis(222))?,
        fade_out: FadeDuration::try_new(Duration::from_millis(444))?,
    };
    let collection_title = Title::try_new("Session effects".to_owned())?;
    let entry_title = Title::try_new("Distant rain".to_owned())?;

    client
        .update_collection(
            id,
            &UpdateCollection {
                title: Some(collection_title.clone()),
                background: Some(Background::Gradient),
                order: Some(order),
                ..UpdateCollection::default()
            },
        )
        .await?;
    client
        .update_collection_entry(
            entry_id,
            &UpdateCollectionEntry {
                title: Some(entry_title.clone()),
                sound: Some(sound),
                icon: Some(SoundIcon::Bell),
                ..UpdateCollectionEntry::default()
            },
        )
        .await?;

    let after = latest_snapshot(&client).await?;
    expected.title = collection_title;
    expected.background = Background::Gradient;
    expected.order = order;
    expected.entries[0].title = entry_title;
    expected.entries[0].sound = sound;
    expected.entries[0].icon = SoundIcon::Bell;
    assert_eq!(collection(&after, id)?, &expected);
    server.stop().await
}

#[tokio::test]
async fn conflicting_raw_requests_leave_the_library_unchanged() -> anyhow::Result<()> {
    let Fixture {
        server,
        client,
        sounds: _sounds,
        collection: imported,
    } = fixture().await?;
    let id = imported.id;
    let entry_id = imported.entries[0].id;
    let before = latest_snapshot(&client).await?;
    let mut raw = LibraryServiceClient::new(server.channel().await?);
    let raw_collection_result = tokio::time::timeout(
        TEST_TIMEOUT,
        raw.update_collection(pb::UpdateCollectionRequest {
            collection_id: id.to_string(),
            title: Some("Rejected raw collection title".to_owned()),
            order: Some(pb::EntryOrder {
                key: pb::EntryOrderKey::Title.into(),
                descending: true,
            }),
            order_patch: Some(pb::EntryOrderPatch {
                key: Some(pb::EntryOrderKey::Duration.into()),
                descending: None,
            }),
            ..pb::UpdateCollectionRequest::default()
        }),
    )
    .await?;
    assert!(
        matches!(&raw_collection_result, Err(status)
        if status.code() == Code::InvalidArgument),
        "{raw_collection_result:?}"
    );
    assert_eq!(latest_snapshot(&client).await?, before);

    let raw_entry_result = tokio::time::timeout(
        TEST_TIMEOUT,
        raw.update_collection_entry(pb::UpdateCollectionEntryRequest {
            collection_entry_id: entry_id.to_string(),
            title: Some("Rejected raw entry title".to_owned()),
            sound: Some(pb::SoundSettings {
                looping: false,
                volume: 0.15,
                fade_in: Some(prost_types::Duration::default()),
                fade_out: Some(prost_types::Duration::default()),
            }),
            icon: Some(pb::SoundIcon::Bell.into()),
            sound_patch: Some(pb::SoundSettingsPatch {
                looping: Some(true),
                ..pb::SoundSettingsPatch::default()
            }),
        }),
    )
    .await?;
    assert!(
        matches!(&raw_entry_result, Err(status)
        if status.code() == Code::InvalidArgument),
        "{raw_entry_result:?}"
    );
    assert_eq!(latest_snapshot(&client).await?, before);
    server.stop().await
}
