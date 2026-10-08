//! Moves the browser's audio packets into the mixer: each page stream becomes stereo at the
//! mixer's rate and fills its own ring. A new mixer after an output change gets fresh rings, and
//! packets that find no mixer are dropped.

use std::{
    collections::BTreeMap,
    sync::{Arc, atomic::Ordering},
};

use kira::Frame;
use rtrb::Producer;
use rubato::{Fft, FixedSync, Resampler as _, audioadapter_buffers::direct::InterleavedSlice};
use tufo_wire::browser_host::AudioStreamId;

use crate::audio::browser_input::{BrowserAudioCounters, BrowserInputHandle};

const STEREO: usize = 2;

/// What the engine tells the router about mixers.
pub(crate) enum RouterControl {
    /// Plays streams on this input from now on.
    Attach(BrowserInputHandle),
    /// The mixer closed; packets drop until the next one.
    Detach,
}

/// Whether any page sends audio, after the router took an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Audible {
    Unchanged,
    Changed(bool),
}

pub(crate) struct AudioRouter {
    attachment: Option<Attachment>,
    attachment_generation: u64,
    streams: BTreeMap<AudioStreamId, RoutedStream>,
    counters: Arc<BrowserAudioCounters>,
}

struct Attachment {
    input: BrowserInputHandle,
    generation: u64,
}

struct RoutedStream {
    sample_rate: u32,
    channels: usize,
    sink: Option<StreamSink>,
    stereo: Vec<f32>,
}

/// One stream's ring in one mixer.
struct StreamSink {
    attachment_generation: u64,
    producer: Producer<Frame>,
    resampler: Option<LiveResampler>,
}

impl AudioRouter {
    pub(crate) fn new(counters: Arc<BrowserAudioCounters>) -> Self {
        Self {
            attachment: None,
            attachment_generation: 0,
            streams: BTreeMap::new(),
            counters,
        }
    }

    pub(crate) fn control(&mut self, control: RouterControl) {
        self.attachment_generation += 1;
        self.attachment = match control {
            RouterControl::Attach(input) => Some(Attachment {
                input,
                generation: self.attachment_generation,
            }),
            RouterControl::Detach => None,
        };
    }

    pub(crate) fn start_stream(
        &mut self,
        id: AudioStreamId,
        sample_rate: u32,
        channels: usize,
    ) -> Audible {
        let was_audible = !self.streams.is_empty();
        self.streams.insert(
            id,
            RoutedStream {
                sample_rate,
                channels,
                sink: None,
                stereo: Vec::new(),
            },
        );
        audible_change(was_audible, true)
    }

    pub(crate) fn stop_stream(&mut self, id: AudioStreamId) -> Audible {
        let Some(stream) = self.streams.remove(&id) else {
            return Audible::Unchanged;
        };
        if let (Some(sink), Some(attachment)) = (&stream.sink, &mut self.attachment)
            && sink.attachment_generation == attachment.generation
        {
            // A full queue leaves the stream to run dry, which the mixer reads as a pause.
            let _ = attachment.input.finish_stream(id);
        }
        audible_change(true, !self.streams.is_empty())
    }

    /// Routes one packet of interleaved samples to its stream's ring.
    pub(crate) fn route_packet(&mut self, id: AudioStreamId, samples: &[f32]) {
        let counters = Arc::clone(&self.counters);
        let Some(stream) = self.streams.get_mut(&id) else {
            return;
        };
        let channels = stream.channels.max(1);
        let frames = (samples.len() / channels) as u64;
        let Some(attachment) = &mut self.attachment else {
            counters.dropped_frames.fetch_add(frames, Ordering::Relaxed);
            return;
        };
        attachment.input.drop_retired();
        let sink_is_stale = stream
            .sink
            .as_ref()
            .is_none_or(|sink| sink.attachment_generation != attachment.generation);
        if sink_is_stale {
            stream.sink = new_sink(id, stream.sample_rate, attachment);
        }
        let Some(sink) = &mut stream.sink else {
            counters.dropped_frames.fetch_add(frames, Ordering::Relaxed);
            return;
        };
        to_stereo(samples, channels, &mut stream.stereo);
        let output: &[f32] = match &mut sink.resampler {
            Some(resampler) => resampler.push(&stream.stereo),
            None => &stream.stereo,
        };
        let mut unwritten = 0_u64;
        for [left, right] in output.as_chunks::<STEREO>().0 {
            if sink
                .producer
                .push(Frame {
                    left: *left,
                    right: *right,
                })
                .is_err()
            {
                unwritten += 1;
            }
        }
        if unwritten > 0 {
            counters
                .dropped_frames
                .fetch_add(unwritten, Ordering::Relaxed);
        }
    }

    pub(crate) fn is_audible(&self) -> bool {
        !self.streams.is_empty()
    }
}

fn audible_change(before: bool, after: bool) -> Audible {
    if before == after {
        Audible::Unchanged
    } else {
        Audible::Changed(after)
    }
}

fn new_sink(
    id: AudioStreamId,
    sample_rate: u32,
    attachment: &mut Attachment,
) -> Option<StreamSink> {
    let mixer_rate = attachment.input.sample_rate();
    let resampler = if sample_rate == mixer_rate {
        None
    } else {
        match LiveResampler::new(sample_rate, mixer_rate) {
            Ok(resampler) => Some(resampler),
            Err(error) => {
                tracing::warn!(%error, sample_rate, mixer_rate, "cannot resample browser audio");
                return None;
            }
        }
    };
    let producer = attachment.input.add_stream(id).ok()?;
    Some(StreamSink {
        attachment_generation: attachment.generation,
        producer,
        resampler,
    })
}

