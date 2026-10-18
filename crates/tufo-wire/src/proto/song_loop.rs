//! Protobuf mappings for song loops.

use std::num::{NonZeroU32, NonZeroU64};

use tonic::Status;
use tufo_models::library::{
    AudioFileId, AudioFingerprint,
    song_loop::{LoopMarkers, LoopRegion, SeamPreviewSide, SourceFrames, SourceTiming},
};

use super::{
    InvalidMessage, duration_message, duration_value, invalid, parse_uuid, required, system_time,
    timestamp_message,
};
use crate::{
    pb,
    song_loops::{
        ImportSongLoops, PreviewId, PreviewMode, PreviewPlayback, PreviewSongLoop, PreviewStarted,
        SavedSongLoop, SongLoopImportOutcome, SongLoopImportResult, SongWaveform, UpdateSongLoop,
        UpdateSongLoopPreview,
    },
};

// Requests the server parses.

pub fn get_song_waveform_request(
    request: &pb::GetSongWaveformRequest,
) -> Result<AudioFileId, Status> {
    audio_file_id(&request.audio_file_id)
}

pub fn update_song_loop_request(
    request: pb::UpdateSongLoopRequest,
) -> Result<UpdateSongLoop, Status> {
    Ok(UpdateSongLoop {
        audio_file_id: audio_file_id(&request.audio_file_id)?,
        source_fingerprint: fingerprint("source_fingerprint", request.source_fingerprint)?,
        playback_start: request.playback_start_frame.map(SourceFrames),
        loop_start: request.loop_start_frame.map(SourceFrames),
        loop_end: request.loop_end_frame.map(SourceFrames),
        crossfade: request.crossfade_frames.map(SourceFrames),
    })
}

pub fn delete_song_loop_request(
    request: &pb::DeleteSongLoopRequest,
) -> Result<AudioFileId, Status> {
    audio_file_id(&request.audio_file_id)
}

#[must_use]
pub fn import_song_loops_request(request: pb::ImportSongLoopsRequest) -> ImportSongLoops {
    ImportSongLoops {
        document: request.document,
        dry_run: request.dry_run,
        replace_existing: request.replace_existing,
    }
}

pub fn preview_song_loop_request(
    request: pb::PreviewSongLoopRequest,
) -> Result<PreviewSongLoop, Status> {
    let mode = match required("mode", request.mode)? {
        pb::preview_song_loop_request::Mode::FromFrame(from_frame) => {
            PreviewMode::FromFrame(SourceFrames(from_frame.start_frame))
        }
        pb::preview_song_loop_request::Mode::Seam(seam) => {
            let side = required("seam.side", seam.side.and_then(duration_value))?;
            PreviewMode::Seam(
                SeamPreviewSide::try_new(side).map_err(|error| invalid("seam.side", error))?,
            )
        }
    };
    Ok(PreviewSongLoop {
        audio_file_id: audio_file_id(&request.audio_file_id)?,
        source_fingerprint: fingerprint("source_fingerprint", request.source_fingerprint)?,
        draft: loop_region(required("draft", request.draft)?)
            .map_err(|reason| invalid("draft", reason))?,
        mode,
    })
}

pub fn update_song_loop_preview_request(
    request: pb::UpdateSongLoopPreviewRequest,
) -> Result<UpdateSongLoopPreview, Status> {
    Ok(UpdateSongLoopPreview {
        preview_id: PreviewId(request.preview_id),
        draft: loop_region(required("draft", request.draft)?)
            .map_err(|reason| invalid("draft", reason))?,
    })
}

// Responses the server builds.

#[must_use]
pub fn get_song_waveform_response(waveform: SongWaveform) -> pb::GetSongWaveformResponse {
    pb::GetSongWaveformResponse {
        fingerprint: waveform.fingerprint.to_string(),
        sample_rate: waveform.timing.sample_rate.get(),
        frame_count: waveform.timing.frame_count.0,
        bucket_frames: waveform.bucket_frames.get(),
        peaks: waveform.peaks,
    }
}

#[must_use]
pub fn import_song_loops_response(results: &[SongLoopImportResult]) -> pb::ImportSongLoopsResponse {
    pb::ImportSongLoopsResponse {
        results: results
            .iter()
            .map(|result| pb::SongLoopImportResult {
                path: result.path.clone(),
                audio_file_id: result.audio_file_id.map(|id| id.to_string()),
                outcome: outcome_message(result.outcome).into(),
            })
            .collect(),
    }
}

#[must_use]
pub fn preview_song_loop_response(started: PreviewStarted) -> pb::PreviewSongLoopResponse {
    let (start_frame, seam_cycle) = match started.playback {
        PreviewPlayback::FromFrame(start) => (Some(start.0), None),
        PreviewPlayback::Seam { cycle } => (None, Some(duration_message(cycle))),
    };
    pb::PreviewSongLoopResponse {
        started_at: Some(timestamp_message(started.started_at)),
        seam_cycle,
        start_frame,
        draft: Some(song_loop_message(started.draft)),
        preview_id: started.preview_id.0,
    }
}

