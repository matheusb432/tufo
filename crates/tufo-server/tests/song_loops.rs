//! Saving, sharing, and previewing song loops through the typed client.

mod support;

use std::path::Path;

use support::{TEST_TIMEOUT, TestServer, write_wav};
use tufo_application::{
    library::{import_song_loops::ImportSongLoopsError, update_song_loop::UpdateSongLoopError},
    playback::preview_song_loop::PreviewSongLoopError,
};
use tufo_client::{ClientError, Code, TufoClient, error};
use tufo_models::{
    library::{
        AudioFileId, AudioFingerprint, CollectionKind,
        song_loop::{LoopMarkers, SeamPreviewSide, SourceFrames},
    },
    path::LocalPath,
};
use tufo_wire::{
    library::{ImportPaths, LibrarySnapshot},
    song_loops::{
        ImportSongLoops, PreviewMode, PreviewSongLoop, SavedSongLoop, SongLoopImportOutcome,
        UpdateSongLoop,
    },
};

/// The test tone lasts one second at 8 kHz.
const TONE_FRAMES: u64 = 8_000;

struct Fixture {
    server: TestServer,
    client: TufoClient,
    music: tempfile::TempDir,
    song: AudioFileId,
}

async fn fixture() -> anyhow::Result<Fixture> {
    let server = TestServer::start().await?;
    let client = server.client()?;
    let music = tempfile::tempdir()?;
    write_wav(&music.path().join("boss/dragon.wav"), 440.0)?;
    client
        .import_paths(&ImportPaths {
            kind: CollectionKind::Playlist,
            paths: vec![LocalPath::try_new(music.path().join("boss"))?],
        })
        .await?;
    let song = latest_snapshot(&client).await?.songs[0].audio_file_id;
    Ok(Fixture {
        server,
        client,
        music,
        song,
    })
}

async fn latest_snapshot(client: &TufoClient) -> anyhow::Result<LibrarySnapshot> {
    let mut snapshots = client.watch_library().await?;
    Ok(tokio::time::timeout(TEST_TIMEOUT, snapshots.next())
        .await?
        .ok_or_else(|| anyhow::anyhow!("the library stream ended"))??)
}

async fn saved_loop(
    client: &TufoClient,
    song: AudioFileId,
) -> anyhow::Result<Option<SavedSongLoop>> {
    Ok(latest_snapshot(client)
        .await?
        .songs
        .into_iter()
        .find(|view| view.audio_file_id == song)
        .and_then(|view| view.saved_loop))
}

fn markers(playback_start: u64, loop_start: u64, loop_end: u64, crossfade: u64) -> LoopMarkers {
    LoopMarkers {
        playback_start: SourceFrames(playback_start),
        loop_start: SourceFrames(loop_start),
        loop_end: SourceFrames(loop_end),
        crossfade: SourceFrames(crossfade),
    }
}

fn full_update(
    song: AudioFileId,
    fingerprint: &AudioFingerprint,
    markers: LoopMarkers,
) -> UpdateSongLoop {
    UpdateSongLoop {
        audio_file_id: song,
        source_fingerprint: fingerprint.clone(),
        playback_start: Some(markers.playback_start),
        loop_start: Some(markers.loop_start),
        loop_end: Some(markers.loop_end),
        crossfade: Some(markers.crossfade),
    }
}

fn reason(result: &Result<impl Sized, ClientError>) -> Option<(Code, String)> {
    match result {
        Err(ClientError::Status(status)) => Some((status.code(), error::reason(status)?)),
        _ => None,
    }
}

fn sha256_hex(path: &Path) -> anyhow::Result<String> {
    use std::fmt::Write as _;

    use sha2::Digest as _;
    Ok(sha2::Sha256::digest(std::fs::read(path)?)
        .iter()
        .fold(String::new(), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        }))
}