/// Duplicates mono and keeps the front pair of wider layouts.
fn to_stereo(samples: &[f32], channels: usize, stereo: &mut Vec<f32>) {
    stereo.clear();
    for frame in samples.chunks_exact(channels) {
        match frame {
            [mono] => stereo.extend_from_slice(&[*mono, *mono]),
            [left, right, ..] => stereo.extend_from_slice(&[*left, *right]),
            [] => {}
        }
    }
}

/// Converts a live stereo stream between two fixed rates as packets arrive, keeping the
/// resampler's delay rather than trimming it.
struct LiveResampler {
    resampler: Fft<f32>,
    pending: Vec<f32>,
    chunk: Vec<f32>,
    output: Vec<f32>,
}

/// Input frames per resampler chunk: 10 ms at 48 kHz, one browser packet.
const RESAMPLER_CHUNK_FRAMES: usize = 480;

impl LiveResampler {
    fn new(source_rate: u32, output_rate: u32) -> anyhow::Result<Self> {
        let resampler = Fft::<f32>::new(
            usize::try_from(source_rate)?,
            usize::try_from(output_rate)?,
            RESAMPLER_CHUNK_FRAMES,
            STEREO,
            FixedSync::Input,
        )?;
        let chunk = vec![0.0; resampler.output_frames_max() * STEREO];
        Ok(Self {
            resampler,
            pending: Vec::new(),
            chunk,
            output: Vec::new(),
        })
    }

    /// Resamples every whole chunk now pending, returning the frames produced.
    fn push(&mut self, stereo: &[f32]) -> &[f32] {
        self.pending.extend_from_slice(stereo);
        self.output.clear();
        while self.pending.len() / STEREO >= self.resampler.input_frames_next() {
            let input_frames = self.pending.len() / STEREO;
            let output_frames = self.chunk.len() / STEREO;
            let (Ok(input), Ok(mut output)) = (
                InterleavedSlice::new(&self.pending, STEREO, input_frames),
                InterleavedSlice::new_mut(&mut self.chunk, STEREO, output_frames),
            ) else {
                break;
            };
            let Ok((consumed, produced)) =
                self.resampler
                    .process_into_buffer(&input, &mut output, None)
            else {
                break;
            };
            self.output
                .extend_from_slice(&self.chunk[..produced * STEREO]);
            self.pending.drain(..consumed * STEREO);
        }
        &self.output
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use super::*;
    use crate::audio::browser_input::browser_input;

    fn stream_id(id: u32) -> AudioStreamId {
        AudioStreamId(NonZeroU32::new(id).unwrap())
    }

    fn router() -> (AudioRouter, Arc<BrowserAudioCounters>) {
        let counters = Arc::new(BrowserAudioCounters::default());
        (AudioRouter::new(Arc::clone(&counters)), counters)
    }

    #[test]
    fn audibility_follows_the_first_and_last_stream() {
        let (mut router, _) = router();

        assert_eq!(
            router.start_stream(stream_id(1), 48_000, 2),
            Audible::Changed(true)
        );
        assert_eq!(
            router.start_stream(stream_id(2), 48_000, 2),
            Audible::Unchanged
        );
        assert_eq!(router.stop_stream(stream_id(1)), Audible::Unchanged);
        assert_eq!(router.stop_stream(stream_id(9)), Audible::Unchanged);
        assert_eq!(router.stop_stream(stream_id(2)), Audible::Changed(false));
    }

    #[test]
    fn packets_without_a_mixer_are_dropped() {
        let (mut router, counters) = router();
        router.start_stream(stream_id(1), 48_000, 2);

        router.route_packet(stream_id(1), &[0.5; 960]);

        assert_eq!(counters.dropped_frames.load(Ordering::Relaxed), 480);
    }

    #[test]
    fn mono_packets_reach_the_mixer_as_stereo() {
        let (mut router, counters) = router();
        let (_sound, input) = browser_input(48_000, Arc::clone(&counters));
        router.control(RouterControl::Attach(input));
        router.start_stream(stream_id(1), 48_000, 1);

        router.route_packet(stream_id(1), &[0.25; 480]);

        let stream = &router.streams[&stream_id(1)];
        let sink = stream.sink.as_ref().unwrap();
        assert_eq!(sink.producer.slots(), 24_000 - 480);
        assert_eq!(stream.stereo, [0.25; 960]);
        assert_eq!(counters.dropped_frames.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn a_new_mixer_gets_a_new_ring_at_its_rate() {
        let (mut router, counters) = router();
        let (_first_sound, first) = browser_input(48_000, Arc::clone(&counters));
        router.control(RouterControl::Attach(first));
        router.start_stream(stream_id(1), 48_000, 2);
        router.route_packet(stream_id(1), &[0.1; 960]);

        let (_second_sound, second) = browser_input(44_100, Arc::clone(&counters));
        router.control(RouterControl::Attach(second));
        for _ in 0..10 {
            router.route_packet(stream_id(1), &[0.1; 960]);
        }

        let sink = router.streams[&stream_id(1)].sink.as_ref().unwrap();
        assert!(sink.resampler.is_some());
        let written = 22_050 - sink.producer.slots();
        assert!((4_000..=4_410).contains(&written), "{written} frames");
    }
}
