use anyhow::Context as _;
use serde_json::{Value, json};

use super::{
    fixtures::write_wav,
    support::{Session, identifier},
};

#[tokio::test]
async fn collections_return_discoverable_ids_and_preserve_omitted_update_fields()
-> anyhow::Result<()> {
    let mut session = Session::new()?;
    session.start().await?;
    let created = session
        .mutation(
            &["collection", "add"],
            json!({
                "kind": "playlist", "title": "  Night Travel  ",
                "background": {"kind": "preset", "preset": "tavern"}
            }),
        )
        .await?;
    let id = identifier(&created)?;
    assert_eq!(created["resource"], "collection");
    assert!(created["action"].is_string());
    let original = session.json(&["collection", "get", id]).await?;
    assert_eq!(original["title"], "Night Travel");
    assert_eq!(
        original["background"],
        json!({"kind": "preset", "preset": "tavern"})
    );
    let trailing_json = session
        .success(&["collection", "get", id, "--json"])
        .await?;
    assert_eq!(
        serde_json::from_slice::<Value>(&trailing_json.stdout)?,
        original
    );
    let found = session
        .json(&["collection", "search", "TRAVEL night", "--kind", "playlist"])
        .await?;
    assert_eq!(
        found.as_array().context("search was not an array")?.len(),
        1
    );
    assert_eq!(found[0]["id"], id);

    session
        .mutation(
            &["collection", "update", id],
            json!({
                "order": {"key": "duration", "descending": true}
            }),
        )
        .await?;
    session
        .mutation(
            &["collection", "update", id],
            json!({"order": {"key": "date_added"}}),
        )
        .await?;
    let ordered = session.json(&["collection", "get", id]).await?;
    assert_eq!(ordered["title"], original["title"]);
    assert_eq!(ordered["background"], original["background"]);
    assert_eq!(
        ordered["order"],
        json!({"key": "date_added", "descending": true})
    );
    session
        .mutation(&["collection", "update", id], json!({"background": null}))
        .await?;
    let cleared = session.json(&["collection", "get", id]).await?;
    assert_eq!(cleared["background"], json!({"kind": "gradient"}));
    assert_eq!(cleared["title"], original["title"]);
    assert_eq!(cleared["order"], ordered["order"]);

    let invalid = session
        .failure(
            &[
                "collection",
                "update",
                id,
                r#"{"order":{"descending":false,"typo":true}}"#,
            ],
            2,
        )
        .await?;
    assert!(String::from_utf8(invalid.stderr)?.contains("unknown field"));
    assert_eq!(session.json(&["collection", "get", id]).await?, cleared);
    assert!(
        session
            .library()
            .await?
            .collections
            .iter()
            .any(|collection| collection.id.to_string() == id)
    );

    let deleted = session.json(&["collection", "delete", id]).await?;
    assert_eq!(deleted["id"], id);
    session
        .failure(&["--json", "collection", "get", id], 1)
        .await?;
    assert!(
        !session
            .library()
            .await?
            .collections
            .iter()
            .any(|collection| collection.id.to_string() == id)
    );
    Ok(())
}

