//! Live audio from the browser's pages inside a mixer: one kira sound that sums every page's
//! stream from wait-free rings.
//!
//! Pages and the output keep separate clocks, so each stream plays behind a small cushion. A
//! stream that runs dry plays silence until its cushion refills; one that runs ahead past the
//! latency limit skips back to the cushion. Neither changes tempo or pitch. Streams the sound
//! lets go travel back through a second queue, so their rings are freed off the audio thread.

use std::{
    convert::Infallible,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use kira::{
    Frame,
    info::Info,
    sound::{Sound, SoundData},
};
use rtrb::{Consumer, Producer, RingBuffer};
use tufo_wire::browser_host::AudioStreamId;

use super::buffered_track::frames_in;

/// Pages playing at once; a further stream is dropped until one stops.
pub const BROWSER_STREAMS_MAX: usize = 8;
const COMMAND_CAPACITY: usize = 32;
const RETIRED_CAPACITY: usize = BROWSER_STREAMS_MAX * 2;
/// Audio buffered before a stream plays, which absorbs packet jitter and the output's period.
pub const BROWSER_CUSHION: Duration = Duration::from_millis(80);
/// Buffered audio beyond which a stream skips back to its cushion.
pub const BROWSER_LATENCY_MAX: Duration = Duration::from_millis(250);
/// Each stream's ring, which holds what arrives between two output periods.
pub const BROWSER_RING: Duration = Duration::from_millis(500);
/// Silence a stream ends with a longer gap counts as a pause rather than starvation.
const STARVATION_GAP_MAX: Duration = Duration::from_millis(500);

/// Browser audio totals shared by every mixer the engine opens.
#[derive(Debug, Default)]
pub struct BrowserAudioCounters {
    /// Silence inserted because a page's packets arrived late.
    pub underrun_frames: AtomicU64,
    /// Frames skipped or never played because they arrived early or found no mixer.
    pub dropped_frames: AtomicU64,
}

#[derive(Debug, thiserror::Error)]
#[error("the browser input's command queue is full")]
pub struct BrowserInputBusy;

/// Frame counts at one mixer's sample rate.
#[derive(Debug, Clone, Copy)]
struct StreamTimings {
    cushion_frames: usize,
    latency_max_frames: usize,
    starvation_gap_max_frames: u64,
}

impl StreamTimings {
    fn at(sample_rate: u32) -> Self {
        Self {
            cushion_frames: frames_in(BROWSER_CUSHION, sample_rate),
            latency_max_frames: frames_in(BROWSER_LATENCY_MAX, sample_rate),
            starvation_gap_max_frames: u64::try_from(frames_in(STARVATION_GAP_MAX, sample_rate))
                .unwrap_or(u64::MAX),
        }
    }
}

/// Creates the input for a mixer running at `sample_rate`: the sound to play on the browser track
/// and the handle that adds streams to it.
pub(crate) fn browser_input(
    sample_rate: u32,
    counters: Arc<BrowserAudioCounters>,
) -> (BrowserInputSound, BrowserInputHandle) {
    let (command_producer, command_consumer) = RingBuffer::new(COMMAND_CAPACITY);
    let (retired_producer, retired_consumer) = RingBuffer::new(RETIRED_CAPACITY);
    let input = BrowserInput {
        commands: command_consumer,
        retired: retired_producer,
        streams: [const { None }; BROWSER_STREAMS_MAX],
        timings: StreamTimings::at(sample_rate),
        counters,
    };
    let handle = BrowserInputHandle {
        commands: command_producer,
        retired: retired_consumer,
        sample_rate,
    };
    (BrowserInputSound(input), handle)
}

/// Adds and ends streams from outside the audio thread. Dropping it lets the sound finish once
/// its streams have.
pub struct BrowserInputHandle {
    commands: Producer<BrowserInputCommand>,
    retired: Consumer<LiveStream>,
    sample_rate: u32,
}

impl BrowserInputHandle {
    #[must_use]
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Starts a stream that plays once its cushion fills, returning the ring's writing end.
    pub fn add_stream(&mut self, id: AudioStreamId) -> Result<Producer<Frame>, BrowserInputBusy> {
        let (producer, consumer) = RingBuffer::new(frames_in(BROWSER_RING, self.sample_rate));
        let stream = LiveStream {
            id,
            consumer,
            playing: false,
            finishing: false,
            starved_frames: 0,
        };
        self.commands
            .push(BrowserInputCommand::Add(stream))
            .map_err(|_| BrowserInputBusy)?;
        Ok(producer)
    }

    /// Plays what the stream buffered, then drops it.
    pub fn finish_stream(&mut self, id: AudioStreamId) -> Result<(), BrowserInputBusy> {
        self.commands
            .push(BrowserInputCommand::Finish(id))
            .map_err(|_| BrowserInputBusy)
    }

    /// Frees the streams the sound let go of.
    pub fn drop_retired(&mut self) {
        while let Ok(stream) = self.retired.pop() {
            drop(stream);
        }
    }
}

enum BrowserInputCommand {
    Add(LiveStream),
    Finish(AudioStreamId),
}

struct LiveStream {
    id: AudioStreamId,
    consumer: Consumer<Frame>,
    /// Past its cushion; otherwise silent until the cushion fills.
    playing: bool,
    /// The page stopped, so the stream ends once its ring is empty.
    finishing: bool,
    /// Silence played since the ring last ran dry.
    starved_frames: u64,
}

/// What became of a stream after one period.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamProgress {
    Continues,
    Ended,
}

