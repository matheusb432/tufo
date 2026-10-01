use tufo_models::{
    library::{CollectionId, Volume},
    playback::{
        AudioFilePath,
        music_layers::MusicLayerState,
        player_state::{MusicState, PositionSample},
        queue::QueueEntry,
    },
};

use super::*;

fn music(paused: bool) -> MusicState {
    MusicState {
        collection_id: CollectionId::from_uuid(uuid::Uuid::from_u128(1)),
        entry: QueueEntry {
            entry_id: CollectionEntryId::from_uuid(uuid::Uuid::from_u128(2)),
            path: AudioFilePath::try_new(std::env::temp_dir().join("tufo-media-test.wav")).unwrap(),
            title: "Forest".to_owned(),
            duration: Some(Duration::from_secs(120)),
        },
        position: PositionSample {
            position: Duration::from_secs(10),
            sampled_at: SystemTime::UNIX_EPOCH,
        },
        duration: Some(Duration::from_secs(120)),
        paused,
    }
}

#[test]
fn paused_music_remains_resumable_and_its_position_stays_put() {
    let mut player = PlayerState {
        music: Some(music(true)),
        ..PlayerState::default()
    };
    let snapshot = LibrarySnapshot::default();
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(5);
    let paused = MediaState::from_player(&snapshot, &player, now);
    assert!(paused.track.as_ref().unwrap().paused);
    assert_eq!(
        paused.track.as_ref().unwrap().position,
        Duration::from_secs(10)
    );

    player.music.as_mut().unwrap().paused = false;
    let playing = MediaState::from_player(&snapshot, &player, now);
    assert!(!playing.track.as_ref().unwrap().paused);
    assert_eq!(
        playing.track.as_ref().unwrap().position,
        Duration::from_secs(15)
    );
    assert_eq!(
        playing.track.unwrap().metadata,
        paused.track.unwrap().metadata
    );
}

#[test]
fn a_music_layer_keeps_media_controls_available_after_the_main_track_stops() {
    let mut player = PlayerState::default();
    player
        .music_layers
        .try_insert(MusicLayerState {
            music: music(true),
            volume: Volume::default(),
            looping: true,
        })
        .unwrap();
    let state =
        MediaState::from_player(&LibrarySnapshot::default(), &player, SystemTime::UNIX_EPOCH);
    assert_eq!(state.track.as_ref().unwrap().metadata.title, "Forest");
    assert!(state.track.unwrap().paused);
    assert!(!state.can_skip);
    assert!(!state.can_seek);
}

#[test]
fn library_duration_does_not_advertise_seeking_before_the_player_knows_the_length() {
    let mut player = PlayerState {
        music: Some(music(false)),
        ..PlayerState::default()
    };
    player.music.as_mut().unwrap().duration = None;
    let state =
        MediaState::from_player(&LibrarySnapshot::default(), &player, SystemTime::UNIX_EPOCH);
    assert_eq!(
        state.track.unwrap().metadata.duration,
        Some(Duration::from_secs(120))
    );
    assert!(!state.can_seek);
}

#[test]
fn stopping_music_clears_metadata_and_position_never_exceeds_the_track() {
    let mut player = PlayerState {
        music: Some(music(false)),
        ..PlayerState::default()
    };
    let snapshot = LibrarySnapshot::default();
    let state = MediaState::from_player(
        &snapshot,
        &player,
        SystemTime::UNIX_EPOCH + Duration::from_secs(200),
    );
    assert_eq!(state.track.unwrap().position, Duration::from_secs(120));
    player.music = None;
    let stopped = MediaState::from_player(&snapshot, &player, SystemTime::UNIX_EPOCH);
    assert!(stopped.track.is_none());
    assert!(!stopped.can_skip);
}
