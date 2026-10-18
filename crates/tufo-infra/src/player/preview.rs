//! Auditioning a draft loop on the local device, apart from the session's output.

use std::{
    num::NonZeroU64,
    sync::Arc,
    time::{Duration, SystemTime},
};

use tokio::sync::{oneshot, watch};
use tufo_application::playback::{
    player::{PlayAudioError, PreviewSession, SongPreview},
    update_song_loop_preview::UpdateSongLoopPreviewError,
};
use tufo_models::{
    library::song_loop::{LoopMarkers, LoopRegion, SourceFrames},
    playback::{AudioFilePath, OutputTarget},
};
use tufo_wire::song_loops::{PreviewId, PreviewMode, PreviewPlayback, PreviewStarted};

use super::{
    ANTI_CLICK_FADE, Engine, EnginePlayer,
    music::play_audio_error,
    outputs::{AudioOutputs, OpenedOutput},
};
use crate::audio::{
    Mixer, MixerVolumes,
    buffered_track::{AudioFile, BufferedTrack, DecodeOptions, OpenTrackError, TrackHandle},
    decibels,
    music_deck::DeckTrack,
    seam::{SeamClip, render_seam_clip},
};

pub(super) struct Preview {
    id: PreviewId,
    output: OpenedOutput,
    /// Cleared when someone pauses or resumes the music during the preview.
    pub(super) resumes_music: bool,
    /// Absent for seam clips, which keep the draft they rendered.
    track: Option<PreviewTrack>,
    started: watch::Sender<PreviewStarted>,
    /// Dropping it ends the session's `ended`.
    _ended: oneshot::Sender<()>,
}

/// A preview from a frame, which takes newer drafts while it plays.
struct PreviewTrack {
    path: AudioFilePath,
    draft: LoopRegion,
    start: SourceFrames,
    handle: TrackHandle,
    /// The latest draft no track plays yet.
    pending_draft: Option<LoopRegion>,
    /// Set while a replacement decodes; drafts arriving meanwhile wait for it.
    replacing: bool,
}

impl PreviewTrack {
    fn wraps(&self) -> bool {
        self.start < self.draft.markers().loop_end
    }

    fn frame(&self) -> SourceFrames {
        let played = self.draft.timing().frames_in(self.handle.played());
        self.draft.preview_frame_after(self.start, played)
    }
}

/// A track decoding for a newer draft, lined up with where the preview was when it began.
struct TrackReplacement {
    preview_id: PreviewId,
    path: AudioFilePath,
    draft: LoopRegion,
    start: SourceFrames,
    sampled_at: SystemTime,
    played_frames_at_sample: u64,
    sample_rate: u32,
}

enum PreviewAudio {
    Track {
        track: BufferedTrack,
        start: SourceFrames,
    },
    SeamClip(Box<SeamClip>),
}

enum StartedAudio {
    Track {
        handle: TrackHandle,
        start: SourceFrames,
    },
    SeamClip {
        cycle: Duration,
    },
}

impl PreviewAudio {
    fn start_on(self, mixer: &mut Mixer, gain: f32) -> Result<StartedAudio, PlayAudioError> {
        let unavailable = |error: anyhow::Error| PlayAudioError::OutputUnavailable(error);
        match self {
            Self::Track { track, start } => {
                mixer.set_music_volume(gain);
                mixer
                    .music()
                    .start(
                        DeckTrack {
                            token: NonZeroU64::MIN,
                            reader: track.reader,
                        },
                        ANTI_CLICK_FADE,
                        Duration::ZERO,
                    )
                    .map_err(|error| unavailable(error.into()))?;
                Ok(StartedAudio::Track {
                    handle: track.handle,
                    start,
                })
            }
            Self::SeamClip(clip) => {
                mixer
                    .play_static_sound(clip.sound.volume(decibels(gain)))
                    .map_err(|error| unavailable(error.into()))?;
                Ok(StartedAudio::SeamClip { cycle: clip.cycle })
            }
        }
    }
}

