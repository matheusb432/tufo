//! The engine's record of the music queue and the tracks it handed to the deck.

use std::{num::NonZeroU64, time::Duration};

use tufo_application::playback::player::PlayAudioError;
use tufo_models::{
    library::song_loop::LoopRegion,
    playback::{AudioFilePath, queue::MusicQueue},
};

use crate::audio::buffered_track::{
    AudioFile, BufferedTrack, DecodeOptions, OpenTrackError, TrackFile, TrackHandle,
};

/// The music the engine plays: its queue and the deck tracks behind it.
pub(super) struct Music {
    pub(super) queue: MusicQueue,
    pub(super) current: MusicTrack,
    /// The track the deck plays after the current one.
    pub(super) armed: Option<MusicTrack>,
    pub(super) paused: bool,
    /// A change is opening the next current track; the tick leaves the queue alone meanwhile.
    pub(super) changing: bool,
    /// An arm request for the current track is in flight or failed; the tick doesn't repeat it.
    pub(super) arm_requested: bool,
}

/// One track of the queue as the deck plays it.
pub(super) struct MusicTrack {
    pub(super) token: NonZeroU64,
    pub(super) file: TrackFile,
    /// Where decoding started.
    pub(super) start_offset: Duration,
    /// The decoded length when the container declares it, else the library's.
    pub(super) duration: Option<Duration>,
    pub(super) loop_region: Option<LoopRegion>,
    /// Absent while no output renders the track.
    pub(super) handle: Option<TrackHandle>,
}

impl MusicTrack {
    /// The source position, which wraps from the loop end to the loop start.
    pub(super) fn position(&self) -> Duration {
        match self.loop_region {
            Some(region) => region.position_after(self.start_offset, self.played()),
            None => self.elapsed(),
        }
    }

    /// Counts every pass, unlike [`Self::position`].
    pub(super) fn elapsed(&self) -> Duration {
        self.start_offset + self.played()
    }

    /// Where a pass that doesn't wrap ends.
    pub(super) fn end(&self) -> Option<Duration> {
        self.loop_region
            .map(|region| region.loop_end())
            .or(self.duration)
    }

    fn played(&self) -> Duration {
        self.handle
            .as_ref()
            .map_or(Duration::ZERO, TrackHandle::played)
    }

    pub(super) fn underrun_frames(&self) -> u64 {
        self.handle.as_ref().map_or(0, TrackHandle::underrun_frames)
    }
}

pub(super) fn start_position(loop_region: Option<LoopRegion>) -> Duration {
    loop_region.map_or(Duration::ZERO, |region| region.playback_start())
}

/// Where a track's file comes from.
pub(super) enum TrackSource {
    Path(AudioFilePath),
    /// A track already open, which restarts or seeks without looking up its path again.
    Open(TrackFile),
}

/// A track whose decoder is running, with what the engine keeps about it.
pub(super) struct OpenedTrack {
    pub(super) track: BufferedTrack,
    pub(super) file: TrackFile,
    pub(super) duration: Option<Duration>,
}

/// Opens and probes a track on a blocking thread, returning its file for [`open_track`].
pub(super) async fn probe_track(path: AudioFilePath) -> Result<TrackFile, PlayAudioError> {
    tokio::task::spawn_blocking(move || {
        AudioFile::open(path.as_ref()).map(|file| file.file().clone())
    })
    .await
    .map_err(|error| PlayAudioError::OutputUnavailable(error.into()))?
    .map_err(play_audio_error)
}

/// Opens, probes, and starts decoding a track on a blocking thread.
pub(super) async fn open_track(
    source: TrackSource,
    sample_rate: u32,
    options: DecodeOptions,
) -> Result<OpenedTrack, PlayAudioError> {
    tokio::task::spawn_blocking(move || {
        let file = match source {
            TrackSource::Path(path) => AudioFile::open(path.as_ref())?,
            TrackSource::Open(file) => AudioFile::reopen(file)?,
        };
        let track_file = file.file().clone();
        let duration = file.duration();
        let track = BufferedTrack::start(file, sample_rate, options)?;
        Ok(OpenedTrack {
            track,
            file: track_file,
            duration,
        })
    })
    .await
    .map_err(|error| PlayAudioError::OutputUnavailable(error.into()))?
    .map_err(play_audio_error)
}

pub(super) fn play_audio_error(error: OpenTrackError) -> PlayAudioError {
    match error {
        OpenTrackError::NotFound => PlayAudioError::NotFound,
        OpenTrackError::Unreadable(source) => PlayAudioError::Unreadable(source),
        OpenTrackError::Unsupported(source) => PlayAudioError::Unsupported(source),
        OpenTrackError::DecoderThread(source) => PlayAudioError::OutputUnavailable(source.into()),
    }
}
