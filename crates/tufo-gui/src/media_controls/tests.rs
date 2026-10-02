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
    let ended = MediaState::from_player(
        &snapshot,
        &player,
        SystemTime::UNIX_EPOCH + Duration::from_secs(200),
    );
    assert_eq!(ended.track.unwrap().position, Duration::from_secs(120));
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

#[cfg(target_os = "linux")]
#[test]
fn seeks_are_distinguished_from_clock_updates_pause_and_track_changes() {
    let track = |music| {
        MediaState::from_player(
            &LibrarySnapshot::default(),
            &PlayerState {
                music: Some(music),
                ..PlayerState::default()
            },
            SystemTime::UNIX_EPOCH + Duration::from_secs(5),
        )
        .track
        .unwrap()
    };
    let previous = track(music(false));
    let mut current = music(false);
    current.position.sampled_at += Duration::from_secs(5);
    for (position_ms, jumped) in [
        (15_000, false),
        (15_040, false),
        (30_000, true),
        (5_000, true),
    ] {
        current.position.position = Duration::from_millis(position_ms);
        assert_eq!(
            track(current.clone()).position_jumped_since(Some(&previous)),
            jumped
        );
    }
    current.paused = true;
    current.position.position = Duration::from_secs(15);
    assert!(!track(current.clone()).position_jumped_since(Some(&previous)));
    current.position.position = Duration::from_secs(5);
    assert!(track(current.clone()).position_jumped_since(Some(&previous)));
    let paused = track(music(true));
    assert!(track(current.clone()).position_jumped_since(Some(&paused)));
    current.entry.entry_id = CollectionEntryId::from_uuid(uuid::Uuid::from_u128(3));
    assert!(track(current.clone()).position_jumped_since(Some(&paused)));
    assert!(track(current.clone()).position_jumped_since(None));
    current.position.position = Duration::ZERO;
    assert!(!track(current.clone()).position_jumped_since(Some(&paused)));
    assert!(!track(current).position_jumped_since(None));
}
