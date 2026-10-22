//! Decodes one file into stereo frames at the output rate on the decoder thread.

use std::{
    io,
    ops::ControlFlow,
    sync::{Arc, atomic::Ordering},
    thread,
    time::Duration,
};

use anyhow::Context as _;
use rtrb::Producer;
use rubato::{
    Fft, FixedSync, Indexing, Resampler as _, audioadapter_buffers::direct::InterleavedSlice,
};
use symphonia::core::{
    codecs::{
        CodecParameters,
        audio::{AudioDecoder, AudioDecoderOptions},
    },
    errors,
    formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType, probe::Hint},
    io::{MediaSourceStream, MediaSourceStreamOptions},
    meta::MetadataOptions,
    units::{TimeBase, Timestamp},
};
use tufo_models::library::song_loop::{LoopMarkers, SourceFrames};

use super::{
    TrackShared, duration_of_frames, frame_count, frames_in,
    track_file::{TrackFile, TrackFileReader},
};
use crate::audio::{
    codecs,
    memory::{AudioMemory, MemoryReservation, PreparationCancellation, ResidentMemory},
    packed_frame::PackedFrame,
    seam,
};

const STEREO: usize = 2;
const RESAMPLER_CHUNK_FRAMES: usize = 1024;
const FULL_BUFFER_WAIT: Duration = Duration::from_millis(50);

struct ReservedFrames {
    // Release the allocation before its admission reservation.
    frames: Vec<PackedFrame>,
    reservation: MemoryReservation,
}

impl ReservedFrames {
    fn new(capacity: usize, memory: &AudioMemory) -> Option<Self> {
        let reservation = memory.reserve(capacity.saturating_mul(size_of::<PackedFrame>()))?;
        Some(Self {
            frames: Vec::with_capacity(capacity),
            reservation,
        })
    }

    fn extend(&mut self, stereo: &[f32], frames_max: usize, memory: &AudioMemory) -> bool {
        let frames_next = self.frames.len().saturating_add(stereo.len() / STEREO);
        if frames_next > frames_max {
            return false;
        }
        if frames_next > self.frames.capacity() {
            let capacity = self
                .frames
                .capacity()
                .saturating_mul(2)
                .max(frames_next)
                .min(frames_max);
            let Some(mut next) = Self::new(capacity, memory) else {
                return false;
            };
            next.frames.extend_from_slice(&self.frames);
            *self = next;
        }
        self.frames.extend(
            stereo
                .as_chunks::<STEREO>()
                .0
                .iter()
                .map(|&[left, right]| PackedFrame::new(left, right)),
        );
        true
    }
}

/// A container probed by content, since file extensions lie (MP4 files named `.mp3`).
pub(crate) struct ProbedTrack {
    format_reader: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    source_sample_rate: u32,
    time_base: TimeBase,
    duration: Option<Duration>,
    seek_preroll: Duration,
    decoder_delay: SourceFrames,
}

impl ProbedTrack {
    pub(super) fn probe(reader: TrackFileReader) -> anyhow::Result<Self> {
        let source = MediaSourceStream::new(Box::new(reader), MediaSourceStreamOptions::default());
        let format_reader = symphonia::default::get_probe()
            .probe(
                &Hint::default(),
                source,
                FormatOptions::default(),
                MetadataOptions::default(),
            )
            .context("probing the container")?;
        let track = format_reader
            .default_track(TrackType::Audio)
            .context("the file has no audio track")?;
        let Some(CodecParameters::Audio(parameters)) = track.codec_params.as_ref() else {
            anyhow::bail!("the default track is not audio");
        };
        let source_sample_rate = parameters
            .sample_rate
            .context("the audio track has no sample rate")?;
        let decoder = codecs::registry()
            .make_audio_decoder(parameters, &AudioDecoderOptions::default())
            .context("creating the audio decoder")?;
        let seek_preroll = codecs::seek_preroll(parameters.codec);
        let decoder_delay_frames =
            SourceFrames(u64::from(codecs::decoder_delay_frames(parameters, track)));
        let time_base = track
            .time_base
            .or_else(|| TimeBase::try_from_recip(source_sample_rate))
            .context("the audio track has no time base")?;
        let duration = match (track.num_frames, track.duration) {
            (Some(frames), _) => Some(duration_of_frames(frames, source_sample_rate)),
            (None, Some(duration)) => time_base
                .calc_duration(duration)
                .and_then(|time| Duration::try_from_secs_f64(time.as_secs_f64()).ok()),
            (None, None) => None,
        }
        .map(|duration| duration.saturating_sub(codecs::decoder_delay(parameters, track)));
        let track_id = track.id;
        Ok(Self {
            format_reader,
            decoder,
            track_id,
            source_sample_rate,
            time_base,
            duration,
            seek_preroll,
            decoder_delay: decoder_delay_frames,
        })
    }