#[tokio::test]
async fn imported_audio_is_discoverable_reusable_and_left_untouched() -> anyhow::Result<()> {
    let mut session = Session::new()?;
    let audio_path = session.path("audio/encounter/thunder.wav");
    let audio_bytes = write_wav(&audio_path)?;
    session.start().await?;
    let imported = session
        .mutation(
            &["library", "import"],
            json!({
                "kind": "playlist", "paths": [session.path("audio/encounter")]
            }),
        )
        .await?;
    assert_eq!(imported["report"]["collections_created"], 1);
    assert_eq!(imported["report"]["files_added"], 1);
    let playlist_id = added_id(&imported, "collection_ids")?;
    let entry_id = added_id(&imported, "entry_ids")?;
    let song_id = added_id(&imported, "audio_file_ids")?;
    let folder_id = added_id(&imported, "folder_ids")?;
    let playlist = session.json(&["collection", "get", playlist_id]).await?;
    assert_eq!(playlist["entries"][0]["id"], entry_id);
    assert_eq!(playlist["entries"][0]["audio_file_id"], song_id);
    assert_eq!(playlist["entries"][0]["duration_ms"], 1000);
    assert_eq!(
        session.json(&["folder", "get", folder_id]).await?["folder"],
        json!(session.path("audio/encounter"))
    );
    assert_eq!(
        session.json(&["song", "get", song_id]).await?["path"],
        json!(audio_path)
    );

    let board = session
        .mutation(
            &["collection", "add"],
            json!({"kind": "soundboard", "title": "Weather"}),
        )
        .await?;
    let board_id = identifier(&board)?;
    let added = session
        .mutation(
            &["collection", "add-files", board_id],
            json!({"paths": [audio_path]}),
        )
        .await?;
    let sound_id = added_id(&added, "entry_ids")?;
    customize_sound_and_verify_shared_song(&session, sound_id, song_id, entry_id).await?;

    let manual = session
        .mutation(
            &["collection", "add"],
            json!({"kind": "playlist", "title": "Finale"}),
        )
        .await?;
    let manual_id = identifier(&manual)?;
    let added = session
        .mutation(
            &["collection", "add-songs", manual_id],
            json!({"audio_file_ids": [song_id]}),
        )
        .await?;
    let copied_entry_id = added_id(&added, "entry_ids")?;
    assert_eq!(
        session.json(&["entry", "get", copied_entry_id]).await?["audio_file_id"],
        song_id
    );
    session.json(&["entry", "delete", copied_entry_id]).await?;
    session.json(&["song", "get", song_id]).await?;
    session.json(&["library", "rescan"]).await?;
    session.json(&["folder", "unfollow", folder_id]).await?;
    session.failure(&["folder", "get", folder_id], 1).await?;
    assert_eq!(std::fs::read(&audio_path)?, audio_bytes);
    assert!(session.player().await?.music.is_none());
    assert_eq!(
        session.json(&["effect", "list"]).await?["sounds"],
        json!([])
    );
    Ok(())
}

#[tokio::test]
async fn saved_loops_use_source_frames_and_round_trip_through_the_portable_document()
-> anyhow::Result<()> {
    let mut session = Session::new()?;
    let audio_path = session.path("audio/song.wav");
    let audio_bytes = write_wav(&audio_path)?;
    session.start().await?;
    let imported = session
        .mutation(
            &["library", "import"],
            json!({"kind": "playlist", "paths": [audio_path]}),
        )
        .await?;
    let song_id = added_id(&imported, "audio_file_ids")?;
    let waveform = session.json(&["loop", "waveform", song_id]).await?;
    assert_eq!(
        waveform["timing"],
        json!({"sample_rate": 8_000, "frame_count": 8_000})
    );
    assert_ne!(
        waveform["peaks"]
            .as_array()
            .context("missing peaks")?
            .as_slice(),
        [] as [Value; 0]
    );
    let fingerprint = waveform["fingerprint"]
        .as_str()
        .context("missing fingerprint")?;
    session
        .mutation(
            &["loop", "update", song_id],
            json!({
                "source_fingerprint": fingerprint,
                "playback_start_frame": 800, "loop_start_frame": 2000,
                "loop_end_frame": 6000, "crossfade_frames": 400
            }),
        )
        .await?;
    session
        .mutation(
            &["loop", "update", song_id],
            json!({"source_fingerprint": fingerprint, "crossfade_frames": 200}),
        )
        .await?;
    session
        .mutation(
            &["loop", "update", song_id],
            json!({"source_fingerprint": fingerprint, "playback_start_frame": 0}),
        )
        .await?;
    let saved = session.json(&["loop", "get", song_id]).await?;
    assert_eq!(
        saved["saved_loop"]["region"],
        json!({
            "timing": {"sample_rate": 8_000, "frame_count": 8_000},
            "playback_start_frame": 0, "loop_start_frame": 2000,
            "loop_end_frame": 6000, "crossfade_frames": 200
        })
    );
    assert_eq!(saved["saved_loop"]["needs_review"], false);
    let mismatch =
        json!({"source_fingerprint": "ab".repeat(32), "crossfade_frames": 0}).to_string();
    session
        .failure(&["loop", "update", song_id, &mismatch], 1)
        .await?;
    assert_eq!(session.json(&["loop", "get", song_id]).await?, saved);

    let document = session.json(&["song-loops", "export"]).await?;
    let document_path = session.path("loops.json");
    std::fs::write(&document_path, serde_json::to_vec(&document)?)?;
    session.json(&["loop", "delete", song_id]).await?;
    session.failure(&["loop", "get", song_id], 1).await?;
    let document_argument = document_path.to_str().context("non-UTF-8 fixture path")?;
    let dry_run = session
        .json(&["song-loops", "import", document_argument, "--dry-run"])
        .await?;
    assert_eq!(dry_run[0]["outcome"], "saved");
    session.failure(&["loop", "get", song_id], 1).await?;
    let imported_loops = session
        .json(&["song-loops", "import", document_argument])
        .await?;
    assert_eq!(imported_loops[0]["audio_file_id"], song_id);
    assert_eq!(imported_loops[0]["outcome"], "saved");
    assert_eq!(session.json(&["loop", "get", song_id]).await?, saved);
    assert_eq!(std::fs::read(&audio_path)?, audio_bytes);
    assert!(session.player().await?.music.is_none());
    Ok(())
}

