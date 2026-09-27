//! Decodes one file into stereo frames at the output rate on the decoder thread.

use std::{io, ops::ControlFlow, sync::atomic::Ordering, thread, time::Duration};

use anyhow::Context as _;
use kira::Frame;
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
    units::{Time, TimeBase},
};

use super::{
    TrackShared, duration_of_frames,
    track_file::{TrackFile, TrackFileReader},
};
use crate::audio::codecs;

const STEREO: usize = 2;
const RESAMPLER_CHUNK_FRAMES: usize = 1024;
const FULL_BUFFER_WAIT: Duration = Duration::from_millis(50);

/// A container probed by content, since file extensions lie (MP4 files named `.mp3`).
pub(crate) struct ProbedTrack {
    format_reader: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    source_sample_rate: u32,
    time_base: TimeBase,
    duration: Option<Duration>,
    seek_preroll: Duration,
    decoder_delay: Duration,
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
        let decoder_delay = codecs::decoder_delay(parameters, track);
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
        .map(|duration| duration.saturating_sub(decoder_delay));
        let track_id = track.id;
        Ok(Self {
            format_reader,
            decoder,
            track_id,
            source_sample_rate,
            time_base,
            duration,
            seek_preroll,
            decoder_delay,
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
        start_position: Duration,
    ) -> anyhow::Result<TrackDecoder> {
        let skip_frames = if start_position.is_zero() {
            0
        } else {
            self.seek(start_position)?
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
            skip_frames,
            decoded: Vec::new(),
            stereo: Vec::new(),
        })
    }

    /// Decodes the file into stereo frames at its own rate, or returns `None` once it runs past
    /// `frames_max`. Blocks the calling thread.
    pub(crate) fn decode_whole(mut self, frames_max: usize) -> anyhow::Result<Option<Vec<Frame>>> {
        let mut decoded = Vec::new();
        let mut stereo = Vec::new();
        let mut frames = Vec::new();
        while let Some(packet) = self.next_packet()? {
            if packet.track_id != self.track_id {
                continue;
            }
            let buffer = match self.decoder.decode(&packet) {
                Ok(buffer) => buffer,
                Err(errors::Error::DecodeError(reason)) => {
                    tracing::debug!(reason, "skipping an undecodable packet");
                    continue;
                }
                Err(error) => return Err(error).context("decoding a packet"),
            };
            let channel_count = buffer.spec().channels().count();
            buffer.copy_to_vec_interleaved(&mut decoded);
            downmix_to_stereo(&decoded, channel_count, &mut stereo);
            if frames.len() + stereo.len() / STEREO > frames_max {
                return Ok(None);
            }
            frames.extend(
                stereo
                    .as_chunks::<STEREO>()
                    .0
                    .iter()
                    .map(|&[left, right]| Frame::new(left, right)),
            );
        }
        Ok(Some(frames))
    }

    /// Moves to `position` and returns how many source frames the decoder must discard to land
    /// on it exactly, since demuxers seek to a packet boundary before the requested time and some
    /// decoders need a run-up.
    fn seek(&mut self, position: Duration) -> anyhow::Result<u64> {
        let preroll = self.seek_preroll.min(position);
        let target = position.saturating_sub(preroll) + self.decoder_delay;
        let time =
            Time::try_from_secs_f64(target.as_secs_f64()).context("the position overflows")?;
        let seeked = self
            .format_reader
            .seek(
                SeekMode::Accurate,
                SeekTo::Time {
                    time,
                    track_id: Some(self.track_id),
                },
            )
            .context("seeking in the container")?;
        self.decoder.reset();
        let required = self.time_base.calc_time_saturating(seeked.required_ts);
        let actual = self.time_base.calc_time_saturating(seeked.actual_ts);
        let skipped = Duration::try_from_secs_f64(required.as_secs_f64() - actual.as_secs_f64())
            .unwrap_or_default()
            + preroll;
        Ok(duration_to_frames(skipped, self.source_sample_rate))
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
    /// Source frames still to discard after a seek.
    skip_frames: u64,
    decoded: Vec<f32>,
    stereo: Vec<f32>,
}

impl TrackDecoder {
    pub(super) fn run(mut self, mut producer: Producer<Frame>, shared: &TrackShared) {
        let mut writer = FrameWriter {
            producer: &mut producer,
            shared,
        };
        if let Err(error) = self.decode_all(&mut writer) {
            tracing::warn!(error = format!("{error:#}"), "decoding stopped early");
        }
        shared.decoding_finished.store(true, Ordering::Release);
    }