impl LiveStream {
    fn mix_into(
        &mut self,
        out: &mut [Frame],
        timings: StreamTimings,
        counters: &BrowserAudioCounters,
    ) -> StreamProgress {
        let mut available = self.consumer.slots();
        if !self.playing
            && let Some(progress) = self.wait_for_cushion(available, out.len(), timings, counters)
        {
            return progress;
        }
        if available > timings.latency_max_frames {
            let excess = available - timings.cushion_frames;
            if let Ok(chunk) = self.consumer.read_chunk(excess) {
                chunk.commit_all();
            }
            counters
                .dropped_frames
                .fetch_add(excess as u64, Ordering::Relaxed);
            available = timings.cushion_frames;
        }
        let count = available.min(out.len());
        if let Ok(chunk) = self.consumer.read_chunk(count) {
            let (first, second) = chunk.as_slices();
            for (mixed, frame) in out.iter_mut().zip(first.iter().chain(second)) {
                *mixed += *frame;
            }
            chunk.commit_all();
        }
        if count < out.len() {
            if self.finishing {
                return StreamProgress::Ended;
            }
            self.playing = false;
            self.starved_frames = (out.len() - count) as u64;
        }
        StreamProgress::Continues
    }
}

impl LiveStream {
    /// Keeps a silent stream silent until its cushion fills or it is finishing; `None` once it
    /// plays again.
    fn wait_for_cushion(
        &mut self,
        available: usize,
        period_frames: usize,
        timings: StreamTimings,
        counters: &BrowserAudioCounters,
    ) -> Option<StreamProgress> {
        if self.finishing && available == 0 {
            return Some(StreamProgress::Ended);
        }
        if available < timings.cushion_frames && !self.finishing {
            if self.starved_frames > 0 {
                self.starved_frames += period_frames as u64;
            }
            return Some(StreamProgress::Continues);
        }
        self.playing = true;
        if self.starved_frames <= timings.starvation_gap_max_frames {
            counters
                .underrun_frames
                .fetch_add(self.starved_frames, Ordering::Relaxed);
        }
        self.starved_frames = 0;
        None
    }
}

/// The input as kira plays it.
pub struct BrowserInputSound(BrowserInput);

impl SoundData for BrowserInputSound {
    type Error = Infallible;
    type Handle = ();

    fn into_sound(self) -> Result<(Box<dyn Sound>, Self::Handle), Self::Error> {
        Ok((Box::new(self.0), ()))
    }
}

struct BrowserInput {
    commands: Consumer<BrowserInputCommand>,
    retired: Producer<LiveStream>,
    streams: [Option<LiveStream>; BROWSER_STREAMS_MAX],
    timings: StreamTimings,
    counters: Arc<BrowserAudioCounters>,
}

impl BrowserInput {
    fn apply_commands(&mut self) {
        while let Ok(command) = self.commands.pop() {
            match command {
                BrowserInputCommand::Add(stream) => self.add(stream),
                BrowserInputCommand::Finish(id) => self
                    .streams
                    .iter_mut()
                    .flatten()
                    .filter(|stream| stream.id == id)
                    .for_each(|stream| stream.finishing = true),
            }
        }
    }

    fn add(&mut self, stream: LiveStream) {
        match self.streams.iter().position(Option::is_none) {
            Some(slot) => self.streams[slot] = Some(stream),
            None => self.retire(stream),
        }
    }