    pub(super) fn duration(&self) -> Option<Duration> {
        self.duration
    }

    pub(super) fn source_sample_rate(&self) -> u32 {
        self.source_sample_rate
    }

    pub(super) fn into_decoder(
        mut self,
        file: TrackFile,
        output_sample_rate: u32,
        start: SourceFrames,
        region: Option<LoopMarkers>,
    ) -> anyhow::Result<TrackDecoder> {
        let region = region
            .map(|markers| RegionCursor::new(&file, markers, start))
            .transpose()?;
        let frames_to_skip = if start == SourceFrames::ZERO {
            SourceFrames::ZERO
        } else {
            self.seek_to_frame(start)?
        };
        let resampler = if self.source_sample_rate == output_sample_rate {
            None
        } else {
            Some(StereoResampler::new(
                self.source_sample_rate,
                output_sample_rate,
            )?)
        };
        Ok(TrackDecoder {
            file,
            track: self,
            resampler,
            output_sample_rate,
            frames_to_skip,
            decoded: Vec::new(),
            stereo: Vec::new(),
            region,
        })
    }

    /// Decodes the file into stereo frames at its own rate, or returns `None` once it runs past
    /// `frames_max`, reserving every live allocation before growing. Blocks the calling thread.
    pub(crate) fn decode_whole(
        mut self,
        frames_max: usize,
        memory: &AudioMemory,
        cancellation: &PreparationCancellation,
    ) -> anyhow::Result<Option<(Vec<PackedFrame>, MemoryReservation)>> {
        let mut decoded = Vec::new();
        let mut stereo = Vec::new();
        let frames_expected = self.duration.map_or(frames_max, |duration| {
            frames_in(duration, self.source_sample_rate)
                .saturating_add(1)
                .min(frames_max)
        });
        let Some(mut frames) = ReservedFrames::new(frames_expected, memory) else {
            return Ok(None);
        };
        while self.decode_next_packet(&mut decoded, &mut stereo)? {
            if cancellation.is_cancelled() {
                return Ok(None);
            }
            if !frames.extend(&stereo, frames_max, memory) {
                return Ok(None);
            }
        }
        Ok(Some((frames.frames, frames.reservation)))
    }

    /// Decodes up to `frames_max` frames from `start` into interleaved stereo at the file's own
    /// rate. Blocks the calling thread.
    pub(crate) fn decode_range(
        mut self,
        start: SourceFrames,
        frames_max: usize,
    ) -> anyhow::Result<Vec<f32>> {
        let mut frames_to_skip = if start == SourceFrames::ZERO {
            0
        } else {
            self.seek_to_frame(start)?.0
        };
        let mut decoded = Vec::new();
        let mut stereo = Vec::new();
        let samples_max = frames_max.saturating_mul(STEREO);
        let mut samples = Vec::with_capacity(samples_max);
        while samples.len() < samples_max && self.decode_next_packet(&mut decoded, &mut stereo)? {
            let skipped = usize::try_from(frames_to_skip)
                .unwrap_or(usize::MAX)
                .min(stereo.len() / STEREO);
            frames_to_skip -= frame_count(skipped);
            let kept = &stereo[skipped * STEREO..];
            let wanted = (samples_max - samples.len()).min(kept.len());
            samples.extend_from_slice(&kept[..wanted]);
        }
        Ok(samples)
    }

    /// Decodes the track's next packet into `stereo`, which stays empty for packets of other
    /// tracks and packets the decoder rejects. Returns `false` at the end of the file.
    pub(crate) fn decode_next_packet(
        &mut self,
        decoded: &mut Vec<f32>,
        stereo: &mut Vec<f32>,
    ) -> anyhow::Result<bool> {
        self.decode_next_packet_observed(decoded, stereo, |_| {})
    }

