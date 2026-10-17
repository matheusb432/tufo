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
    num::NonZeroU64,
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
use tufo_models::library::song_loop::{LoopRegion, SourceFrames, nearest_frames};

use super::packed_frame::{PackedFrame, unpack_into};

const BUFFER_DURATION: Duration = Duration::from_secs(30);
const PREFILL_DURATION: Duration = Duration::from_millis(500);
const PREFILL_TIMEOUT: Duration = Duration::from_secs(2);
const PREFILL_POLL_INTERVAL: Duration = Duration::from_millis(2);

#[derive(Debug, thiserror::Error)]
pub enum OpenTrackError {
    #[error("the audio file does not exist")]
    NotFound,
    #[error("the audio file cannot be read")]
    Unreadable(#[source] io::Error),
    #[error("the file holds no audio track that can be decoded")]
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DecodeOptions {
    pub start_position: Duration,
    /// Wraps from the loop end to the loop start, the file's own bounds without a region, until
    /// turned off through [`TrackHandle::set_looping`].
    pub looping: bool,
    pub loop_region: Option<LoopRegion>,
}

/// A track whose decoder is running, split into the audio thread's reader and the control
/// side's handle.
pub struct BufferedTrack {
    pub reader: TrackReader,
    pub handle: TrackHandle,
}

impl BufferedTrack {
    /// Starts decoding `file` at `output_sample_rate` and returns once [`PREFILL_DURATION`] is
    /// buffered, while the decoder goes on to fill [`BUFFER_DURATION`]. Short files return at EOF;
    /// a slow decoder gets at most [`PREFILL_TIMEOUT`] to prefill. Blocks the calling thread.
    pub fn start(
        file: AudioFile,
        output_sample_rate: u32,
        options: DecodeOptions,
    ) -> Result<Self, OpenTrackError> {
        let duration = file.duration();
        let source_sample_rate = file.source_sample_rate();
        let loop_region = options.loop_region.filter(|region| {
            let saved_at_source_rate = region.timing().sample_rate.get() == source_sample_rate;
            if !saved_at_source_rate {
                tracing::warn!("ignoring a loop region saved for another sample rate");
            }
            saved_at_source_rate
        });
        let requested_start =
            SourceFrames(nearest_frames(options.start_position, source_sample_rate));
        let start = match loop_region {
            Some(region) if requested_start >= region.markers().loop_end => {
                region.markers().loop_start
            }
            _ => requested_start,
        };
        let pass_ends = match loop_region {
            Some(region) => PassEnds::Region(RegionPassEnds {
                region,
                start,
                output_sample_rate,
            }),
            None => PassEnds::WholeFile {
                first_pass: duration
                    .map(|duration| duration.saturating_sub(options.start_position))
                    .map(|remaining| frame_count(frames_in(remaining, output_sample_rate))),
                file: duration.and_then(|duration| {
                    NonZeroU64::new(frame_count(frames_in(duration, output_sample_rate)))
                }),
            },
        };
        let track_file = file.file().clone();
        let decoder = file
            .into_probed()
            .into_decoder(
                track_file,
                output_sample_rate,
                start,
                loop_region.map(|region| region.markers()),
            )
            .map_err(OpenTrackError::Unsupported)?;
        let (producer, consumer) = RingBuffer::new(frames_in(BUFFER_DURATION, output_sample_rate));
        let shared = Arc::new(TrackShared {
            looping: AtomicBool::new(options.looping),
            ..TrackShared::default()
        });
        let decoder_thread = thread::Builder::new()
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
            declared_frames: pass_ends.first(),
            underrun_frames_total: None,
        };
        reader.wait_for_prefill(frames_in(PREFILL_DURATION, output_sample_rate));
        Ok(Self {
            reader,
            handle: TrackHandle {
                shared,
                sample_rate: output_sample_rate,
                decoder_thread: decoder_thread.thread().clone(),
                pass_ends,
            },
        })
    }
}

/// The audio thread's end of a track: copies decoded frames without blocking or allocating.
pub struct TrackReader {
    consumer: Consumer<PackedFrame>,
    shared: Arc<TrackShared>,
    frames_played: u64,
    /// The frames left in the first pass, as the container or loop region declares its length.
    declared_frames: Option<u64>,
    underrun_frames_total: Option<Arc<AtomicU64>>,
}

impl TrackReader {
    /// Copies up to `out.len()` frames into the start of `out` and returns how many it copied.
    /// A short read while the decoder still runs counts as an underrun.
    pub fn read(&mut self, out: &mut [Frame]) -> usize {
        // Load the flag before counting frames: once it is set, the count includes every frame.
        let decoding_finished = self.decoding_finished();
        let wanted = out
            .len()
            .min(usize::try_from(self.frames_until_loop_end()).unwrap_or(usize::MAX));
        let count = self.consumer.slots().min(wanted);
        let copied = match self.consumer.read_chunk(count) {
            Ok(chunk) => {
                let (first, second) = chunk.as_slices();
                unpack_into(first, out);
                unpack_into(second, &mut out[first.len()..count]);
                chunk.commit_all();
                count
            }
            Err(_) => 0,
        };
        self.frames_played += frame_count(copied);
        self.shared
            .frames_played
            .store(self.frames_played, Ordering::Relaxed);
        if copied < wanted && !decoding_finished {
            let missing = frame_count(wanted - copied);
            self.shared
                .underrun_frames
                .fetch_add(missing, Ordering::Relaxed);
            if let Some(total) = &self.underrun_frames_total {
                total.fetch_add(missing, Ordering::Relaxed);
            }
        }
        copied
    }

