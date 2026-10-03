//! Opus through libopus.

use std::{
    sync::{Mutex, PoisonError},
    time::Duration,
};

use symphonia::core::{
    audio::{
        AsGenericAudioBufferRef as _, AudioBuffer, AudioMut as _, AudioSpec, Channels,
        GenericAudioBufferRef,
    },
    codecs::{
        CodecInfo,
        audio::{
            AudioCodecParameters, AudioDecoder, AudioDecoderOptions, FinalizeResult,
            well_known::CODEC_ID_OPUS,
        },
        registry::{RegisterableAudioDecoder, SupportedAudioCodec},
    },
    errors::{Result, decode_error, unsupported_error},
    packet::PacketRef,
    units,
};

/// Opus decodes to 48 kHz whatever rate it was encoded from.
const SAMPLE_RATE: u32 = 48_000;
/// The longest packet: 120 ms.
const PACKET_FRAMES_MAX: usize = 5_760;
/// RFC 7845 asks for at least 80 ms; 400 ms matches continuous decoding, so loop seams stay exact.
pub(super) const SEEK_PREROLL: Duration = Duration::from_millis(400);
const SUPPORTED_CODECS: &[SupportedAudioCodec] = &[SupportedAudioCodec {
    id: CODEC_ID_OPUS,
    info: CodecInfo {
        short_name: "opus",
        long_name: "Opus",
        profiles: &[],
    },
}];
/// Where each channel of the Vorbis order lands in symphonia's canonical order, by channel count.
const VORBIS_ORDER_PLANES: [&[usize]; 8] = [
    &[0],
    &[0, 1],
    &[0, 2, 1],
    &[0, 1, 2, 3],
    &[0, 2, 1, 3, 4],
    &[0, 2, 1, 4, 5, 3],
    &[0, 2, 1, 5, 6, 4, 3],
    &[0, 2, 1, 6, 7, 4, 5, 3],
];
const DISCRETE_PLANES: [usize; 8] = [0, 1, 2, 3, 4, 5, 6, 7];

pub(super) struct OpusDecoder {
    params: AudioCodecParameters,
    /// Only reached through `&mut self`; the mutex makes the decoder `Sync`, as symphonia needs.
    streams: Mutex<Streams>,
    plane_of_channel: &'static [usize],
    gapless: bool,
    /// Frames of the encoder's delay still to drop; symphonia's Ogg reader leaves them in.
    pre_skip_remaining: usize,
    interleaved: Vec<f32>,
    buffer: AudioBuffer<f32>,
}

impl OpusDecoder {
    fn try_new(params: &AudioCodecParameters, options: AudioDecoderOptions) -> Result<Self> {
        let Some(head) = params.extra_data.as_deref().and_then(OpusHead::parse) else {
            return unsupported_error("opus: missing or unsupported identification header");
        };
        let channel_count = head.mapping.channel_count();
        let (channels, plane_of_channel) = match &params.channels {
            Some(channels) if channels.count() != usize::from(channel_count) => {
                return decode_error("opus: the header and the track disagree on channels");
            }
            Some(channels @ Channels::Positioned(_)) => (
                channels.clone(),
                VORBIS_ORDER_PLANES[usize::from(channel_count) - 1],
            ),
            _ => (
                Channels::Discrete(u16::from(channel_count)),
                &DISCRETE_PLANES[..usize::from(channel_count)],
            ),
        };
        let mut streams = Streams::new(&head.mapping)?;
        if streams.set_gain(head.output_gain_q8_db).is_err() {
            return decode_error("opus: libopus rejected the output gain");
        }
        Ok(Self {
            params: params.clone(),
            streams: Mutex::new(streams),
            plane_of_channel,
            gapless: options.gapless,
            pre_skip_remaining: usize::from(head.pre_skip),
            interleaved: vec![0.0; PACKET_FRAMES_MAX * usize::from(channel_count)],
            buffer: AudioBuffer::new(AudioSpec::new(SAMPLE_RATE, channels), PACKET_FRAMES_MAX),
        })
    }