    /// Hands the track's compressed packets to `observe_packet` before decoding them.
    pub(crate) fn decode_next_packet_observed(
        &mut self,
        decoded: &mut Vec<f32>,
        stereo: &mut Vec<f32>,
        observe_packet: impl FnOnce(&[u8]),
    ) -> anyhow::Result<bool> {
        stereo.clear();
        let Some(packet) = self.next_packet()? else {
            return Ok(false);
        };
        if packet.track_id != self.track_id {
            return Ok(true);
        }
        observe_packet(&packet.data);
        let buffer = match self.decoder.decode(&packet) {
            Ok(buffer) => buffer,
            Err(errors::Error::DecodeError(reason)) => {
                tracing::debug!(reason, "skipping an undecodable packet");
                return Ok(true);
            }
            Err(error) => return Err(error).context("decoding a packet"),
        };
        let channel_count = buffer.spec().channels().count();
        buffer.copy_to_vec_interleaved(&mut *decoded);
        downmix_to_stereo(decoded, channel_count, stereo);
        Ok(true)
    }

    /// Returns the frames to discard after the seek to land exactly on `frame`: demuxers stop at
    /// a packet boundary, and some decoders need a run-up.
    fn seek_to_frame(&mut self, frame: SourceFrames) -> anyhow::Result<SourceFrames> {
        let preroll = SourceFrames(frame_count(frames_in(
            self.seek_preroll,
            self.source_sample_rate,
        )))
        .min(frame);
        let target = frame.saturating_add(self.decoder_delay);
        let seeked = self
            .format_reader
            .seek(
                SeekMode::Accurate,
                SeekTo::Timestamp {
                    ts: self.timestamp_of_frame(target.saturating_sub(preroll))?,
                    track_id: self.track_id,
                },
            )
            .context("seeking in the container")?;
        self.decoder.reset();
        Ok(target.saturating_sub(self.frame_of_timestamp(seeked.actual_ts)))
    }

    fn timestamp_of_frame(&self, frame: SourceFrames) -> anyhow::Result<Timestamp> {
        let ticks = u128::from(frame.0) * u128::from(self.time_base.denom.get())
            / (u128::from(self.time_base.numer.get()) * u128::from(self.source_sample_rate));
        Ok(Timestamp::new(
            i64::try_from(ticks).context("the position overflows")?,
        ))
    }

    fn frame_of_timestamp(&self, timestamp: Timestamp) -> SourceFrames {
        let ticks = u128::try_from(timestamp.get()).unwrap_or(0);
        let frames =
            ticks * u128::from(self.time_base.numer.get()) * u128::from(self.source_sample_rate)
                / u128::from(self.time_base.denom.get());
        SourceFrames(u64::try_from(frames).unwrap_or(u64::MAX))
    }

    /// The next packet, or `None` at the end of the file.
    fn next_packet(&mut self) -> anyhow::Result<Option<symphonia::core::packet::Packet>> {
        match self.format_reader.next_packet() {
            Ok(packet) => Ok(packet),
            Err(errors::Error::IoError(error)) if error.kind() == io::ErrorKind::UnexpectedEof => {
                Ok(None)
            }
            Err(error) => Err(error).context("reading the next packet"),
        }
    }
}

pub(super) struct TrackDecoder {
    file: TrackFile,
    track: ProbedTrack,
    resampler: Option<StereoResampler>,
    output_sample_rate: u32,
    frames_to_skip: SourceFrames,
    decoded: Vec<f32>,
    stereo: Vec<f32>,
    region: Option<RegionCursor>,
}

struct RegionCursor {
    markers: LoopMarkers,
    next_frame: SourceFrames,
    /// Interleaved stereo frames just before the loop start, as long as the crossfade.
    lead_in: Vec<f32>,
}

impl RegionCursor {
    fn new(file: &TrackFile, markers: LoopMarkers, start: SourceFrames) -> anyhow::Result<Self> {
        let lead_in = if markers.crossfade == SourceFrames::ZERO {
            Vec::new()
        } else {
            ProbedTrack::probe(file.reader())?
                .decode_range(
                    markers.loop_start.saturating_sub(markers.crossfade),
                    usize::try_from(markers.crossfade.0)?,
                )
                .context("decoding the audio leading into the loop")?
        };
        Ok(Self {
            markers,
            next_frame: start,
            lead_in,
        })
    }