    /// Frames decoded and waiting to be read.
    #[must_use]
    pub fn buffered_frames(&self) -> usize {
        self.consumer.slots()
    }

    pub(crate) fn count_underruns_in(&mut self, total: Arc<AtomicU64>) {
        self.underrun_frames_total = Some(total);
    }

    /// Whether playback ended, including a loop turned off with repeats still buffered.
    #[must_use]
    pub fn is_exhausted(&self) -> bool {
        self.frames_until_loop_end() == 0 || (self.decoding_finished() && self.consumer.is_empty())
    }

    /// Frames left to read: exact once decoding has finished, otherwise the container's
    /// declared length minus what played, or `None` when neither is known.
    #[must_use]
    pub fn remaining_frames(&self) -> Option<u64> {
        if self.shared.looping.load(Ordering::Relaxed)
            && !self.shared.decoder_stopped.load(Ordering::Acquire)
        {
            return None;
        }
        let frames_end = self.shared.playback_frames_end.load(Ordering::Acquire);
        if frames_end != u64::MAX {
            return Some(frames_end.saturating_sub(self.frames_played));
        }
        if self.shared.decoding_finished.load(Ordering::Acquire) {
            return Some(frame_count(self.consumer.slots()));
        }
        self.declared_frames
            .map(|frames| frames.saturating_sub(self.frames_played))
    }

    fn frames_until_loop_end(&self) -> u64 {
        self.shared
            .playback_frames_end
            .load(Ordering::Acquire)
            .saturating_sub(self.frames_played)
    }

    fn decoding_finished(&self) -> bool {
        self.shared.decoding_finished.load(Ordering::Acquire)
            && (!self.shared.looping.load(Ordering::Relaxed)
                || self.shared.decoder_stopped.load(Ordering::Acquire))
    }