pub(super) fn added_id<'a>(record: &'a Value, key: &str) -> anyhow::Result<&'a str> {
    let identifiers = record["resources"][key]
        .as_array()
        .with_context(|| format!("missing added.{key}"))?;
    anyhow::ensure!(identifiers.len() == 1, "expected one added.{key}: {record}");
    identifiers[0]
        .as_str()
        .context("resource id is not a string")
}

async fn customize_sound_and_verify_shared_song(
    session: &Session,
    sound_id: &str,
    song_id: &str,
    entry_id: &str,
) -> anyhow::Result<()> {
    session
        .mutation(
            &["entry", "update", sound_id],
            json!({
                "title": "Thunderstorm", "icon": "cloud_rain",
                "sound": {"looping": true, "volume": 0.5, "fade_in_ms": 300, "fade_out_ms": 600}
            }),
        )
        .await?;
    session
        .mutation(
            &["entry", "update", sound_id],
            json!({"sound": {"volume": 0.25}}),
        )
        .await?;
    let sound = session.json(&["entry", "get", sound_id]).await?;
    assert_eq!(
        sound["sound"],
        json!({"looping": true, "volume": 0.25, "fade_in_ms": 300, "fade_out_ms": 600})
    );
    assert_eq!(sound["icon"], "cloud_rain");
    assert_eq!(
        session.json(&["song", "get", song_id]).await?["title"],
        "Thunderstorm"
    );
    assert_eq!(
        session.json(&["entry", "get", entry_id]).await?["title"],
        "Thunderstorm"
    );
    let invalid = session
        .failure(
            &[
                "entry",
                "update",
                sound_id,
                r#"{"sound":{"volume":0.75,"typo":true}}"#,
            ],
            2,
        )
        .await?;
    assert!(String::from_utf8(invalid.stderr)?.contains("unknown field"));
    assert_eq!(session.json(&["entry", "get", sound_id]).await?, sound);
    session
        .mutation(&["entry", "update", sound_id], json!({"icon": null}))
        .await?;
    let cleared = session.json(&["entry", "get", sound_id]).await?;
    assert_eq!(cleared["icon"], "audio_lines");
    assert_eq!(cleared["sound"], sound["sound"]);

    Ok(())
}