impl<O: AudioOutputs> EnginePlayer<O> {
    pub(super) async fn begin_preview(
        &self,
        preview: SongPreview,
    ) -> Result<PreviewSession, PlayAudioError> {
        let mut output = self
            .outputs
            .open(OutputTarget::Local, MixerVolumes::default())
            .await
            .map_err(|error| PlayAudioError::OutputUnavailable(error.into()))?;
        let SongPreview { path, draft, mode } = preview;
        let audio = prepare_audio(path.clone(), draft, mode, output.mixer.sample_rate()).await?;
        let mut engine = self.engine.lock().await;
        let started_audio = audio.start_on(&mut output.mixer, engine.volume.into_inner())?;
        let resumes_music = match engine.preview.take() {
            Some(previous) => {
                self.outputs.close(previous.output).await;
                previous.resumes_music
            }
            None => engine.pause_local_music_for_preview()?,
        };
        engine.last_preview_id += 1;
        let id = PreviewId(engine.last_preview_id);
        let (playback, track) = match started_audio {
            StartedAudio::Track { handle, start } => (
                PreviewPlayback::FromFrame(start),
                Some(PreviewTrack {
                    path,
                    draft,
                    start,
                    handle,
                    pending_draft: None,
                    replacing: false,
                }),
            ),
            StartedAudio::SeamClip { cycle } => (PreviewPlayback::Seam { cycle }, None),
        };
        let (started_sender, started) = watch::channel(PreviewStarted {
            preview_id: id,
            draft,
            started_at: SystemTime::now(),
            playback,
        });
        let (ended_sender, ended) = oneshot::channel();
        engine.preview = Some(Preview {
            id,
            output,
            resumes_music,
            track,
            started: started_sender,
            _ended: ended_sender,
        });
        self.publish(&engine);
        Ok(PreviewSession { id, started, ended })
    }

    /// Holds `draft` for the tick, which replaces the preview's track once no replacement is
    /// decoding, so a draft dragged across the waveform decodes at most one track at a time.
    pub(super) async fn queue_preview_draft(
        &self,
        id: PreviewId,
        draft: LoopRegion,
    ) -> Result<(), UpdateSongLoopPreviewError> {
        let mut engine = self.engine.lock().await;
        let track = engine
            .preview
            .as_mut()
            .filter(|preview| preview.id == id)
            .ok_or(UpdateSongLoopPreviewError::Ended)?
            .track
            .as_mut()
            .ok_or(UpdateSongLoopPreviewError::Seam)?;
        if draft.timing() != track.draft.timing() {
            return Err(UpdateSongLoopPreviewError::OtherAudio);
        }
        track.pending_draft = Some(draft);
        Ok(())
    }

    /// Runs on the tick. A draft that moves only the playback start needs no new track.
    pub(super) fn replace_preview_track_if_due(self: &Arc<Self>, engine: &mut Engine) {
        let Some(preview) = engine.preview.as_mut() else {
            return;
        };
        preview.output.mixer.music().drop_retired();
        let sample_rate = preview.output.mixer.sample_rate();
        let Some(track) = preview.track.as_mut().filter(|track| !track.replacing) else {
            return;
        };
        let Some(draft) = track.pending_draft.take() else {
            return;
        };
        if wraps_alike(draft.markers(), track.draft.markers()) {
            track.draft = draft;
            preview.started.send_modify(|started| started.draft = draft);
            return;
        }
        track.replacing = true;
        let replacement = TrackReplacement {
            preview_id: preview.id,
            path: track.path.clone(),
            draft,
            start: draft.preview_start_continuing(track.frame(), track.wraps()),
            sampled_at: SystemTime::now(),
            played_frames_at_sample: track.handle.played_frames(),
            sample_rate,
        };
        tokio::spawn(Arc::clone(self).replace_preview_track(replacement));
    }