    fn wait_for_prefill(&self, prefill_frames: usize) {
        let deadline = Instant::now() + PREFILL_TIMEOUT;
        while self.consumer.slots() < prefill_frames
            && !self.shared.decoding_finished.load(Ordering::Acquire)
            && Instant::now() < deadline
        {
            thread::sleep(PREFILL_POLL_INTERVAL);
        }
        if self.consumer.slots() < prefill_frames
            && !self.shared.decoding_finished.load(Ordering::Acquire)
        {
            tracing::warn!(
                buffered_frames = self.consumer.slots(),
                prefill_frames,
                "starting playback before the decoder filled its headroom"
            );
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
    decoder_thread: thread::Thread,
    pass_ends: PassEnds,
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

    /// Disabling ends the current pass where its length is known, discarding prefetched repeats.
    pub fn set_looping(&self, looping: bool) {
        if looping {
            self.shared
                .playback_frames_end
                .store(u64::MAX, Ordering::Release);
        } else if self.shared.looping.load(Ordering::Relaxed)
            && let Some(end) = self
                .pass_ends
                .at_or_after(self.shared.frames_played.load(Ordering::Relaxed))
        {
            self.shared
                .playback_frames_end
                .store(end, Ordering::Release);
        }
        self.shared.looping.store(looping, Ordering::Relaxed);
        self.decoder_thread.unpark();
    }
}

/// Output frames from the start position to the end of each pass of a looping track.
#[derive(Debug, Clone, Copy)]
enum PassEnds {
    /// Unknown when the container doesn't declare its length.
    WholeFile {
        first_pass: Option<u64>,
        file: Option<NonZeroU64>,
    },
    Region(RegionPassEnds),
}

impl PassEnds {
    fn first(&self) -> Option<u64> {
        match self {
            Self::WholeFile { first_pass, .. } => *first_pass,
            Self::Region(region) => Some(region.pass_end(0)),
        }
    }

    fn at_or_after(&self, played_frames: u64) -> Option<u64> {
        match *self {
            Self::WholeFile {
                first_pass: Some(first_pass),
                file: Some(file),
            } => {
                let passes = played_frames
                    .saturating_sub(first_pass)
                    .div_ceil(file.get());
                Some(first_pass.saturating_add(passes.saturating_mul(file.get())))
            }
            Self::WholeFile { .. } => None,
            Self::Region(region) => Some(region.at_or_after(played_frames)),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct RegionPassEnds {
    region: LoopRegion,
    start: SourceFrames,
    output_sample_rate: u32,
}

impl RegionPassEnds {
    /// Matches the resampler, which emits the floor of the frames its input covers.
    fn output_frames(&self, frames: SourceFrames) -> u64 {
        let output = u128::from(frames.0) * u128::from(self.output_sample_rate)
            / u128::from(self.region.timing().sample_rate.get());
        u64::try_from(output).unwrap_or(u64::MAX)
    }

    fn source_frames(&self, output_frames: u64) -> SourceFrames {
        let source = u128::from(output_frames) * u128::from(self.region.timing().sample_rate.get())
            / u128::from(self.output_sample_rate);
        SourceFrames(u64::try_from(source).unwrap_or(u64::MAX))
    }

    fn pass_end(&self, pass: u64) -> u64 {
        self.output_frames(self.region.frames_to_pass_end(self.start, pass))
    }

    fn at_or_after(&self, played_frames: u64) -> u64 {
        let past_first_pass = self
            .source_frames(played_frames)
            .saturating_sub(self.region.frames_to_pass_end(self.start, 0));
        let pass = past_first_pass.0.div_ceil(self.region.loop_length().0);
        let end = self.pass_end(pass);
        // Rounding between the rates can leave the reader a frame into the next pass.
        if end >= played_frames {
            end
        } else {
            self.pass_end(pass + 1)
        }
    }
}

#[derive(Debug)]
struct TrackShared {
    stop_requested: AtomicBool,
    looping: AtomicBool,
    /// Set after the decoder pushed its last frame, so a reader that observes it also observes
    /// every frame.
    decoding_finished: AtomicBool,
    decoder_stopped: AtomicBool,
    frames_played: AtomicU64,
    underrun_frames: AtomicU64,
    playback_frames_end: AtomicU64,
}

impl Default for TrackShared {
    fn default() -> Self {
        Self {
            stop_requested: AtomicBool::new(false),
            looping: AtomicBool::new(false),
            decoding_finished: AtomicBool::new(false),
            decoder_stopped: AtomicBool::new(false),
            frames_played: AtomicU64::new(0),
            underrun_frames: AtomicU64::new(0),
            playback_frames_end: AtomicU64::new(u64::MAX),
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn underrun_totals_outlive_readers_and_exclude_finished_decoders() {
        let total = Arc::new(AtomicU64::new(0));
        for expected in [3, 6] {
            let (mut producer, consumer) = RingBuffer::new(4);
            producer.push(PackedFrame::new(0.25, 0.25)).unwrap();
            let shared = Arc::new(TrackShared::default());
            let mut reader = TrackReader {
                consumer,
                shared: Arc::clone(&shared),
                frames_played: 0,
                declared_frames: None,
                underrun_frames_total: None,
            };
            reader.count_underruns_in(Arc::clone(&total));
            let mut out = [Frame::ZERO; 4];

            assert_eq!(reader.read(&mut out), 1);
            assert_eq!(out[0], Frame::new(0.25, 0.25));
            assert_eq!(shared.underrun_frames.load(Ordering::Relaxed), 3);
            shared.decoding_finished.store(true, Ordering::Release);
            if expected == 6 {
                shared.looping.store(true, Ordering::Relaxed);
                shared.decoder_stopped.store(true, Ordering::Release);
            }
            assert_eq!(reader.read(&mut out), 0);
            assert!(reader.is_exhausted());
            assert_eq!(reader.remaining_frames(), Some(0));
            drop(reader);

            assert_eq!(total.load(Ordering::Relaxed), expected);
        }
    }
}