    fn decode_into_buffer(&mut self, packet: &PacketRef<'_>) -> Result<()> {
        let streams = self
            .streams
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner);
        let Ok(frames) = streams.decode(packet.data, &mut self.interleaved) else {
            return decode_error("opus: libopus rejected a packet");
        };
        self.buffer.resize_uninit(frames);
        let channel_count = self.plane_of_channel.len();
        for (channel, &plane_index) in self.plane_of_channel.iter().enumerate() {
            let Some(plane) = self.buffer.plane_mut(plane_index) else {
                return decode_error("opus: missing audio plane");
            };
            let samples = self.interleaved.iter().skip(channel).step_by(channel_count);
            for (sample, &decoded) in plane.iter_mut().zip(samples) {
                *sample = decoded;
            }
        }
        if self.gapless {
            let trim_end = frames_of(packet.trim_end);
            let pre_skip = self.pre_skip_remaining.min(frames.saturating_sub(trim_end));
            self.pre_skip_remaining -= pre_skip;
            self.buffer
                .trim(frames_of(packet.trim_start) + pre_skip, trim_end);
        }
        Ok(())
    }
}

impl AudioDecoder for OpusDecoder {
    /// Seeks discard the pre-skip with the rest of their run-up, so a reset drops it too.
    fn reset(&mut self) {
        self.pre_skip_remaining = 0;
        let streams = self
            .streams
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner);
        if let Err(error) = streams.reset() {
            tracing::warn!(%error, "resetting the Opus decoder failed");
        }
    }

    fn codec_info(&self) -> &CodecInfo {
        &SUPPORTED_CODECS[0].info
    }

    fn codec_params(&self) -> &AudioCodecParameters {
        &self.params
    }

    fn decode_ref(&mut self, packet: &PacketRef<'_>) -> Result<GenericAudioBufferRef<'_>> {
        self.buffer.clear();
        if let Err(error) = self.decode_into_buffer(packet) {
            self.buffer.clear();
            return Err(error);
        }
        Ok(self.buffer.as_generic_audio_buffer_ref())
    }

    fn finalize(&mut self) -> FinalizeResult {
        FinalizeResult::default()
    }

    fn last_decoded(&self) -> GenericAudioBufferRef<'_> {
        self.buffer.as_generic_audio_buffer_ref()
    }
}

impl RegisterableAudioDecoder for OpusDecoder {
    fn try_registry_new(
        params: &AudioCodecParameters,
        options: &AudioDecoderOptions,
    ) -> Result<Box<dyn AudioDecoder>> {
        Ok(Box::new(Self::try_new(params, *options)?))
    }

    fn supported_codecs() -> &'static [SupportedAudioCodec] {
        SUPPORTED_CODECS
    }
}

enum Streams {
    Single(opus2::Decoder),
    Multi(opus2::MSDecoder),
}

impl Streams {
    fn new(mapping: &ChannelMapping) -> Result<Self> {
        let streams = match mapping {
            ChannelMapping::Single { stereo: false } => {
                opus2::Decoder::new(SAMPLE_RATE, opus2::Channels::Mono).map(Self::Single)
            }
            ChannelMapping::Single { stereo: true } => {
                opus2::Decoder::new(SAMPLE_RATE, opus2::Channels::Stereo).map(Self::Single)
            }
            ChannelMapping::Vorbis {
                stream_count,
                coupled_count,
                stream_of_channel,
            } => opus2::MSDecoder::new(
                SAMPLE_RATE,
                *stream_count,
                *coupled_count,
                stream_of_channel,
            )
            .map(Self::Multi),
        };
        streams.or_else(|_| unsupported_error("opus: libopus rejected the stream layout"))
    }

    /// Returns the frames decoded into `interleaved`.
    fn decode(&mut self, packet: &[u8], interleaved: &mut [f32]) -> opus2::Result<usize> {
        match self {
            Self::Single(decoder) => decoder.decode_float(packet, interleaved, false),
            Self::Multi(decoder) => decoder.decode_float(packet, interleaved, false),
        }
    }

    fn reset(&mut self) -> opus2::Result<()> {
        match self {
            Self::Single(decoder) => decoder.reset_state(),
            Self::Multi(decoder) => decoder.reset_state(),
        }
    }

