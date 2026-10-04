use anyhow::Context as _;
use serde_json::json;

use super::{
    fixtures::{write_long_wav, write_wav},
    library::added_id,
    support::Session,
};

struct MusicFixture {
    session: Session,
    collection_id: String,
    primary_entry_id: String,
    secondary_entry_id: String,
    primary_audio_file_id: String,
}

impl MusicFixture {
    async fn new() -> anyhow::Result<Self> {
        let mut session = Session::new()?;
        write_long_wav(&session.path("music/encounter/a-main.wav"), 32)?;
        write_long_wav(&session.path("music/encounter/b-layer.wav"), 48)?;
        session.start_pulled().await?;
        let imported = session
            .mutation(
                &["library", "import"],
                json!({
                    "kind": "playlist", "paths": [session.path("music/encounter")]
                }),
            )
            .await?;
        let collection_id = added_id(&imported, "collection_ids")?.to_owned();
        let collection = session.json(&["collection", "get", &collection_id]).await?;
        let entries = collection["entries"]
            .as_array()
            .context("playlist has no entries")?;
        anyhow::ensure!(
            entries.len() == 2,
            "fixture playlist must contain two entries"
        );
        let primary_entry_id = entries[0]["id"]
            .as_str()
            .context("missing primary entry ID")?
            .to_owned();
        let secondary_entry_id = entries[1]["id"]
            .as_str()
            .context("missing secondary entry ID")?
            .to_owned();
        let primary_audio_file_id = entries[0]["audio_file_id"]
            .as_str()
            .context("missing audio file ID")?
            .to_owned();
        session
            .mutation(
                &["player", "settings", "update"],
                json!({"repeat": "track", "crossfade_ms": 0}),
            )
            .await?;
        Ok(Self {
            session,
            collection_id,
            primary_entry_id,
            secondary_entry_id,
            primary_audio_file_id,
        })
    }
}

#[tokio::test]
async fn music_queue_and_layers_follow_executable_commands_on_pulled_outputs() -> anyhow::Result<()>
{
    let MusicFixture {
        mut session,
        collection_id,
        primary_entry_id,
        secondary_entry_id,
        primary_audio_file_id,
    } = MusicFixture::new().await?;
    let waveform = session
        .json(&["loop", "waveform", &primary_audio_file_id])
        .await?;
    session
        .mutation(
            &["loop", "update", &primary_audio_file_id],
            json!({
                "source_fingerprint": waveform["fingerprint"],
                "playback_start_frame": 0, "loop_start_frame": 8_000,
                "loop_end_frame": 160_000, "crossfade_frames": 0
            }),
        )
        .await?;
    session
        .json(&[
            "music",
            "play",
            "collection",
            &collection_id,
            "--entry",
            &primary_entry_id,
        ])
        .await?;
    let playing = session.json(&["player", "state"]).await?;
    assert_eq!(playing["music"]["entry"]["entry_id"], primary_entry_id);
    assert_eq!(
        playing["music"]["loop_region"],
        json!({
            "timing": {"sample_rate": 8_000, "frame_count": 240_000},
            "playback_start_frame": 0, "loop_start_frame": 8_000,
            "loop_end_frame": 160_000, "crossfade_frames": 0
        })
    );
    assert_eq!(
        session.json(&["output", "get"]).await?,
        json!({"kind": "local"})
    );
    session
        .mutation(
            &["player", "settings", "update"],
            json!({"repeat": "playlist"}),
        )
        .await?;

    exercise_queue_and_layer_controls(&session, &secondary_entry_id).await?;
    session.json(&["queue", "add", &secondary_entry_id]).await?;
    session.json(&["music", "next"]).await?;
    session
        .wait_player(|state| {
            state
                .music
                .as_ref()
                .is_some_and(|music| music.entry.entry_id.to_string() == secondary_entry_id)
        })
        .await?;
    session.json(&["music", "stop"]).await?;
    let stopped = session
        .wait_player(|state| state.music.is_none() && state.music_layers.is_empty())
        .await?;
    assert_eq!(
        stopped.queued_entry_ids.as_slice(),
        [] as [tufo_models::library::CollectionEntryId; 0]
    );
    let diagnostics = session.json(&["player", "diagnostics"]).await?;
    assert_eq!(diagnostics["local_stream_errors"], 0);
    assert_eq!(diagnostics["local_stream_errors_discarded"], 0);
    session.stop_pulled().await?;
    Ok(())
}

