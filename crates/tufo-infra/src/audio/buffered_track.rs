//! Audio that decodes ahead of the audio thread.
//!
//! A dedicated thread decodes a file as it reads it and resamples it to the output rate, staying
//! up to [`BUFFER_DURATION`] ahead of playback. The audio thread only copies frames out of a
//! wait-free ring buffer through a [`TrackReader`], so disk stalls never reach it, and a starved
//! decoder produces silence instead of a tempo change.

mod decoder;
mod track_file;

use std::{
    io,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use kira::Frame;
use rtrb::{Consumer, RingBuffer};
pub use track_file::TrackFile;

const BUFFER_DURATION: Duration = Duration::from_secs(5);
const PREFILL_DURATION: Duration = Duration::from_millis(250);
const PREFILL_TIMEOUT: Duration = Duration::from_secs(2);
const PREFILL_POLL_INTERVAL: Duration = Duration::from_millis(2);

#[derive(Debug, thiserror::Error)]
pub enum OpenTrackError {
    #[error("the audio file does not exist")]
    NotFound,
    #[error("the audio file cannot be read")]
    Unreadable(#[source] io::Error),
    #[error("the file holds no audio track Tufo can decode")]
    Unsupported(#[source] anyhow::Error),
    #[error("the decoder thread cannot start")]
    DecoderThread(#[source] io::Error),
}

/// An open audio file, probed and ready to decode at any output rate and position.
pub struct AudioFile {
    file: TrackFile,
    probed: decoder::ProbedTrack,
}

impl AudioFile {
    /// Opens `path` and probes its container. Blocks the calling thread.
    pub fn open(path: &Path) -> Result<Self, OpenTrackError> {
        let file = TrackFile::open(path).map_err(|error| match error.kind() {
            io::ErrorKind::NotFound => OpenTrackError::NotFound,
            _ => OpenTrackError::Unreadable(error),
        })?;
        Self::reopen(file)
    }

    /// Probes a file already open, such as another [`AudioFile`]'s, from its start. Blocks the
    /// calling thread.
    pub fn reopen(file: TrackFile) -> Result<Self, OpenTrackError> {
        let probed =
            decoder::ProbedTrack::probe(file.reader()).map_err(OpenTrackError::Unsupported)?;
        Ok(Self { file, probed })
    }

    /// The open file, which reopens the track without looking up its path again.
    #[must_use]
    pub fn file(&self) -> &TrackFile {
        &self.file
    }

    /// The length the container declares, when it declares one.
    #[must_use]
    pub fn duration(&self) -> Option<Duration> {
        self.probed.duration()
    }

    pub(crate) fn source_sample_rate(&self) -> u32 {
        self.probed.source_sample_rate()
    }

    pub(crate) fn into_probed(self) -> decoder::ProbedTrack {
        self.probed
    }
}

/// Where decoding starts and whether it wraps around at the end.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DecodeOptions {
    pub start_position: Duration,
    /// Restart from the beginning at the end of the file, until turned off through
    /// [`TrackHandle::set_looping`].
    pub looping: bool,
}

/// A track whose decoder is running, split into the audio thread's reader and the control
/// side's handle.
pub struct BufferedTrack {
    pub reader: TrackReader,
    pub handle: TrackHandle,
}

impl BufferedTrack {
    /// Starts decoding `file` at `output_sample_rate` and returns once [`PREFILL_DURATION`] of
    /// audio is buffered or decoding has ended. Blocks the calling thread.
    pub fn start(
        file: AudioFile,
        output_sample_rate: u32,
        options: DecodeOptions,
    ) -> Result<Self, OpenTrackError> {
        let declared_frames = file
            .duration()
            .map(|duration| duration.saturating_sub(options.start_position))
            .map(|remaining| frames_in(remaining, output_sample_rate));
        let track_file = file.file().clone();
        let decoder = file
            .into_probed()
            .into_decoder(track_file, output_sample_rate, options.start_position)
            .map_err(OpenTrackError::Unsupported)?;
        let (producer, consumer) = RingBuffer::new(frames_in(BUFFER_DURATION, output_sample_rate));
        let shared = Arc::new(TrackShared {
            looping: AtomicBool::new(options.looping),
            ..TrackShared::default()
        });
        thread::Builder::new()
            .name("tufo-track-decoder".to_owned())
            .spawn({
                let shared = Arc::clone(&shared);
                move || decoder.run(producer, &shared)
            })
            .map_err(OpenTrackError::DecoderThread)?;

        let reader = TrackReader {
            consumer,
            shared: Arc::clone(&shared),
            frames_played: 0,
            declared_frames: declared_frames.map(frame_count),
        };
        reader.wait_for_prefill(frames_in(PREFILL_DURATION, output_sample_rate));
        Ok(Self {
            reader,
            handle: TrackHandle {
                shared,
                sample_rate: output_sample_rate,
            },
        })
    }
}

/// The audio thread's end of a track: copies decoded frames without blocking or allocating.
pub struct TrackReader {
    consumer: Consumer<Frame>,
    shared: Arc<TrackShared>,
    frames_played: u64,
    /// The frames left from the start position, as the container declares its length.
    declared_frames: Option<u64>,
}

impl TrackReader {
    /// Copies up to `out.len()` frames into the start of `out` and returns how many it copied.
    /// A short read while the decoder still runs counts as an underrun.
    pub fn read(&mut self, out: &mut [Frame]) -> usize {
        // Load the flag before counting frames: once it is set, the count includes every frame.
        let decoding_finished = self.shared.decoding_finished.load(Ordering::Acquire);
        let count = self.consumer.slots().min(out.len());
        let copied = match self.consumer.read_chunk(count) {
            Ok(chunk) => {
                let (first, second) = chunk.as_slices();
                out[..first.len()].copy_from_slice(first);
                out[first.len()..count].copy_from_slice(second);
                chunk.commit_all();
                count
            }
            Err(_) => 0,
        };
        self.frames_played += frame_count(copied);
        self.shared
            .frames_played
            .store(self.frames_played, Ordering::Relaxed);
        if copied < out.len() && !decoding_finished {
            self.shared
                .underrun_frames
                .fetch_add(frame_count(out.len() - copied), Ordering::Relaxed);
        }
        copied
    }

    /// Whether every decoded frame has been read and the decoder has stopped.
    #[must_use]
    pub fn is_exhausted(&self) -> bool {
        self.shared.decoding_finished.load(Ordering::Acquire) && self.consumer.is_empty()
    }

    /// Frames left to read: exact once decoding has finished, otherwise the container's
    /// declared length minus what played, or `None` when neither is known.
    #[must_use]
    pub fn remaining_frames(&self) -> Option<u64> {
        if self.shared.decoding_finished.load(Ordering::Acquire) {
            return Some(frame_count(self.consumer.slots()));
        }
        if self.shared.looping.load(Ordering::Relaxed) {
            return None;
        }
        self.declared_frames
            .map(|frames| frames.saturating_sub(self.frames_played))
    }

    fn wait_for_prefill(&self, prefill_frames: usize) {
        let deadline = Instant::now() + PREFILL_TIMEOUT;
        while self.consumer.slots() < prefill_frames
            && !self.shared.decoding_finished.load(Ordering::Acquire)
            && Instant::now() < deadline
        {
            thread::sleep(PREFILL_POLL_INTERVAL);
        }
    }
}

impl Drop for TrackReader {
    /// Stops the decoder even if it is waiting for room in the ring buffer.
    fn drop(&mut self) {
        self.shared.stop_requested.store(true, Ordering::Relaxed);
    }
}

/// The control side's view of a track the audio thread reads.
#[derive(Debug, Clone)]
pub struct TrackHandle {
    shared: Arc<TrackShared>,
    sample_rate: u32,
}

impl TrackHandle {
    /// How much audio the reader has copied out, from the start position.
    #[must_use]
    pub fn played(&self) -> Duration {
        duration_of_frames(
            self.shared.frames_played.load(Ordering::Relaxed),
            self.sample_rate,
        )
    }

    /// Frames of silence played because the decoder fell behind.
    #[must_use]
    pub fn underrun_frames(&self) -> u64 {
        self.shared.underrun_frames.load(Ordering::Relaxed)
    }

    /// Changes whether the decoder wraps around at the end of the file.
    pub fn set_looping(&self, looping: bool) {
        self.shared.looping.store(looping, Ordering::Relaxed);
    }
}

#[derive(Debug, Default)]
struct TrackShared {
    stop_requested: AtomicBool,
    looping: AtomicBool,
    /// Set after the decoder pushed its last frame, so a reader that observes it also observes
    /// every frame.
    decoding_finished: AtomicBool,
    frames_played: AtomicU64,
    underrun_frames: AtomicU64,
}

pub(crate) fn frames_in(duration: Duration, sample_rate: u32) -> usize {
    let frames = u128::from(sample_rate) * duration.as_nanos() / 1_000_000_000;
    usize::try_from(frames).unwrap_or(usize::MAX)
}

#[expect(
    clippy::cast_precision_loss,
    reason = "frame counts past 2^52 are centuries of audio"
)]
pub(crate) fn duration_of_frames(frames: u64, sample_rate: u32) -> Duration {
    Duration::from_secs_f64(frames as f64 / f64::from(sample_rate))
}

/// Widens a frame count; `usize` never exceeds `u64` on supported targets.
pub(crate) fn frame_count(frames: usize) -> u64 {
    u64::try_from(frames).unwrap_or(u64::MAX)
}