#[tokio::test]
async fn markers_save_against_the_decoded_frames_and_partial_updates_keep_the_rest()
-> anyhow::Result<()> {
    let Fixture {
        server,
        client,
        song,
        music: _music,
    } = fixture().await?;
    let waveform = client.get_song_waveform(song).await?;

    client
        .update_song_loop(&full_update(
            song,
            &waveform.fingerprint,
            markers(800, 2_000, 6_000, 400),
        ))
        .await?;
    client
        .update_song_loop(&UpdateSongLoop {
            audio_file_id: song,
            source_fingerprint: waveform.fingerprint.clone(),
            playback_start: None,
            loop_start: None,
            loop_end: None,
            crossfade: Some(SourceFrames(200)),
        })
        .await?;
    let saved = saved_loop(&client, song).await?;
    let out_of_order = client
        .update_song_loop(&full_update(
            song,
            &waveform.fingerprint,
            markers(0, 6_000, 2_000, 0),
        ))
        .await;
    let other_audio = client
        .update_song_loop(&full_update(
            song,
            &AudioFingerprint::try_new("ab".repeat(32))?,
            markers(0, 0, 100, 0),
        ))
        .await;

    assert_eq!(waveform.timing.sample_rate.get(), 8_000);
    assert_eq!(waveform.timing.frame_count, SourceFrames(TONE_FRAMES));
    assert!(!waveform.peaks.is_empty() && waveform.peaks.iter().any(|peak| *peak > 0));
    let saved = saved.ok_or_else(|| anyhow::anyhow!("the loop was not saved"))?;
    assert!(!saved.needs_review);
    assert_eq!(saved.region.markers(), markers(800, 2_000, 6_000, 200));
    assert_eq!(
        reason(&out_of_order),
        Some((
            Code::InvalidArgument,
            UpdateSongLoopError::INVALID_MARKERS.to_owned()
        ))
    );
    assert_eq!(
        reason(&other_audio),
        Some((
            Code::FailedPrecondition,
            UpdateSongLoopError::SOURCE_CHANGED.to_owned()
        ))
    );
    server.stop().await
}

#[tokio::test]
async fn exported_loops_import_again_and_report_conflicts() -> anyhow::Result<()> {
    let Fixture {
        server,
        client,
        song,
        music: _music,
    } = fixture().await?;
    let fingerprint = client.get_song_waveform(song).await?.fingerprint;
    client
        .update_song_loop(&full_update(
            song,
            &fingerprint,
            markers(0, 1_000, 7_000, 0),
        ))
        .await?;
    let document = client.export_song_loops().await?;
    client.delete_song_loop(song).await?;
    let import = |dry_run, replace_existing| ImportSongLoops {
        document: document.clone(),
        dry_run,
        replace_existing,
    };

    let dry_run = client.import_song_loops(&import(true, false)).await?;
    let after_dry_run = saved_loop(&client, song).await?;
    let saved = client.import_song_loops(&import(false, false)).await?;
    let unchanged = client.import_song_loops(&import(false, false)).await?;
    client
        .update_song_loop(&full_update(
            song,
            &fingerprint,
            markers(0, 2_000, 7_000, 0),
        ))
        .await?;
    let conflict = client.import_song_loops(&import(false, false)).await?;
    let replaced = client.import_song_loops(&import(false, true)).await?;
    let invalid = client
        .import_song_loops(&ImportSongLoops {
            document: "{}".to_owned(),
            dry_run: false,
            replace_existing: false,
        })
        .await;

    let outcome = |results: &[tufo_wire::song_loops::SongLoopImportResult]| {
        results
            .iter()
            .map(|result| result.outcome)
            .collect::<Vec<_>>()
    };
    assert_eq!(outcome(&dry_run), [SongLoopImportOutcome::Saved]);
    assert_eq!(after_dry_run, None);
    assert_eq!(outcome(&saved), [SongLoopImportOutcome::Saved]);
    assert_eq!(saved[0].audio_file_id, Some(song));
    assert_eq!(outcome(&unchanged), [SongLoopImportOutcome::Unchanged]);
    assert_eq!(outcome(&conflict), [SongLoopImportOutcome::Conflict]);
    assert_eq!(outcome(&replaced), [SongLoopImportOutcome::Replaced]);
    assert_eq!(
        saved_loop(&client, song)
            .await?
            .map(|saved| saved.region.markers()),
        Some(markers(0, 1_000, 7_000, 0))
    );
    assert_eq!(
        reason(&invalid),
        Some((
            Code::InvalidArgument,
            ImportSongLoopsError::INVALID_DOCUMENT.to_owned()
        ))
    );
    server.stop().await
}