#[tokio::test]
async fn looping_effects_resume_after_standby_and_explicit_stop_clears_saved_playback()
-> anyhow::Result<()> {
    let mut session = Session::new()?;
    let file = session.path("effects/rain.wav");
    let audio_bytes = write_wav(&file)?;
    session.start_pulled().await?;
    let imported = session
        .mutation(
            &["library", "import"],
            json!({"kind": "soundboard", "paths": [file]}),
        )
        .await?;
    let id = added_id(&imported, "entry_ids")?;
    session
        .mutation(
            &["entry", "update", id],
            json!({"sound": {"looping": true, "fade_in_ms": 0, "fade_out_ms": 0}}),
        )
        .await?;
    session.json(&["effect", "play", id]).await?;
    let effects = session.json(&["effect", "list"]).await?;
    assert_eq!(effects["sounds"][0]["entry_id"], id);
    assert_eq!(effects["sounds"][0]["looping"], true);
    session.json(&["effect", "pause"]).await?;
    assert!(session.player().await?.sounds_paused);
    session.json(&["effect", "resume"]).await?;
    assert!(!session.player().await?.sounds_paused);
    session.json(&["player", "standby"]).await?;
    session.wait_player(|state| state.sounds.is_empty()).await?;
    session.json(&["effect", "resume-saved-loops"]).await?;
    session
        .wait_player(|state| {
            state
                .sounds
                .iter()
                .any(|sound| sound.entry_id.to_string() == id && sound.looping)
        })
        .await?;
    session.json(&["effect", "stop", id]).await?;
    session.wait_player(|state| state.sounds.is_empty()).await?;
    session.json(&["effect", "resume-saved-loops"]).await?;
    assert_eq!(
        session.json(&["effect", "list"]).await?["sounds"],
        json!([])
    );
    assert_eq!(std::fs::read(&file)?, audio_bytes);
    session.stop_pulled().await?;
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn bounded_and_cancelled_loop_previews_release_their_mixer_and_resume_local_music()
-> anyhow::Result<()> {
    use super::streams::Watch;

    let MusicFixture {
        mut session,
        primary_entry_id,
        primary_audio_file_id,
        ..
    } = MusicFixture::new().await?;
    session
        .json(&["music", "play", "entry", &primary_entry_id])
        .await?;
    let waveform = session
        .json(&["loop", "waveform", &primary_audio_file_id])
        .await?;
    bounded_preview_resumes_music(
        &session,
        &primary_entry_id,
        &primary_audio_file_id,
        &waveform,
    )
    .await?;
    let payload = json!({
        "source_fingerprint": waveform["fingerprint"],
        "draft": {
            "timing": {"sample_rate": 8_000, "frame_count": 240_000},
            "playback_start_frame": 0, "loop_start_frame": 8_000,
            "loop_end_frame": 160_000, "crossfade_frames": 0
        },
        "mode": {"kind": "seam", "side_ms": 100}
    })
    .to_string();
    let mut preview = Watch::spawn(
        &session,
        &[
            "--json",
            "loop",
            "preview",
            &primary_audio_file_id,
            &payload,
        ],
    )?;
    let started = preview.next().await?;
    assert_eq!(started["event"], "started");
    assert_eq!(started["audio_file_id"], primary_audio_file_id);
    session
        .wait_player(|state| state.music.as_ref().is_some_and(|music| music.paused))
        .await?;
    preview.interrupt()?;
    let ended = preview.next().await?;
    assert_eq!(ended["event"], "ended");
    assert_eq!(ended["reason"], "cancelled");
    preview.stopped().await?;
    let resumed = session
        .wait_player(|state| state.music.as_ref().is_some_and(|music| !music.paused))
        .await?;
    assert_eq!(
        resumed
            .music
            .as_ref()
            .context("music did not resume")?
            .entry
            .entry_id
            .to_string(),
        primary_entry_id
    );
    session
        .failure(&["loop", "get", &primary_audio_file_id], 1)
        .await?;
    session.json(&["music", "stop"]).await?;
    session.stop_pulled().await?;
    Ok(())
}

async fn exercise_queue_and_layer_controls(
    session: &Session,
    secondary_entry_id: &str,
) -> anyhow::Result<()> {
    session.json(&["queue", "add", secondary_entry_id]).await?;
    assert_eq!(
        session.json(&["queue", "list"]).await?["queued_entry_ids"],
        json!([secondary_entry_id])
    );
    session
        .json(&["queue", "remove", secondary_entry_id])
        .await?;
    assert_eq!(
        session.json(&["queue", "list"]).await?["queued_entry_ids"],
        json!([])
    );
    session.json(&["layer", "add", secondary_entry_id]).await?;
    session
        .mutation(
            &["layer", "update", secondary_entry_id],
            json!({"volume": 0.5, "looping": true}),
        )
        .await?;
    session
        .mutation(
            &["layer", "update", secondary_entry_id],
            json!({"volume": 0.25}),
        )
        .await?;
    let layers = session.json(&["layer", "list"]).await?;
    assert_eq!(
        layers["music_layers"][0]["music"]["entry"]["entry_id"],
        secondary_entry_id
    );
    assert_eq!(layers["music_layers"][0]["volume"], 0.25);
    assert_eq!(layers["music_layers"][0]["looping"], true);

    session.json(&["music", "pause"]).await?;
    let paused = session
        .wait_player(|state| {
            state.music.as_ref().is_some_and(|music| music.paused)
                && state
                    .music_layers
                    .iter()
                    .all(|(_, layer)| layer.music.paused)
        })
        .await?;
    assert_eq!(paused.music_layers.len(), 1);
    session.json(&["music", "seek", "750"]).await?;
    let sought = session.json(&["music", "state"]).await?;
    assert_eq!(sought["music"]["position"]["position_ms"], 750);
    session.json(&["music", "resume"]).await?;
    session
        .wait_player(|state| {
            state.music.as_ref().is_some_and(|music| !music.paused)
                && state
                    .music_layers
                    .iter()
                    .all(|(_, layer)| !layer.music.paused)
        })
        .await?;
    session.json(&["layer", "stop", secondary_entry_id]).await?;
    assert_eq!(
        session.json(&["layer", "list"]).await?["music_layers"],
        json!([])
    );
    Ok(())
}

#[cfg(unix)]
async fn bounded_preview_resumes_music(
    session: &Session,
    primary_entry_id: &str,
    primary_audio_file_id: &str,
    waveform: &serde_json::Value,
) -> anyhow::Result<()> {
    let bounded_payload = json!({
        "source_fingerprint": waveform["fingerprint"],
        "draft": {
            "timing": {"sample_rate": 8_000, "frame_count": 240_000},
            "playback_start_frame": 0, "loop_start_frame": 8_000,
            "loop_end_frame": 160_000, "crossfade_frames": 0
        },
        "mode": {"kind": "from_frame", "start_frame": 4_000}
    })
    .to_string();
    let bounded = session
        .ndjson(&[
            "loop",
            "preview",
            primary_audio_file_id,
            &bounded_payload,
            "--duration-ms",
            "150",
        ])
        .await?;
    assert_eq!(bounded.len(), 2);
    assert_eq!(bounded[0]["event"], "started");
    assert_eq!(bounded[0]["audio_file_id"], primary_audio_file_id);
    assert_eq!(
        bounded[0]["mode"],
        json!({"kind": "from_frame", "start_frame": 4_000})
    );
    assert_eq!(bounded[1]["event"], "ended");
    assert_eq!(bounded[1]["reason"], "duration");
    assert!(
        bounded[1]["elapsed_ms"]
            .as_u64()
            .context("missing elapsed_ms")?
            >= 150
    );
    let resumed = session
        .wait_player(|state| state.music.as_ref().is_some_and(|music| !music.paused))
        .await?;
    assert_eq!(
        resumed
            .music
            .as_ref()
            .context("music did not resume after bounded preview")?
            .entry
            .entry_id
            .to_string(),
        primary_entry_id
    );
    Ok(())
}
