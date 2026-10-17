//! Player settings saved through one engine come back in the next one.

use std::time::Duration;

use tufo_application::playback::{
    player::{Player as _, SongLoops},
    update_player_settings,
};
use tufo_infra::{
    browser::UnavailableBrowser, database::open_database, discord::SongbirdDiscordClient,
    player::EnginePlayer,
};
use tufo_models::{
    library::{AudioFileId, Volume, song_loop::LoopRegion},
    playback::{player_state::CrossfadeDuration, queue::RepeatMode},
};
use tufo_wire::player::UpdatePlayerSettings;

struct NoSongLoops;

impl SongLoops for NoSongLoops {
    fn loop_region(&self, _: AudioFileId) -> Option<LoopRegion> {
        None
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn saved_settings_survive_a_restart() {
    let directory = tempfile::tempdir().unwrap();
    let database = open_database(&directory.path().join("tufo.sqlite3"))
        .await
        .unwrap();
    let update = UpdatePlayerSettings {
        volume: Some(Volume::try_new(0.25).unwrap()),
        effects_volume: Some(Volume::try_new(0.5).unwrap()),
        muted: Some(true),
        shuffle: Some(true),
        repeat: Some(RepeatMode::Track),
        crossfade: Some(CrossfadeDuration::try_new(Duration::from_millis(500)).unwrap()),
        browser_volume: Some(Volume::try_new(0.75).unwrap()),
        browser_muted: Some(true),
    };
    let before = EnginePlayer::start(
        SongbirdDiscordClient::not_configured(),
        NoSongLoops,
        UnavailableBrowser,
    );
    update_player_settings::execute(update, before.as_ref(), database.clone())
        .await
        .unwrap();

    let after = EnginePlayer::start(
        SongbirdDiscordClient::not_configured(),
        NoSongLoops,
        UnavailableBrowser,
    );
    update_player_settings::restore(after.as_ref(), database)
        .await
        .unwrap();

    let state = after.watch_player().borrow().clone();
    assert_eq!(state.volume, Volume::try_new(0.25).unwrap());
    assert_eq!(state.effects_volume, Volume::try_new(0.5).unwrap());
    assert!(state.muted && state.modes.shuffle);
    assert_eq!(state.modes.repeat, RepeatMode::Track);
    assert_eq!(state.crossfade.into_inner(), Duration::from_millis(500));
    assert_eq!(state.browser.volume, Volume::try_new(0.75).unwrap());
    assert!(state.browser.muted);
}

#[tokio::test]
async fn existing_player_settings_default_effects_and_browser_to_full_volume() -> anyhow::Result<()>
{
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("tufo.sqlite3");
    let database = open_database(&path).await?;
    let before = EnginePlayer::start(
        SongbirdDiscordClient::not_configured(),
        NoSongLoops,
        UnavailableBrowser,
    );
    update_player_settings::execute(
        UpdatePlayerSettings {
            volume: Some(Volume::try_new(0.25)?),
            muted: Some(true),
            ..UpdatePlayerSettings::default()
        },
        before.as_ref(),
        database,
    )
    .await?;
    let legacy = rusqlite::Connection::open(&path)?;
    legacy.execute_batch(
        "DROP TABLE song_loops;
         ALTER TABLE player_settings DROP COLUMN effects_volume;
         ALTER TABLE player_settings DROP COLUMN browser_volume;
         ALTER TABLE player_settings DROP COLUMN browser_muted;
         ALTER TABLE collections DROP COLUMN pinned;
         ALTER TABLE collection_entries DROP COLUMN position;
         DELETE FROM __toasty_migrations
         WHERE name IN (
             '0009_effects_volume.sql', '0010_song_loops.sql', '0011_browser_settings.sql',
             '0012_pinned_collections.sql', '0013_custom_entry_order.sql'
         );",
    )?;
    drop(legacy);

    let migrated = open_database(&path).await?;
    let after = EnginePlayer::start(
        SongbirdDiscordClient::not_configured(),
        NoSongLoops,
        UnavailableBrowser,
    );
    update_player_settings::restore(after.as_ref(), migrated).await?;
    let state = after.watch_player().borrow().clone();
    assert_eq!(state.effects_volume, Volume::default());
    assert_eq!(state.browser.volume, Volume::default());
    assert!(!state.browser.muted);
    assert_eq!(state.volume, Volume::try_new(0.25)?);
    assert!(state.muted);

    let connection = rusqlite::Connection::open(&path)?;
    for column in ["effects_volume", "browser_volume"] {
        for volume in [-0.1, 1.1, f64::NAN] {
            assert!(
                connection
                    .execute(
                        &format!("UPDATE player_settings SET {column} = ?1"),
                        [volume]
                    )
                    .is_err()
            );
        }
    }
    Ok(())
}
