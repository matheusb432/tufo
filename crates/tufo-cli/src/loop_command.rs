mod input;

use std::{collections::BTreeMap, time::Duration};

use anyhow::Context as _;
use clap::Subcommand;
use serde_json::{Value, json};
use tufo_client::{
    TufoClient,
    library::LibrarySnapshot,
    song_loops::{PreviewMode, PreviewPlayback, PreviewStarted, SavedSongLoop},
};
use tufo_models::library::AudioFileId;

use self::input::{
    AudioFileArgument, LoopPreview, LoopPreviewUpdate, LoopUpdate, PreviewIdArgument,
};
use crate::{api, record};

const PREVIEW_START_TIMEOUT: Duration = Duration::from_secs(150);

#[derive(Subcommand)]
pub(crate) enum LoopCommand {
    /// List saved loops with their audio file IDs, source timing, and review state.
    List,
    /// Read one saved loop by its audio file ID.
    Get {
        #[arg(value_name = "AUDIO_FILE_ID")]
        audio_file_id: AudioFileArgument,
    },
    /// Analyze a song and read its fingerprint, source timing, and waveform peaks.
    Waveform {
        #[arg(value_name = "AUDIO_FILE_ID")]
        audio_file_id: AudioFileArgument,
    },
    /// Save markers in source frames; omitted markers keep their saved values.
    #[command(after_long_help = r#"Example:
  tufo loop update AUDIO_FILE_ID '{"source_fingerprint":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","loop_start_frame":48000,"loop_end_frame":240000,"crossfade_frames":2400}'

Use fingerprint from `tufo loop waveform AUDIO_FILE_ID --json` as source_fingerprint.
Optional fields: playback_start_frame, loop_start_frame, loop_end_frame, crossfade_frames.
Without a saved loop, defaults cover the whole file with no crossfade.
Use zero to reset a start marker or remove the crossfade. Null is not a marker;
use `tufo loop delete AUDIO_FILE_ID` to clear the saved loop."#)]
    Update {
        #[arg(value_name = "AUDIO_FILE_ID")]
        audio_file_id: AudioFileArgument,
        #[arg(value_name = "JSON")]
        payload: LoopUpdate,
    },
    /// Remove a song's saved loop and playback start.
    Delete {
        #[arg(value_name = "AUDIO_FILE_ID")]
        audio_file_id: AudioFileArgument,
    },
    /// Audition a draft locally until its duration, Ctrl-C, or server end.
    #[command(after_long_help = r#"Examples:
  tufo loop preview AUDIO_FILE_ID '{"source_fingerprint":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","draft":{"timing":{"sample_rate":48000,"frame_count":480000},"playback_start_frame":0,"loop_start_frame":48000,"loop_end_frame":240000,"crossfade_frames":2400},"mode":{"kind":"from_frame","start_frame":0}}' --duration-ms 5000 --json
  tufo loop preview AUDIO_FILE_ID '{"source_fingerprint":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","draft":{"timing":{"sample_rate":48000,"frame_count":480000},"playback_start_frame":0,"loop_start_frame":48000,"loop_end_frame":240000,"crossfade_frames":2400},"mode":{"kind":"seam","side_ms":1000}}' --duration-ms 5000 --json

Source timing comes from `tufo loop waveform`. All markers and start_frame
count frames at that sample_rate. side_ms is 1..10000 milliseconds.
from_frame can start anywhere within the source file, including beyond the loop end.
The command emits started and ended records and releases playback on exit. The started record's
preview_id names the preview for `tufo loop update-preview`; an updated record follows each draft
that starts playing.
Preview uses the local device, including when music is routed to Discord."#)]
    Preview {
        #[arg(value_name = "AUDIO_FILE_ID")]
        audio_file_id: AudioFileArgument,
        #[arg(value_name = "JSON")]
        payload: LoopPreview,
        /// End after this many milliseconds of preview playback (at most one day).
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..=86_400_000))]
        duration_ms: Option<u64>,
    },
    /// Play a newer draft in a running preview from a frame, going on from where it plays.
    #[command(after_long_help = r#"Example:
  tufo loop update-preview PREVIEW_ID '{"draft":{"timing":{"sample_rate":48000,"frame_count":480000},"playback_start_frame":0,"loop_start_frame":96000,"loop_end_frame":192000,"crossfade_frames":2400}}'

PREVIEW_ID is the preview_id of the started record `tufo loop preview` emits. The draft keeps the
preview's timing. A preview that was wrapping and is past the new loop end goes to the loop start.
The preview command emits an updated record once the draft plays; seam previews keep their draft."#)]
    UpdatePreview {
        #[arg(value_name = "PREVIEW_ID")]
        preview_id: PreviewIdArgument,
        #[arg(value_name = "JSON")]
        payload: LoopPreviewUpdate,
    },
}

