//! The player RPCs through the typed client, over a real local endpoint.

use tonic::Code;
use tufo_client::ClientError;
use tufo_models::{
    discord::{GuildId, VoiceChannelId, VoiceChannelTarget},
    library::{CollectionEntryId, CollectionKind, Volume},
    path::LocalPath,
    playback::{OutputTarget, player_state::PlayerState},
};
use tufo_wire::{library::ImportPaths, player::UpdatePlayerSettings};

mod support;

use support::{TEST_TIMEOUT, TestServer, write_wav};

fn status_code<T>(result: Result<T, ClientError>) -> Option<Code> {
    match result {
        Err(ClientError::Status(status)) => Some(status.code()),
        _ => None,
    }
}

fn some_voice_channel() -> VoiceChannelTarget {
    VoiceChannelTarget {
        guild_id: GuildId::new(std::num::NonZeroU64::MIN),
        channel_id: VoiceChannelId::new(std::num::NonZeroU64::MIN),
    }
}

#[tokio::test]
async fn effects_volume_updates_are_partial_and_invalid_values_leave_settings_unchanged()
-> anyhow::Result<()> {
    use tufo_wire::pb;

    let server = TestServer::start().await?;
    let client = server.client()?;
    client
        .update_player_settings(&UpdatePlayerSettings {
            volume: Some(Volume::try_new(0.25)?),
            effects_volume: Some(Volume::try_new(0.5)?),
            muted: Some(true),
            ..UpdatePlayerSettings::default()
        })
        .await?;
    let mut raw = pb::player_service_client::PlayerServiceClient::new(server.channel().await?);
    for volume in [-0.1, 1.1, f32::NAN, f32::INFINITY] {
        assert_eq!(
            raw.update_player_settings(pb::UpdatePlayerSettingsRequest {
                effects_volume: Some(volume),
                volume: Some(0.75),
                ..pb::UpdatePlayerSettingsRequest::default()
            })
            .await
            .unwrap_err()
            .code(),
            Code::InvalidArgument
        );
    }
    let state = client
        .watch_player()
        .await?
        .next()
        .await
        .transpose()?
        .unwrap();
    assert_eq!(state.effects_volume, Volume::try_new(0.5)?);
    assert_eq!(state.volume, Volume::try_new(0.25)?);
    assert!(state.muted);

    client
        .update_player_settings(&UpdatePlayerSettings {
            effects_volume: Some(Volume::try_new(0.0)?),
            ..UpdatePlayerSettings::default()
        })
        .await?;
    client
        .update_player_settings(&UpdatePlayerSettings::default())
        .await?;
    let state = client
        .watch_player()
        .await?
        .next()
        .await
        .transpose()?
        .unwrap();
    assert_eq!(state.effects_volume, Volume::try_new(0.0)?);
    assert_eq!(state.volume, Volume::try_new(0.25)?);
    assert!(state.muted);
    server.stop().await?;
    Ok(())
}

#[tokio::test]
async fn layer_rpcs_reject_invalid_and_unknown_songs_and_ignore_stopped_layers()
-> anyhow::Result<()> {
    use tufo_wire::{pb, player::UpdateMusicLayer};
    let server = TestServer::start().await?;
    let client = server.client()?;
    let id = CollectionEntryId::from_uuid(uuid::Uuid::from_u128(1));
    assert_eq!(
        status_code(client.add_music_layer(id).await),
        Some(Code::NotFound)
    );
    client.stop_music_layer(id).await?;
    client
        .update_music_layer(
            id,
            UpdateMusicLayer {
                volume: None,
                looping: Some(false),
            },
        )
        .await?;
    let mut raw = pb::player_service_client::PlayerServiceClient::new(server.channel().await?);
    assert_eq!(
        raw.add_music_layer(pb::AddMusicLayerRequest {
            collection_entry_id: "invalid".to_owned()
        })
        .await
        .unwrap_err()
        .code(),
        Code::InvalidArgument
    );
    assert_eq!(
        raw.update_music_layer(pb::UpdateMusicLayerRequest {
            collection_entry_id: id.to_string(),
            volume: Some(2.0),
            looping: None
        })
        .await
        .unwrap_err()
        .code(),
        Code::InvalidArgument
    );
    let music = tempfile::tempdir()?;
    let file = music.path().join("song.wav");
    write_wav(&file, 440.0)?;
    client
        .import_paths(&ImportPaths {
            kind: CollectionKind::Playlist,
            paths: vec![LocalPath::try_new(music.path().to_owned())?],
        })
        .await?;
    let snapshot = client
        .watch_library()
        .await?
        .next()
        .await
        .transpose()?
        .unwrap();
    let entry_id = snapshot.collections[0].entries[0].id;
    std::fs::remove_file(&file)?;
    assert_eq!(
        status_code(client.add_music_layer(entry_id).await),
        Some(Code::NotFound)
    );
    server.stop().await?;
    Ok(())
}