    fn set_gain(&mut self, gain_q8_db: i16) -> opus2::Result<()> {
        match self {
            Self::Single(decoder) => decoder.set_gain(i32::from(gain_q8_db)),
            Self::Multi(decoder) => decoder.set_gain(i32::from(gain_q8_db)),
        }
    }
}

/// The identification header, RFC 7845 section 5.1.
struct OpusHead {
    pre_skip: u16,
    /// In 1/256 dB.
    output_gain_q8_db: i16,
    mapping: ChannelMapping,
}

enum ChannelMapping {
    /// Family 0: one mono or stereo stream.
    Single { stereo: bool },
    /// Family 1: up to eight channels in Vorbis order, spread over several streams.
    Vorbis {
        stream_count: u8,
        coupled_count: u8,
        stream_of_channel: Vec<u8>,
    },
}

impl ChannelMapping {
    fn channel_count(&self) -> u8 {
        match self {
            Self::Single { stereo } => 1 + u8::from(*stereo),
            // Parsing bounds the table to eight channels.
            Self::Vorbis {
                stream_of_channel, ..
            } => u8::try_from(stream_of_channel.len()).unwrap_or(u8::MAX),
        }
    }
}

impl OpusHead {
    const MAGIC: &[u8] = b"OpusHead";
    const VORBIS_CHANNELS_MAX: u8 = 8;

    fn parse(header: &[u8]) -> Option<Self> {
        if !header.starts_with(Self::MAGIC) {
            return None;
        }
        let channel_count = *header.get(9)?;
        let pre_skip = u16::from_le_bytes([*header.get(10)?, *header.get(11)?]);
        let output_gain_q8_db = i16::from_le_bytes([*header.get(16)?, *header.get(17)?]);
        let mapping = match *header.get(18)? {
            0 if matches!(channel_count, 1 | 2) => ChannelMapping::Single {
                stereo: channel_count == 2,
            },
            1 if (1..=Self::VORBIS_CHANNELS_MAX).contains(&channel_count) => {
                let stream_count = *header.get(19)?;
                let coupled_count = *header.get(20)?;
                if stream_count == 0 || coupled_count > stream_count {
                    return None;
                }
                let stream_of_channel = header.get(21..21 + usize::from(channel_count))?.to_vec();
                ChannelMapping::Vorbis {
                    stream_count,
                    coupled_count,
                    stream_of_channel,
                }
            }
            _ => return None,
        };
        Some(Self {
            pre_skip,
            output_gain_q8_db,
            mapping,
        })
    }
}

fn frames_of(duration: units::Duration) -> usize {
    usize::try_from(duration.get()).unwrap_or(usize::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_head_parses_stereo_and_surround_layouts_and_rejects_the_rest() {
        let stereo = [
            b"OpusHead".as_slice(),
            &[1, 2, 56, 1, 128, 187, 0, 0, 0, 1, 0],
        ]
        .concat();
        let head = OpusHead::parse(&stereo).unwrap();
        assert_eq!(head.pre_skip, 312);
        assert_eq!(head.output_gain_q8_db, 256);
        assert!(matches!(
            head.mapping,
            ChannelMapping::Single { stereo: true }
        ));

        let surround = [
            b"OpusHead".as_slice(),
            &[1, 6, 56, 1, 128, 187, 0, 0, 0, 0, 1, 4, 2, 0, 4, 1, 2, 3, 5],
        ]
        .concat();
        let head = OpusHead::parse(&surround).unwrap();
        assert_eq!(head.mapping.channel_count(), 6);

        let three_channels_in_family_0 = [
            b"OpusHead".as_slice(),
            &[1, 3, 56, 1, 128, 187, 0, 0, 0, 0, 0],
        ]
        .concat();
        assert!(OpusHead::parse(&three_channels_in_family_0).is_none());
        let truncated_table = [
            b"OpusHead".as_slice(),
            &[1, 6, 56, 1, 128, 187, 0, 0, 0, 0, 1, 4, 2, 0],
        ]
        .concat();
        assert!(OpusHead::parse(&truncated_table).is_none());
    }
}
