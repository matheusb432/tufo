//! Auditioning a draft loop on the local device, apart from the session's output.

use std::{
    num::NonZeroU64,
    time::{Duration, SystemTime},
};

use tokio::sync::oneshot;
use tufo_application::playback::player::{PlayAudioError, PreviewId, PreviewSession, SongPreview};
use tufo_models::playback::OutputTarget;
use tufo_wire::song_loops::{PreviewMode, PreviewStarted};

use super::{
    ANTI_CLICK_FADE, Engine, EnginePlayer,
    music::play_audio_error,
    outputs::{AudioOutputs, OpenedOutput},
};
use crate::audio::{
    Mixer, MixerVolumes,
    buffered_track::{AudioFile, BufferedTrack, DecodeOptions},
    decibels,
    music_deck::DeckTrack,
    seam::{SeamClip, render_seam_clip},
};

pub(super) struct Preview {
    id: PreviewId,
    output: OpenedOutput,
    /// Cleared when someone pauses or resumes the music during the preview.
    pub(super) resumes_music: bool,
    /// Dropping it ends the session's `ended`.
    _ended: oneshot::Sender<()>,
}

enum PreviewPlayback {
    Track(BufferedTrack),
    SeamClip(Box<SeamClip>),
}

impl PreviewPlayback {
    fn start_on(self, mixer: &mut Mixer, gain: f32) -> Result<Option<Duration>, PlayAudioError> {
        let unavailable = |error: anyhow::Error| PlayAudioError::OutputUnavailable(error);
        match self {
            Self::Track(track) => {
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
                Ok(None)
            }
            Self::SeamClip(clip) => {
                mixer
                    .play_static_sound(clip.sound.volume(decibels(gain)))
                    .map_err(|error| unavailable(error.into()))?;
                Ok(Some(clip.cycle))
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
        let playback = prepare_playback(preview, output.mixer.sample_rate()).await?;
        let mut engine = self.engine.lock().await;
        let seam_cycle = playback.start_on(&mut output.mixer, engine.volume.into_inner())?;
        let resumes_music = match engine.preview.take() {
            Some(previous) => {
                self.outputs.close(previous.output).await;
                previous.resumes_music
            }
            None => engine.pause_local_music_for_preview()?,
        };
        engine.last_preview_id += 1;
        let id = PreviewId(engine.last_preview_id);
        let (ended_sender, ended) = oneshot::channel();
        engine.preview = Some(Preview {
            id,
            output,
            resumes_music,
            _ended: ended_sender,
        });
        self.publish(&engine);
        Ok(PreviewSession {
            id,
            started: PreviewStarted {
                started_at: SystemTime::now(),
                seam_cycle,
            },
            ended,
        })
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

/// A start past the draft's loop end plays through to the file's end, for listening beyond it.
async fn prepare_playback(
    preview: SongPreview,
    sample_rate: u32,
) -> Result<PreviewPlayback, PlayAudioError> {
    tokio::task::spawn_blocking(move || {
        let SongPreview { path, draft, mode } = preview;
        match mode {
            PreviewMode::FromFrame(start) => {
                let wraps = start < draft.markers().loop_end;
                let options = DecodeOptions {
                    start_position: draft.timing().duration_of(start),
                    looping: wraps,
                    loop_region: wraps.then_some(draft),
                };
                BufferedTrack::start(AudioFile::open(path.as_ref())?, sample_rate, options)
                    .map(PreviewPlayback::Track)
            }
            PreviewMode::Seam(side) => render_seam_clip(path.as_ref(), draft, side)
                .map(|clip| PreviewPlayback::SeamClip(Box::new(clip))),
        }
    })
    .await
    .map_err(|error| PlayAudioError::OutputUnavailable(error.into()))?
    .map_err(play_audio_error)
}