#[tokio::test]
async fn watching_starts_idle_on_local_output_and_ends_when_the_server_drains() -> anyhow::Result<()>
{
    use tufo_models::playback::browser::{BrowserPlayback, BrowserStatus, BrowserStopReason};

    let server = TestServer::start().await?;
    let mut states = server.client()?.watch_player().await?;

    let first = states.next().await.transpose()?;
    let stop = tokio::spawn(server.stop());
    let after_drain = tokio::time::timeout(TEST_TIMEOUT, states.next()).await?;
    stop.await??;

    let idle_without_a_browser = PlayerState {
        browser: BrowserPlayback {
            status: BrowserStatus::Stopped(BrowserStopReason::Unavailable),
            ..BrowserPlayback::default()
        },
        ..PlayerState::default()
    };
    assert_eq!(first, Some(idle_without_a_browser));
    assert!(after_drain.is_none_or(|state| state.is_err()));
    Ok(())
}

#[tokio::test]
async fn a_track_that_went_bad_fails_before_touching_the_audio_device() -> anyhow::Result<()> {
    let server = TestServer::start().await?;
    let client = server.client()?;
    let music = tempfile::tempdir()?;
    let tavern = music.path().join("rpg/tavern.wav");
    let dragon = music.path().join("rpg/dragon.wav");
    write_wav(&tavern, 440.0)?;
    write_wav(&dragon, 330.0)?;
    client
        .import_paths(&ImportPaths {
            kind: CollectionKind::Playlist,
            paths: vec![LocalPath::try_new(music.path().join("rpg"))?],
        })
        .await?;
    let snapshot = client.watch_library().await?.next().await.transpose()?;
    let playlist = snapshot
        .and_then(|snapshot| snapshot.collections.into_iter().next())
        .ok_or_else(|| anyhow::anyhow!("the import created no playlist"))?;
    std::fs::write(&tavern, b"not audio at all")?;
    std::fs::remove_file(&dragon)?;

    let undecodable = client
        .play_collection(playlist.id, Some(playlist.entries[1].id))
        .await;
    let missing = client
        .play_collection(playlist.id, Some(playlist.entries[0].id))
        .await;
    let unknown_sound = client
        .play_sound(CollectionEntryId::from_uuid(uuid::Uuid::from_u128(1)))
        .await;
    server.stop().await?;

    assert_eq!(status_code(undecodable), Some(Code::FailedPrecondition));
    assert_eq!(status_code(missing), Some(Code::NotFound));
    assert_eq!(status_code(unknown_sound), Some(Code::NotFound));
    Ok(())
}

#[tokio::test]
async fn discord_operations_need_credentials_while_local_controls_succeed() -> anyhow::Result<()> {
    let server = TestServer::start().await?;
    let client = server.client()?;

    let join = client
        .select_output(OutputTarget::DiscordVoice(some_voice_channel()))
        .await;
    let list = client.list_voice_channels().await;
    let stop = client.stop_music().await;
    let pause = client.pause_music().await;
    let select_local = client.select_output(OutputTarget::Local).await;
    let stand_by = client.stand_by().await;
    server.stop().await?;

    assert_eq!(status_code(join), Some(Code::FailedPrecondition));
    assert_eq!(status_code(list), Some(Code::FailedPrecondition));
    assert!(stop.is_ok());
    assert_eq!(status_code(pause), Some(Code::FailedPrecondition));
    assert!(select_local.is_ok());
    assert!(stand_by.is_ok());
    Ok(())
}

#[tokio::test]
async fn a_client_can_save_a_token_and_stop_the_server() -> anyhow::Result<()> {
    let server = TestServer::start().await?;
    let client = server.client()?;

    let empty = client.set_discord_bot_token("  ".to_owned()).await;
    client
        .set_discord_bot_token("abc.def.ghi".to_owned())
        .await?;
    let saved = std::fs::read_to_string(server.discord_credentials_path())?;
    client.stop_server().await?;

    assert_eq!(status_code(empty), Some(Code::InvalidArgument));
    assert!(saved.contains("abc.def.ghi"));
    server.stopped().await?;
    Ok(())
}