// Requests the client builds.

#[must_use]
pub fn get_song_waveform_message(id: AudioFileId) -> pb::GetSongWaveformRequest {
    pb::GetSongWaveformRequest {
        audio_file_id: id.to_string(),
    }
}

#[must_use]
pub fn update_song_loop_message(update: &UpdateSongLoop) -> pb::UpdateSongLoopRequest {
    pb::UpdateSongLoopRequest {
        audio_file_id: update.audio_file_id.to_string(),
        source_fingerprint: update.source_fingerprint.to_string(),
        playback_start_frame: update.playback_start.map(|frames| frames.0),
        loop_start_frame: update.loop_start.map(|frames| frames.0),
        loop_end_frame: update.loop_end.map(|frames| frames.0),
        crossfade_frames: update.crossfade.map(|frames| frames.0),
    }
}

#[must_use]
pub fn delete_song_loop_message(id: AudioFileId) -> pb::DeleteSongLoopRequest {
    pb::DeleteSongLoopRequest {
        audio_file_id: id.to_string(),
    }
}

#[must_use]
pub fn import_song_loops_message(request: &ImportSongLoops) -> pb::ImportSongLoopsRequest {
    pb::ImportSongLoopsRequest {
        document: request.document.clone(),
        dry_run: request.dry_run,
        replace_existing: request.replace_existing,
    }
}

#[must_use]
pub fn preview_song_loop_message(request: &PreviewSongLoop) -> pb::PreviewSongLoopRequest {
    let mode = match request.mode {
        PreviewMode::FromFrame(start) => {
            pb::preview_song_loop_request::Mode::FromFrame(pb::PreviewFromFrame {
                start_frame: start.0,
            })
        }
        PreviewMode::Seam(side) => pb::preview_song_loop_request::Mode::Seam(pb::PreviewSeam {
            side: Some(duration_message(side.into_inner())),
        }),
    };
    pb::PreviewSongLoopRequest {
        audio_file_id: request.audio_file_id.to_string(),
        source_fingerprint: request.source_fingerprint.to_string(),
        draft: Some(song_loop_message(request.draft)),
        mode: Some(mode),
    }
}

#[must_use]
pub fn update_song_loop_preview_message(
    request: UpdateSongLoopPreview,
) -> pb::UpdateSongLoopPreviewRequest {
    pb::UpdateSongLoopPreviewRequest {
        preview_id: request.preview_id.0,
        draft: Some(song_loop_message(request.draft)),
    }
}

// Responses the client decodes.

pub fn song_waveform(
    response: pb::GetSongWaveformResponse,
) -> Result<SongWaveform, InvalidMessage> {
    let invalid = |field| InvalidMessage { field };
    Ok(SongWaveform {
        fingerprint: AudioFingerprint::try_new(response.fingerprint)
            .map_err(|_| invalid("fingerprint"))?,
        timing: SourceTiming {
            sample_rate: NonZeroU32::new(response.sample_rate).ok_or(invalid("sample_rate"))?,
            frame_count: SourceFrames(response.frame_count),
        },
        bucket_frames: NonZeroU64::new(response.bucket_frames).ok_or(invalid("bucket_frames"))?,
        peaks: response.peaks,
    })
}

pub fn song_loop_import_results(
    response: pb::ImportSongLoopsResponse,
) -> Result<Vec<SongLoopImportResult>, InvalidMessage> {
    let invalid = |field| InvalidMessage { field };
    response
        .results
        .into_iter()
        .map(|result| {
            Ok(SongLoopImportResult {
                audio_file_id: result
                    .audio_file_id
                    .map(|id| {
                        parse_uuid(&id)
                            .map(AudioFileId::from_uuid)
                            .ok_or(invalid("audio_file_id"))
                    })
                    .transpose()?,
                outcome: outcome(result.outcome).ok_or(invalid("outcome"))?,
                path: result.path,
            })
        })
        .collect()
}

pub fn preview_started(
    response: pb::PreviewSongLoopResponse,
) -> Result<PreviewStarted, InvalidMessage> {
    let invalid = |field| InvalidMessage { field };
    let playback = match (response.start_frame, response.seam_cycle) {
        (Some(start), None) => PreviewPlayback::FromFrame(SourceFrames(start)),
        (None, Some(cycle)) => PreviewPlayback::Seam {
            cycle: duration_value(cycle).ok_or(invalid("seam_cycle"))?,
        },
        _ => return Err(invalid("start_frame")),
    };
    Ok(PreviewStarted {
        preview_id: PreviewId(response.preview_id),
        draft: response
            .draft
            .ok_or("is required")
            .and_then(loop_region)
            .map_err(|_| invalid("draft"))?,
        started_at: response
            .started_at
            .and_then(system_time)
            .ok_or(invalid("started_at"))?,
        playback,
    })
}