    fn frames_until_loop_end(&self) -> SourceFrames {
        self.markers.loop_end.saturating_sub(self.next_frame)
    }

    /// Returns how many samples of `stereo` lie before the loop end, and whether they reach it.
    fn take_until_loop_end(
        &mut self,
        stereo: &mut [f32],
        wraps_at_loop_end: bool,
    ) -> (usize, bool) {
        let available = SourceFrames(frame_count(stereo.len() / STEREO));
        let taken = self.frames_until_loop_end().min(available);
        let kept_samples = usize::try_from(taken.0).unwrap_or(usize::MAX) * STEREO;
        if wraps_at_loop_end {
            self.fade_in_lead(&mut stereo[..kept_samples]);
        }
        self.next_frame = self.next_frame.saturating_add(taken);
        (
            kept_samples,
            self.frames_until_loop_end() == SourceFrames::ZERO,
        )
    }

    /// `stereo` holds the frames from `next_frame` on.
    fn fade_in_lead(&self, stereo: &mut [f32]) {
        let lead_in_frames = SourceFrames(frame_count(self.lead_in.len() / STEREO));
        let crossfade_start = self.markers.loop_end.saturating_sub(lead_in_frames);
        let chunk_end = self
            .next_frame
            .saturating_add(SourceFrames(frame_count(stereo.len() / STEREO)));
        if self.lead_in.is_empty() || chunk_end <= crossfade_start {
            return;
        }
        let first_inside = crossfade_start.saturating_sub(self.next_frame);
        let tail_offset = self
            .next_frame
            .saturating_add(first_inside)
            .saturating_sub(crossfade_start);
        let (Ok(first_inside), Ok(tail_offset)) = (
            usize::try_from(first_inside.0),
            usize::try_from(tail_offset.0),
        ) else {
            return;
        };
        seam::fade_in_lead(
            &mut stereo[first_inside * STEREO..],
            &self.lead_in,
            tail_offset,
        );
    }
}

impl TrackDecoder {
    pub(super) fn run(
        mut self,
        mut producer: Producer<PackedFrame>,
        shared: &TrackShared,
        resident: [ResidentMemory; 2],
        reservation: Arc<MemoryReservation>,
    ) {
        let mut writer = FrameWriter {
            producer: &mut producer,
            shared,
        };
        if let Err(error) = self.decode_until_stopped(&mut writer) {
            tracing::warn!(error = format!("{error:#}"), "decoding stopped early");
        }
        shared.decoding_finished.store(true, Ordering::Release);
        shared.decoder_stopped.store(true, Ordering::Release);
        while !producer.is_abandoned() {
            thread::park_timeout(FULL_BUFFER_WAIT);
        }
        drop(resident);
        drop(producer);
        drop(reservation);
    }

    fn decode_until_stopped(&mut self, writer: &mut FrameWriter<'_>) -> anyhow::Result<()> {
        loop {
            self.decode_all(writer)?;
            writer
                .shared
                .decoding_finished
                .store(true, Ordering::Release);
            while !writer.shared.looping.load(Ordering::Relaxed) && !writer.should_stop() {
                thread::park_timeout(FULL_BUFFER_WAIT);
            }
            if writer.should_stop() {
                return Ok(());
            }
            // Prefill can reach the end before the caller enables looping. A finished resampler
            // has flushed its tail, so a later pass also needs a fresh one.
            self.restart_loop()?;
            if self.resampler.is_some() {
                self.resampler = Some(StereoResampler::new(
                    self.track.source_sample_rate,
                    self.output_sample_rate,
                )?);
            }
            writer
                .shared
                .decoding_finished
                .store(false, Ordering::Release);
        }
    }