#[tokio::test]
async fn a_whole_file_hash_matches_only_the_song_at_its_path() -> anyhow::Result<()> {
    let Fixture {
        server,
        client,
        music,
        song,
    } = fixture().await?;
    let path = music.path().join("boss/dragon.wav");
    let path = std::fs::canonicalize(path)?;
    let document = |path: &Path, sha256: &str, frame_count: u64| {
        serde_json::json!({
            "format": "song-loops",
            "version": 1,
            "songs": [{
                "path": path,
                "sha256": sha256,
                "sample_rate": 8_000,
                "frame_count": frame_count,
                "playback_start_frame": 0,
                "loop_start_frame": 1_000,
                "loop_end_frame": 7_000,
                "crossfade_frames": 0
            }]
        })
        .to_string()
    };
    let import = |document: String| ImportSongLoops {
        document,
        dry_run: true,
        replace_existing: false,
    };
    let sha256 = sha256_hex(&path)?;

    let matching = client
        .import_song_loops(&import(document(&path, &sha256, TONE_FRAMES)))
        .await?;
    let other_file = client
        .import_song_loops(&import(document(&path, &"cd".repeat(32), TONE_FRAMES)))
        .await?;
    let elsewhere = client
        .import_song_loops(&import(document(
            &music.path().join("boss/other.wav"),
            &sha256,
            TONE_FRAMES,
        )))
        .await?;
    let other_length = client
        .import_song_loops(&import(document(&path, &sha256, TONE_FRAMES + 1)))
        .await?;

    assert_eq!(matching[0].outcome, SongLoopImportOutcome::Saved);
    assert_eq!(matching[0].audio_file_id, Some(song));
    assert_eq!(
        other_file[0].outcome,
        SongLoopImportOutcome::IdentityMismatch
    );
    assert_eq!(elsewhere[0].outcome, SongLoopImportOutcome::NoMatch);
    assert_eq!(
        other_length[0].outcome,
        SongLoopImportOutcome::LengthMismatch
    );
    server.stop().await
}

#[tokio::test]
async fn a_rewritten_file_suspends_its_loop_until_it_is_saved_again() -> anyhow::Result<()> {
    let Fixture {
        server,
        client,
        music,
        song,
    } = fixture().await?;
    let original = client.get_song_waveform(song).await?.fingerprint;
    client
        .update_song_loop(&full_update(song, &original, markers(0, 1_000, 7_000, 0)))
        .await?;

    write_wav(&music.path().join("boss/dragon.wav"), 660.0)?;
    client.rescan_library().await?;
    let suspended = saved_loop(&client, song).await?;
    let rewritten = client.get_song_waveform(song).await?.fingerprint;
    let preview_of_old_audio = client
        .preview_song_loop(&PreviewSongLoop {
            audio_file_id: song,
            source_fingerprint: original.clone(),
            draft: suspended
                .ok_or_else(|| anyhow::anyhow!("the loop vanished"))?
                .region,
            mode: PreviewMode::Seam(SeamPreviewSide::try_new(std::time::Duration::from_millis(
                100,
            ))?),
        })
        .await;
    client
        .update_song_loop(&UpdateSongLoop {
            audio_file_id: song,
            source_fingerprint: rewritten.clone(),
            playback_start: None,
            loop_start: None,
            loop_end: None,
            crossfade: None,
        })
        .await?;
    let reviewed = saved_loop(&client, song).await?;

    assert_ne!(original, rewritten);
    assert!(suspended.is_some_and(|saved| saved.needs_review));
    assert!(reviewed.is_some_and(|saved| !saved.needs_review));
    assert!(matches!(
        &preview_of_old_audio,
        Err(ClientError::Status(status))
            if error::reason(status).as_deref() == Some(PreviewSongLoopError::SOURCE_CHANGED)
    ));
    server.stop().await
}