    async fn replace_preview_track(self: Arc<Self>, replacement: TrackReplacement) {
        let TrackReplacement {
            preview_id,
            path,
            draft,
            start,
            sampled_at,
            played_frames_at_sample,
            sample_rate,
        } = replacement;
        let decoded =
            tokio::task::spawn_blocking(move || start_track_from(&path, draft, start, sample_rate))
                .await;
        let mut engine = self.engine.lock().await;
        let Some(preview) = engine
            .preview
            .as_mut()
            .filter(|preview| preview.id == preview_id)
        else {
            return;
        };
        let Some(track) = preview.track.as_mut() else {
            return;
        };
        track.replacing = false;
        let mut decoded = match decoded {
            Ok(Ok(decoded)) => decoded,
            Ok(Err(error)) => {
                tracing::warn!(%error, "the preview cannot play the updated draft");
                return;
            }
            Err(error) => {
                tracing::warn!(%error, "the preview's updated draft stopped decoding");
                return;
            }
        };
        let played_since_sample = track
            .handle
            .played_frames()
            .saturating_sub(played_frames_at_sample);
        decoded.reader.skip(played_since_sample);
        let deck_track = DeckTrack {
            token: NonZeroU64::MIN,
            reader: decoded.reader,
        };
        if let Err(error) =
            preview
                .output
                .mixer
                .music()
                .start(deck_track, ANTI_CLICK_FADE, ANTI_CLICK_FADE)
        {
            tracing::warn!(%error, "the preview cannot switch to the updated draft");
            return;
        }
        track.draft = draft;
        track.start = start;
        track.handle = decoded.handle;
        preview.started.send_replace(PreviewStarted {
            preview_id,
            draft,
            started_at: sampled_at,
            playback: PreviewPlayback::FromFrame(start),
        });
    }

    /// Leaves paused music paused, for stand-by and shutdown.
    pub(super) async fn discard_preview(&self, engine: &mut Engine) {
        if let Some(preview) = engine.preview.take() {
            self.outputs.close(preview.output).await;
        }
    }

    pub(super) async fn end_preview(&self, id: PreviewId) {
        let mut engine = self.engine.lock().await;
        if engine
            .preview
            .as_ref()
            .is_none_or(|preview| preview.id != id)
        {
            return;
        }
        let Some(preview) = engine.preview.take() else {
            return;
        };
        self.outputs.close(preview.output).await;
        if preview.resumes_music
            && engine.music_is_paused()
            && let Err(error) = engine.set_paused(false)
        {
            tracing::warn!(%error, "the music cannot resume after the preview");
        }
        self.publish(&engine);
    }
}

impl Engine {
    /// Reports whether it paused anything.
    fn pause_local_music_for_preview(&mut self) -> Result<bool, PlayAudioError> {
        let pauses_music = self.target == OutputTarget::Local && self.music_is_playing();
        if pauses_music {
            self.set_paused(true)
                .map_err(|error| PlayAudioError::OutputUnavailable(error.into()))?;
        }
        Ok(pauses_music)
    }
}

/// Only the loop and its crossfade shape what a preview from a frame plays.
fn wraps_alike(left: LoopMarkers, right: LoopMarkers) -> bool {
    (left.loop_start, left.loop_end, left.crossfade)
        == (right.loop_start, right.loop_end, right.crossfade)
}

async fn prepare_audio(
    path: AudioFilePath,
    draft: LoopRegion,
    mode: PreviewMode,
    sample_rate: u32,
) -> Result<PreviewAudio, PlayAudioError> {
    tokio::task::spawn_blocking(move || match mode {
        PreviewMode::FromFrame(start) => start_track_from(&path, draft, start, sample_rate)
            .map(|track| PreviewAudio::Track { track, start }),
        PreviewMode::Seam(side) => render_seam_clip(path.as_ref(), draft, side)
            .map(|clip| PreviewAudio::SeamClip(Box::new(clip))),
    })
    .await
    .map_err(|error| PlayAudioError::OutputUnavailable(error.into()))?
    .map_err(play_audio_error)
}

/// A start past the draft's loop end plays through to the file's end, for listening beyond it.
fn start_track_from(
    path: &AudioFilePath,
    draft: LoopRegion,
    start: SourceFrames,
    sample_rate: u32,
) -> Result<BufferedTrack, OpenTrackError> {
    let wraps = start < draft.markers().loop_end;
    let options = DecodeOptions {
        start_position: draft.timing().duration_of(start),
        looping: wraps,
        loop_region: wraps.then_some(draft),
    };
    BufferedTrack::start(AudioFile::open(path.as_ref())?, sample_rate, options)
}