pub(crate) async fn run_loop(command: LoopCommand, json: bool) -> anyhow::Result<bool> {
    let client = api::connect().await?;
    match command {
        LoopCommand::List => {
            let library = api::library_snapshot(&client).await?;
            let loops = saved_loops(&library)
                .into_iter()
                .map(|(id, saved)| loop_record(id, saved))
                .collect::<Vec<_>>();
            api::emit(&Value::Array(loops), json).await?;
        }
        LoopCommand::Get { audio_file_id } => {
            let library = api::library_snapshot(&client).await?;
            let saved = saved_loops(&library)
                .get(&audio_file_id.0)
                .copied()
                .context("no saved loop exists for that audio file ID")?;
            api::emit(&loop_record(audio_file_id.0, saved), json).await?;
        }
        LoopCommand::Waveform { audio_file_id } => {
            let waveform = client.get_song_waveform(audio_file_id.0).await?;
            let mut waveform_record = record::waveform(&waveform);
            waveform_record["audio_file_id"] = json!(audio_file_id.0.to_string());
            api::emit(&waveform_record, json).await?;
        }
        LoopCommand::Update {
            audio_file_id,
            payload,
        } => {
            tufo_client::song_loop_update::execute(&client, &payload.into_request(audio_file_id.0))
                .await?;
            api::ack("updated", "loop", audio_file_id.0, json).await?;
        }
        LoopCommand::Delete { audio_file_id } => {
            client.delete_song_loop(audio_file_id.0).await?;
            api::ack("deleted", "loop", audio_file_id.0, json).await?;
        }
        LoopCommand::Preview {
            audio_file_id,
            payload,
            duration_ms,
        } => return preview(&client, audio_file_id.0, payload, duration_ms, json).await,
        LoopCommand::UpdatePreview {
            preview_id,
            payload,
        } => {
            client
                .update_song_loop_preview(payload.into_request(preview_id.0))
                .await?;
            api::ack("updated", "loop_preview", preview_id.0.0, json).await?;
        }
    }
    Ok(true)
}

fn saved_loops(library: &LibrarySnapshot) -> BTreeMap<AudioFileId, &SavedSongLoop> {
    library.saved_song_loops().collect()
}

fn loop_record(id: AudioFileId, saved: &SavedSongLoop) -> Value {
    json!({
        "audio_file_id": id.to_string(),
        "saved_loop": record::saved_loop(saved),
    })
}

async fn preview(
    client: &TufoClient,
    id: AudioFileId,
    payload: LoopPreview,
    duration_ms: Option<u64>,
    json: bool,
) -> anyhow::Result<bool> {
    let request = payload.into_request(id);
    let interrupt = tokio::signal::ctrl_c();
    tokio::pin!(interrupt);
    let mut preview = tokio::select! {
        interrupted = &mut interrupt => {
            interrupted?;
            return Ok(true);
        }
        started = tokio::time::timeout(PREVIEW_START_TIMEOUT, client.preview_song_loop(&request)) => {
            started.context("timed out waiting for the loop preview to start")??
        }
    };
    let elapsed = tokio::time::Instant::now();
    let started_at_unix_ms = unix_ms(preview.started.started_at)?;
    let seam_cycle_ms = match preview.started.playback {
        PreviewPlayback::Seam { cycle } => Some(cycle.as_millis()),
        PreviewPlayback::FromFrame(_) => None,
    };
    let mode = match request.mode {
        PreviewMode::FromFrame(start) => json!({"kind": "from_frame", "start_frame": start.0}),
        PreviewMode::Seam(side) => {
            json!({"kind": "seam", "side_ms": side.into_inner().as_millis()})
        }
    };
    let started = json!({
        "event": "started",
        "resource": "loop_preview",
        "preview_id": preview.started.preview_id.0,
        "audio_file_id": id.to_string(),
        "source_fingerprint": request.source_fingerprint.as_ref(),
        "draft": record::loop_region(&request.draft),
        "mode": mode,
        "started_at_unix_ms": started_at_unix_ms,
        "seam_cycle_ms": seam_cycle_ms,
        "duration_ms": duration_ms,
    });
    let deadline = duration_ms.map(|milliseconds| elapsed + Duration::from_millis(milliseconds));
    let emitted = match deadline {
        Some(deadline) => {
            match tokio::time::timeout_at(deadline, api::emit_stream(&started, json)).await {
                Ok(emitted) => emitted?,
                Err(_) => return Ok(true),
            }
        }
        None => api::emit_stream(&started, json).await?,
    };
    if !emitted {
        return Ok(true);
    }
    let duration_end = async {
        if let Some(deadline) = deadline {
            tokio::time::sleep_until(deadline).await;
        } else {
            std::future::pending::<()>().await;
        }
    };
    tokio::pin!(duration_end);
    let stdout_closed = api::stdout_closed();
    tokio::pin!(stdout_closed);
    let reason = loop {
        tokio::select! {
            interrupted = &mut interrupt => {
                interrupted?;
                break "cancelled";
            }
            () = &mut duration_end => break "duration",
            result = &mut stdout_closed => {
                result?;
                return Ok(true);
            }
            started = preview.next_started() => match started {
                Some(started) => {
                    let record = updated_record(id, &started?)?;
                    let emitted = tokio::select! {
                        () = &mut duration_end => return Ok(true),
                        result = api::emit_stream(&record, json) => result?,
                    };
                    if !emitted {
                        return Ok(true);
                    }
                }
                None => break "server",
            },
        }
    };
    drop(preview);
    api::emit_stream(
        &json!({
            "event": "ended",
            "resource": "loop_preview",
            "audio_file_id": id.to_string(),
            "reason": reason,
            "elapsed_ms": elapsed.elapsed().as_millis(),
        }),
        json,
    )
    .await?;
    Ok(true)
}

fn updated_record(id: AudioFileId, started: &PreviewStarted) -> anyhow::Result<Value> {
    let start_frame = match started.playback {
        PreviewPlayback::FromFrame(start) => Some(start.0),
        PreviewPlayback::Seam { .. } => None,
    };
    Ok(json!({
        "event": "updated",
        "resource": "loop_preview",
        "preview_id": started.preview_id.0,
        "audio_file_id": id.to_string(),
        "draft": record::loop_region(&started.draft),
        "start_frame": start_frame,
        "started_at_unix_ms": unix_ms(started.started_at)?,
    }))
}

fn unix_ms(time: std::time::SystemTime) -> anyhow::Result<u64> {
    Ok(u64::try_from(
        time.duration_since(std::time::UNIX_EPOCH)?.as_millis(),
    )?)
}