    fn retire(&mut self, stream: LiveStream) {
        // A full queue drops the stream here, which only frees memory; the handle drains the
        // queue every engine tick, so it stays far from full.
        let _ = self.retired.push(stream);
    }
}

impl Sound for BrowserInput {
    fn process(&mut self, out: &mut [Frame], _dt: f64, _info: &Info) {
        self.apply_commands();
        out.fill(Frame::ZERO);
        for slot in 0..BROWSER_STREAMS_MAX {
            let Some(stream) = &mut self.streams[slot] else {
                continue;
            };
            if stream.mix_into(out, self.timings, &self.counters) == StreamProgress::Ended
                && let Some(stream) = self.streams[slot].take()
            {
                self.retire(stream);
            }
        }
    }

    fn finished(&self) -> bool {
        self.commands.is_abandoned() && self.streams.iter().all(Option::is_none)
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use super::*;

    const SAMPLE_RATE: u32 = 1_000;

    fn stream_id(id: u32) -> AudioStreamId {
        AudioStreamId(NonZeroU32::new(id).unwrap())
    }

    fn input() -> (BrowserInput, BrowserInputHandle, Arc<BrowserAudioCounters>) {
        let counters = Arc::new(BrowserAudioCounters::default());
        let (BrowserInputSound(input), handle) = browser_input(SAMPLE_RATE, Arc::clone(&counters));
        (input, handle, counters)
    }

    fn render(input: &mut BrowserInput, frames: usize) -> Vec<f32> {
        let mut out = vec![Frame::from_mono(9.0); frames];
        input.process(&mut out, 0.0, &kira::info::MockInfoBuilder::new().build());
        out.iter().map(|frame| frame.left).collect()
    }

    fn push(producer: &mut Producer<Frame>, value: f32, frames: usize) {
        for _ in 0..frames {
            producer.push(Frame::from_mono(value)).unwrap();
        }
    }

    #[test]
    fn streams_wait_for_their_cushion_then_sum() {
        let (mut input, mut handle, counters) = input();
        let mut first = handle.add_stream(stream_id(1)).unwrap();
        let mut second = handle.add_stream(stream_id(2)).unwrap();

        push(&mut first, 0.25, 79);
        push(&mut second, 0.5, 80);
        assert_eq!(render(&mut input, 10), [0.5; 10]);

        push(&mut first, 0.25, 11);
        assert_eq!(render(&mut input, 10), [0.75; 10]);
        assert_eq!(counters.underrun_frames.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn short_gaps_count_as_underruns_and_long_ones_as_pauses() {
        let (mut input, mut handle, counters) = input();
        let mut producer = handle.add_stream(stream_id(1)).unwrap();
        push(&mut producer, 0.5, 80);
        render(&mut input, 70);

        let mut played = render(&mut input, 20);
        assert_eq!(played.split_off(10), [0.0; 10]);
        push(&mut producer, 0.5, 80);
        render(&mut input, 10);
        assert_eq!(counters.underrun_frames.load(Ordering::Relaxed), 10);

        render(&mut input, 80);
        render(&mut input, 600);
        push(&mut producer, 0.5, 80);
        render(&mut input, 10);
        assert_eq!(counters.underrun_frames.load(Ordering::Relaxed), 10);
    }

    #[test]
    fn streams_running_ahead_skip_back_to_their_cushion() {
        let (mut input, mut handle, counters) = input();
        let mut producer = handle.add_stream(stream_id(1)).unwrap();
        push(&mut producer, 0.1, 300);

        render(&mut input, 10);

        assert_eq!(counters.dropped_frames.load(Ordering::Relaxed), 220);
        assert_eq!(producer.slots(), 500 - 70);
    }

    #[test]
    fn finished_streams_play_out_and_leave() {
        let (mut input, mut handle, counters) = input();
        let mut producer = handle.add_stream(stream_id(1)).unwrap();
        push(&mut producer, 0.5, 30);
        handle.finish_stream(stream_id(1)).unwrap();

        let played = render(&mut input, 40);

        assert_eq!(played[..30], [0.5; 30]);
        assert_eq!(played[30..], [0.0; 10]);
        assert!(input.streams.iter().all(Option::is_none));
        handle.drop_retired();
        assert_eq!(counters.underrun_frames.load(Ordering::Relaxed), 0);
        drop(handle);
        assert!(input.finished());
    }
}