    fn decode_all(&mut self, writer: &mut FrameWriter<'_>) -> anyhow::Result<()> {
        loop {
            if writer.should_stop() {
                return Ok(());
            }
            let file_continues = self
                .track
                .decode_next_packet(&mut self.decoded, &mut self.stereo)?;
            if !file_continues && self.wrap_if_looping(writer)? {
                continue;
            }
            if !file_continues {
                break;
            }
            let skipped_samples = self.take_skipped_frames(self.stereo.len() / STEREO) * STEREO;
            let wraps_at_loop_end = writer.shared.looping.load(Ordering::Relaxed);
            let (kept_samples, loop_end_reached) = match &mut self.region {
                Some(region) => region
                    .take_until_loop_end(&mut self.stereo[skipped_samples..], wraps_at_loop_end),
                None => (self.stereo.len() - skipped_samples, false),
            };
            let stereo = &self.stereo[skipped_samples..skipped_samples + kept_samples];
            let flow = match &mut self.resampler {
                Some(resampler) => resampler.push(stereo, writer),
                None => writer.write(stereo),
            };
            if flow.is_break() {
                return Ok(());
            }
            if loop_end_reached && !self.wrap_if_looping(writer)? {
                break;
            }
        }
        if let Some(resampler) = &mut self.resampler {
            resampler.finish(writer);
        }
        Ok(())
    }

    /// A file that ends before its loop end wraps where it ends.
    fn wrap_if_looping(&mut self, writer: &FrameWriter<'_>) -> anyhow::Result<bool> {
        if !writer.shared.looping.load(Ordering::Relaxed) {
            return Ok(false);
        }
        self.restart_loop()?;
        Ok(true)
    }

    /// Keeps the resampler's state, so the loop end joins the loop start without a click.
    fn restart_loop(&mut self) -> anyhow::Result<()> {
        let loop_start = self
            .region
            .as_ref()
            .map_or(SourceFrames::ZERO, |region| region.markers.loop_start);
        if loop_start == SourceFrames::ZERO {
            // Seeking to the start drops Vorbis's first packet.
            self.track = ProbedTrack::probe(self.file.reader())?;
            self.frames_to_skip = SourceFrames::ZERO;
        } else {
            self.frames_to_skip = self.track.seek_to_frame(loop_start)?;
        }
        if let Some(region) = &mut self.region {
            region.next_frame = loop_start;
        }
        Ok(())
    }

    /// Consumes up to `available` frames of the pending skip and returns how many to drop.
    fn take_skipped_frames(&mut self, available: usize) -> usize {
        let skipped = usize::try_from(self.frames_to_skip.0)
            .unwrap_or(usize::MAX)
            .min(available);
        self.frames_to_skip = self
            .frames_to_skip
            .saturating_sub(SourceFrames(frame_count(skipped)));
        skipped
    }
}

/// Keeps the front left and right channels, duplicating mono.
fn downmix_to_stereo(interleaved: &[f32], channel_count: usize, stereo: &mut Vec<f32>) {
    stereo.clear();
    match channel_count {
        0 => {}
        1 => stereo.extend(interleaved.iter().flat_map(|&sample| [sample, sample])),
        _ => stereo.extend(
            interleaved
                .chunks_exact(channel_count)
                .flat_map(|frame| [frame[0], frame[1]]),
        ),
    }
}

struct FrameWriter<'a> {
    producer: &'a mut Producer<PackedFrame>,
    shared: &'a TrackShared,
}

impl FrameWriter<'_> {
    fn should_stop(&self) -> bool {
        self.shared.stop_requested.load(Ordering::Relaxed) || self.producer.is_abandoned()
    }

    /// Writes interleaved stereo samples, waiting while the ring buffer is full.
    fn write(&mut self, stereo: &[f32]) -> ControlFlow<()> {
        let mut frames = stereo
            .as_chunks::<STEREO>()
            .0
            .iter()
            .map(|&[left, right]| PackedFrame::new(left, right));
        let mut remaining = stereo.len() / STEREO;
        while remaining > 0 {
            if self.should_stop() {
                return ControlFlow::Break(());
            }
            let writable = self.producer.slots().min(remaining);
            if writable == 0 {
                thread::park_timeout(FULL_BUFFER_WAIT);
                continue;
            }
            let Ok(chunk) = self.producer.write_chunk_uninit(writable) else {
                continue;
            };
            remaining -= chunk.fill_from_iter(frames.by_ref());
            self.shared.buffered_frames.store(
                frame_count(self.producer.buffer().capacity() - self.producer.slots()),
                Ordering::Relaxed,
            );
        }
        ControlFlow::Continue(())
    }
}