// Shared by the library and the player.

pub(super) fn song_loop_message(region: LoopRegion) -> pb::SongLoop {
    let timing = region.timing();
    let markers = region.markers();
    pb::SongLoop {
        sample_rate: timing.sample_rate.get(),
        frame_count: timing.frame_count.0,
        playback_start_frame: markers.playback_start.0,
        loop_start_frame: markers.loop_start.0,
        loop_end_frame: markers.loop_end.0,
        crossfade_frames: markers.crossfade.0,
    }
}

pub(super) fn loop_region(message: pb::SongLoop) -> Result<LoopRegion, &'static str> {
    let timing = SourceTiming {
        sample_rate: NonZeroU32::new(message.sample_rate).ok_or("sample_rate must be non-zero")?,
        frame_count: SourceFrames(message.frame_count),
    };
    let markers = LoopMarkers {
        playback_start: SourceFrames(message.playback_start_frame),
        loop_start: SourceFrames(message.loop_start_frame),
        loop_end: SourceFrames(message.loop_end_frame),
        crossfade: SourceFrames(message.crossfade_frames),
    };
    LoopRegion::try_new(timing, markers).map_err(|_| "the markers are out of order or range")
}

pub(super) fn saved_song_loop_message(saved: SavedSongLoop) -> pb::SavedSongLoop {
    pb::SavedSongLoop {
        markers: Some(song_loop_message(saved.region)),
        needs_review: saved.needs_review,
    }
}

pub(super) fn saved_song_loop(message: pb::SavedSongLoop) -> Result<SavedSongLoop, InvalidMessage> {
    let region = message
        .markers
        .ok_or("is required")
        .and_then(loop_region)
        .map_err(|_| InvalidMessage {
            field: "saved_loop",
        })?;
    Ok(SavedSongLoop {
        region,
        needs_review: message.needs_review,
    })
}

fn audio_file_id(text: &str) -> Result<AudioFileId, Status> {
    parse_uuid(text)
        .map(AudioFileId::from_uuid)
        .ok_or_else(|| invalid("audio_file_id", "must be a canonical UUID"))
}

fn fingerprint(field: &str, text: String) -> Result<AudioFingerprint, Status> {
    AudioFingerprint::try_new(text).map_err(|error| invalid(field, error))
}

fn outcome_message(outcome: SongLoopImportOutcome) -> pb::SongLoopImportOutcome {
    match outcome {
        SongLoopImportOutcome::Saved => pb::SongLoopImportOutcome::Saved,
        SongLoopImportOutcome::Replaced => pb::SongLoopImportOutcome::Replaced,
        SongLoopImportOutcome::Unchanged => pb::SongLoopImportOutcome::Unchanged,
        SongLoopImportOutcome::Conflict => pb::SongLoopImportOutcome::Conflict,
        SongLoopImportOutcome::NoMatch => pb::SongLoopImportOutcome::NoMatch,
        SongLoopImportOutcome::IdentityMismatch => pb::SongLoopImportOutcome::IdentityMismatch,
        SongLoopImportOutcome::LengthMismatch => pb::SongLoopImportOutcome::LengthMismatch,
        SongLoopImportOutcome::InvalidMarkers => pb::SongLoopImportOutcome::InvalidMarkers,
        SongLoopImportOutcome::Unreadable => pb::SongLoopImportOutcome::Unreadable,
    }
}

fn outcome(value: i32) -> Option<SongLoopImportOutcome> {
    Some(match pb::SongLoopImportOutcome::try_from(value).ok()? {
        pb::SongLoopImportOutcome::Unspecified => return None,
        pb::SongLoopImportOutcome::Saved => SongLoopImportOutcome::Saved,
        pb::SongLoopImportOutcome::Replaced => SongLoopImportOutcome::Replaced,
        pb::SongLoopImportOutcome::Unchanged => SongLoopImportOutcome::Unchanged,
        pb::SongLoopImportOutcome::Conflict => SongLoopImportOutcome::Conflict,
        pb::SongLoopImportOutcome::NoMatch => SongLoopImportOutcome::NoMatch,
        pb::SongLoopImportOutcome::IdentityMismatch => SongLoopImportOutcome::IdentityMismatch,
        pb::SongLoopImportOutcome::LengthMismatch => SongLoopImportOutcome::LengthMismatch,
        pb::SongLoopImportOutcome::InvalidMarkers => SongLoopImportOutcome::InvalidMarkers,
        pb::SongLoopImportOutcome::Unreadable => SongLoopImportOutcome::Unreadable,
    })
}
