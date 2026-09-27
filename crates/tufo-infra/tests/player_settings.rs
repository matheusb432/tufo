//! Player settings saved through one engine come back in the next one.

use std::time::Duration;

use tufo_application::playback::{player::Player as _, update_player_settings};
use tufo_infra::{database::open_database, discord::Discord, player::EnginePlayer};
use tufo_models::{
    library::Volume,
    playback::{player_state::CrossfadeDuration, queue::RepeatMode},
};
use tufo_wire::player::UpdatePlayerSettings;

#[tokio::test(flavor = "multi_thread")]
async fn saved_settings_survive_a_restart() {
    let directory = tempfile::tempdir().unwrap();
    let database = open_database(&directory.path().join("tufo.sqlite3"))
        .await
        .unwrap();
    let update = UpdatePlayerSettings {
        volume: Some(Volume::try_new(0.25).unwrap()),
        muted: Some(true),
        shuffle: Some(true),
        repeat: Some(RepeatMode::Track),
        crossfade: Some(CrossfadeDuration::try_new(Duration::from_millis(500)).unwrap()),
    };
    let before = EnginePlayer::start(Discord::NotConfigured);
    update_player_settings::execute(update, before.as_ref(), database.clone())
        .await
        .unwrap();

    let after = EnginePlayer::start(Discord::NotConfigured);
    update_player_settings::restore(after.as_ref(), database)
        .await
        .unwrap();

    let state = after.watch_player().borrow().clone();
    assert_eq!(state.volume, Volume::try_new(0.25).unwrap());
    assert!(state.muted && state.modes.shuffle);
    assert_eq!(state.modes.repeat, RepeatMode::Track);
    assert_eq!(state.crossfade.into_inner(), Duration::from_millis(500));
}