/// Converts interleaved stereo between two fixed sample rates, trimming the resampler's delay so
/// the output lines up with the input and ends where the input ends.
struct StereoResampler {
    resampler: Fft<f32>,
    pending: Vec<f32>,
    output: Vec<f32>,
    delay_frames_remaining: usize,
    input_frames_total: u64,
    output_frames_written: u64,
    source_sample_rate: u64,
    output_sample_rate: u64,
}

impl StereoResampler {
    fn new(source_sample_rate: u32, output_sample_rate: u32) -> anyhow::Result<Self> {
        let resampler = Fft::<f32>::new(
            usize::try_from(source_sample_rate)?,
            usize::try_from(output_sample_rate)?,
            RESAMPLER_CHUNK_FRAMES,
            STEREO,
            FixedSync::Input,
        )
        .context("creating the resampler")?;
        let output = vec![0.0; resampler.output_frames_max() * STEREO];
        let delay_frames_remaining = resampler.output_delay();
        Ok(Self {
            resampler,
            pending: Vec::new(),
            output,
            delay_frames_remaining,
            input_frames_total: 0,
            output_frames_written: 0,
            source_sample_rate: u64::from(source_sample_rate),
            output_sample_rate: u64::from(output_sample_rate),
        })
    }

    fn push(&mut self, stereo: &[f32], writer: &mut FrameWriter<'_>) -> ControlFlow<()> {
        self.pending.extend_from_slice(stereo);
        self.input_frames_total += u64::try_from(stereo.len() / STEREO).unwrap_or(u64::MAX);
        while self.pending.len() / STEREO >= self.resampler.input_frames_next() {
            self.process_chunk(None, writer)?;
        }
        ControlFlow::Continue(())
    }

    /// Resamples the partial last chunk, then silence until the delayed tail is out.
    fn finish(&mut self, writer: &mut FrameWriter<'_>) {
        let partial_frames = self.pending.len() / STEREO;
        if self.process_chunk(Some(partial_frames), writer).is_break() {
            return;
        }
        while self.output_frames_written < self.expected_output_frames() {
            let written_before = self.output_frames_written;
            if self.process_chunk(Some(0), writer).is_break()
                || self.output_frames_written == written_before
            {
                return;
            }
        }
    }

    fn expected_output_frames(&self) -> u64 {
        self.input_frames_total * self.output_sample_rate / self.source_sample_rate
    }

    fn process_chunk(
        &mut self,
        partial_frames: Option<usize>,
        writer: &mut FrameWriter<'_>,
    ) -> ControlFlow<()> {
        let input_frames = self.pending.len() / STEREO;
        let output_frames = self.output.len() / STEREO;
        let (Ok(input), Ok(mut output)) = (
            InterleavedSlice::new(&self.pending, STEREO, input_frames),
            InterleavedSlice::new_mut(&mut self.output, STEREO, output_frames),
        ) else {
            return ControlFlow::Break(());
        };
        let indexing = Indexing {
            partial_len: partial_frames,
            ..Indexing::default()
        };
        let (consumed, produced) =
            match self
                .resampler
                .process_into_buffer(&input, &mut output, Some(&indexing))
            {
                Ok(counts) => counts,
                Err(error) => {
                    tracing::warn!(%error, "resampling failed");
                    return ControlFlow::Break(());
                }
            };
        self.pending
            .drain(..(consumed * STEREO).min(self.pending.len()));

        let skipped = self.delay_frames_remaining.min(produced);
        self.delay_frames_remaining -= skipped;
        let remaining_frames = self
            .expected_output_frames()
            .saturating_sub(self.output_frames_written);
        let emitted =
            (produced - skipped).min(usize::try_from(remaining_frames).unwrap_or(usize::MAX));
        self.output_frames_written += u64::try_from(emitted).unwrap_or(u64::MAX);
        writer.write(&self.output[skipped * STEREO..(skipped + emitted) * STEREO])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downmix_duplicates_mono_and_keeps_the_front_pair() {
        let mut stereo = Vec::new();

        downmix_to_stereo(&[0.1, 0.2], 1, &mut stereo);
        assert_eq!(stereo, [0.1, 0.1, 0.2, 0.2]);

        downmix_to_stereo(&[0.1, 0.2, 0.9, 0.3, 0.4, 0.9], 3, &mut stereo);
        assert_eq!(stereo, [0.1, 0.2, 0.3, 0.4]);
    }
}