#[tokio::test]
async fn playback_failures_identify_the_application_reason_without_rpc_context()
-> anyhow::Result<()> {
    use tufo_application::playback::{
        play_collection::PlayCollectionError, play_entry_sound::PlayEntrySoundError,
    };
    use tufo_client::error;
    use tufo_models::{
        library::{CollectionKind, Title},
        path::LocalPath,
    };
    use tufo_wire::library::{Background, CreateCollection, ImportPaths};

    let server = TestServer::start().await.unwrap();
    let client = server.client().unwrap();
    let empty = client
        .create_collection(&CreateCollection {
            kind: CollectionKind::Playlist,
            title: Title::from_name_lossy("Empty"),
            background: Background::Gradient,
        })
        .await
        .unwrap();
    let ClientError::Status(empty) = client.play_collection(empty, None).await.unwrap_err() else {
        anyhow::bail!("expected a remote failure");
    };
    assert_eq!(empty.code(), Code::FailedPrecondition);
    assert_eq!(
        error::reason(&empty).as_deref(),
        Some(PlayCollectionError::NOTHING_PLAYABLE)
    );

    let files = tempfile::tempdir().unwrap();
    let path = files.path().join("rain.wav");
    support::write_wav(&path, 440.0).unwrap();
    client
        .import_paths(&ImportPaths {
            kind: CollectionKind::Soundboard,
            paths: vec![LocalPath::try_new(path.clone()).unwrap()],
        })
        .await
        .unwrap();
    let mut library = client.watch_library().await.unwrap();
    let snapshot = library.next().await.unwrap().unwrap();
    let entry = snapshot
        .collections
        .iter()
        .flat_map(|collection| &collection.entries)
        .next()
        .unwrap()
        .id;
    std::fs::remove_file(path).unwrap();
    client.rescan_library().await.unwrap();
    let ClientError::Status(missing) = client.play_sound(entry).await.unwrap_err() else {
        anyhow::bail!("expected a remote failure");
    };
    assert_eq!(missing.code(), Code::FailedPrecondition);
    assert_eq!(
        error::reason(&missing).as_deref(),
        Some(PlayEntrySoundError::MISSING)
    );
    assert_ne!(error::reason(&empty), error::reason(&missing));
    server.stop().await.unwrap();
    Ok(())
}

#[tokio::test]
async fn queue_rpcs_reject_invalid_unknown_and_missing_songs() -> anyhow::Result<()> {
    use tufo_application::playback::add_to_queue::AddToQueueError;
    use tufo_client::error;
    use tufo_wire::pb;

    let server = TestServer::start().await?;
    let client = server.client()?;
    let unknown = CollectionEntryId::from_uuid(uuid::Uuid::from_u128(1));
    let unknown_added = client.add_to_queue(unknown).await;
    let unknown_removed = client.remove_from_queue(unknown).await;
    let mut raw = pb::player_service_client::PlayerServiceClient::new(server.channel().await?);
    let invalid = raw
        .add_to_queue(pb::AddToQueueRequest {
            collection_entry_id: "invalid".to_owned(),
        })
        .await;
    let music = tempfile::tempdir()?;
    let file = music.path().join("ambush.wav");
    write_wav(&file, 440.0)?;
    // A hand-picked playlist keeps a song whose file vanished, marked missing.
    client
        .import_paths(&ImportPaths {
            kind: CollectionKind::Playlist,
            paths: vec![LocalPath::try_new(file.clone())?],
        })
        .await?;
    let mut library = client.watch_library().await?;
    let snapshot = library.next().await.transpose()?.unwrap();
    let entry_id = snapshot.collections[0].entries[0].id;
    std::fs::remove_file(&file)?;
    client.rescan_library().await?;
    let missing = client.add_to_queue(entry_id).await;
    server.stop().await?;

    assert_eq!(status_code(unknown_added), Some(Code::NotFound));
    assert!(unknown_removed.is_ok());
    assert_eq!(invalid.unwrap_err().code(), Code::InvalidArgument);
    let Err(ClientError::Status(missing)) = missing else {
        anyhow::bail!("expected a remote failure");
    };
    assert_eq!(missing.code(), Code::FailedPrecondition);
    assert_eq!(
        error::reason(&missing).as_deref(),
        Some(AddToQueueError::MISSING)
    );
    Ok(())
}

#[tokio::test]
async fn a_missing_browser_stays_unavailable_and_closing_succeeds() -> anyhow::Result<()> {
    use tufo_models::playback::browser::{BrowserStatus, BrowserStopReason};

    let server = TestServer::start().await?;
    let client = server.client()?;

    let mut states = client.watch_player().await?;
    let initial = states.next().await.transpose()?.unwrap();
    let opened = client.open_browser().await;
    let stopped = states.next().await.transpose()?.unwrap();
    let closed = client.close_browser().await;
    let after_close = states.next().await.transpose()?.unwrap();
    client
        .update_player_settings(&UpdatePlayerSettings {
            browser_volume: Some(Volume::try_new(0.4)?),
            browser_muted: Some(true),
            ..UpdatePlayerSettings::default()
        })
        .await?;
    let adjusted = states.next().await.transpose()?.unwrap();
    server.stop().await?;

    let unavailable = BrowserStatus::Stopped(BrowserStopReason::Unavailable);
    assert_eq!(initial.browser.status, unavailable);
    assert_eq!(status_code(opened), Some(Code::Unavailable));
    assert_eq!(stopped.browser.status, unavailable);
    assert!(closed.is_ok());
    assert_eq!(after_close.browser.status, unavailable);
    assert_eq!(adjusted.browser.volume, Volume::try_new(0.4)?);
    assert!(adjusted.browser.muted);
    Ok(())
}