    fn decode_all(&mut self, writer: &mut FrameWriter<'_>) -> anyhow::Result<()> {
        loop {
            if writer.should_stop() {
                return Ok(());
            }
            let packet = match self.track.next_packet()? {
                Some(packet) => packet,
                None if self.restart_for_loop(writer)? => continue,
                None => break,
            };
            if packet.track_id != self.track.track_id {
                continue;
            }
            let buffer = match self.track.decoder.decode(&packet) {
                Ok(buffer) => buffer,
                Err(errors::Error::DecodeError(reason)) => {
                    tracing::debug!(reason, "skipping an undecodable packet");
                    continue;
                }
                Err(error) => return Err(error).context("decoding a packet"),
            };
            let channel_count = buffer.spec().channels().count();
            buffer.copy_to_vec_interleaved(&mut self.decoded);
            downmix_to_stereo(&self.decoded, channel_count, &mut self.stereo);
            let skipped_samples = self.take_skipped_frames(self.stereo.len() / STEREO) * STEREO;
            let stereo = &self.stereo[skipped_samples..];
            let flow = match &mut self.resampler {
                Some(resampler) => resampler.push(stereo, writer),
                None => writer.write(stereo),
            };
            if flow.is_break() {
                return Ok(());
            }
        }
        if let Some(resampler) = &mut self.resampler {
            resampler.finish(writer);
        }
        Ok(())
    }

    /// At the end of the file, starts it again when the track loops, and reports whether it did.
    fn restart_for_loop(&mut self, writer: &FrameWriter<'_>) -> anyhow::Result<bool> {
        if !writer.shared.looping.load(Ordering::Relaxed) {
            return Ok(false);
        }
        // Seeking to the start drops Vorbis's first packet, so probe the file afresh. The
        // resampler keeps its state, so the end joins the start without a click.
        self.track = ProbedTrack::probe(self.file.reader())?;
        self.skip_frames = 0;
        Ok(true)
    }

    /// Consumes up to `available` frames of the pending skip and returns how many to drop.
    fn take_skipped_frames(&mut self, available: usize) -> usize {
        let skipped = usize::try_from(self.skip_frames)
            .unwrap_or(usize::MAX)
            .min(available);
        self.skip_frames = self
            .skip_frames
            .saturating_sub(u64::try_from(skipped).unwrap_or(u64::MAX));
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

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the product is a non-negative frame count far below u64::MAX"
)]
fn duration_to_frames(duration: Duration, sample_rate: u32) -> u64 {
    (duration.as_secs_f64() * f64::from(sample_rate)).round() as u64
}

struct FrameWriter<'a> {
    producer: &'a mut Producer<Frame>,
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
            .map(|&[left, right]| Frame::new(left, right));
        let mut remaining = stereo.len() / STEREO;
        while remaining > 0 {
            if self.should_stop() {
                return ControlFlow::Break(());
            }
            let writable = self.producer.slots().min(remaining);
            if writable == 0 {
                thread::sleep(FULL_BUFFER_WAIT);
                continue;
            }
            let Ok(chunk) = self.producer.write_chunk_uninit(writable) else {
                continue;
            };
            remaining -= chunk.fill_from_iter(frames.by_ref());
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
