//! Ogg Opus files laid out as opusenc writes them: the encoder's delay as the pre-skip, and a last
//! granule position that trims the final packet's padding.

use std::path::Path;

use anyhow::Context as _;

/// Opus's 20 ms frame at 48 kHz.
const FRAME_SAMPLES: usize = 960;
const PACKET_BYTES_MAX: usize = 4_000;
const SERIAL: u32 = 0x7475_666f;
const BEGINNING_OF_STREAM: u8 = 0x02;
const END_OF_STREAM: u8 = 0x04;

/// Encodes interleaved 48 kHz `samples` of one or two channels.
pub fn write_ogg_opus(path: &Path, channels: u8, samples: &[f32]) -> anyhow::Result<()> {
    let layout = match channels {
        1 => opus2::Channels::Mono,
        2 => opus2::Channels::Stereo,
        _ => anyhow::bail!("the fixture writes one or two channels"),
    };
    let mut encoder = opus2::Encoder::new(48_000, layout, opus2::Application::Audio)?;
    let pre_skip = usize::try_from(encoder.get_lookahead()?)?;
    let channel_count = usize::from(channels);
    let frames = samples.len() / channel_count;
    // The encoder's output lags its input by the pre-skip, so silence flushes the tail out.
    let mut padded = samples.to_vec();
    padded.resize(
        (frames + pre_skip).div_ceil(FRAME_SAMPLES) * FRAME_SAMPLES * channel_count,
        0.0,
    );

    let mut ogg = OggStream::default();
    ogg.write_page(
        &opus_head(channels, u16::try_from(pre_skip)?),
        0,
        BEGINNING_OF_STREAM,
    )?;
    ogg.write_page(&opus_tags(), 0, 0)?;
    let packets = padded.chunks(FRAME_SAMPLES * channel_count);
    let packet_count = packets.len();
    for (index, chunk) in packets.enumerate() {
        let packet = encoder.encode_vec_float(chunk, PACKET_BYTES_MAX)?;
        let (granule, flags) = if index + 1 == packet_count {
            (pre_skip + frames, END_OF_STREAM)
        } else {
            ((index + 1) * FRAME_SAMPLES, 0)
        };
        ogg.write_page(&packet, u64::try_from(granule)?, flags)?;
    }
    std::fs::write(path, ogg.bytes).context("writing the Ogg Opus fixture")
}

fn opus_head(channels: u8, pre_skip: u16) -> Vec<u8> {
    let mut head = b"OpusHead".to_vec();
    head.extend_from_slice(&[1, channels]);
    head.extend_from_slice(&pre_skip.to_le_bytes());
    head.extend_from_slice(&48_000_u32.to_le_bytes());
    // No output gain, and channel mapping family 0.
    head.extend_from_slice(&[0, 0, 0]);
    head
}

fn opus_tags() -> Vec<u8> {
    let vendor = b"fixture";
    let mut tags = b"OpusTags".to_vec();
    tags.extend_from_slice(&u32::try_from(vendor.len()).unwrap_or(0).to_le_bytes());
    tags.extend_from_slice(vendor);
    tags.extend_from_slice(&0_u32.to_le_bytes());
    tags
}

/// One logical stream, one packet per page.
#[derive(Default)]
struct OggStream {
    bytes: Vec<u8>,
    sequence: u32,
}

impl OggStream {
    fn write_page(&mut self, packet: &[u8], granule: u64, flags: u8) -> anyhow::Result<()> {
        let mut lacing = vec![255_u8; packet.len() / 255];
        lacing.push(u8::try_from(packet.len() % 255)?);
        let mut page = b"OggS".to_vec();
        page.extend_from_slice(&[0, flags]);
        page.extend_from_slice(&granule.to_le_bytes());
        page.extend_from_slice(&SERIAL.to_le_bytes());
        page.extend_from_slice(&self.sequence.to_le_bytes());
        let checksum_at = page.len();
        page.extend_from_slice(&[0; 4]);
        page.push(u8::try_from(lacing.len())?);
        page.extend_from_slice(&lacing);
        page.extend_from_slice(packet);
        let checksum = ogg_crc(&page);
        page[checksum_at..checksum_at + 4].copy_from_slice(&checksum.to_le_bytes());
        self.bytes.extend_from_slice(&page);
        self.sequence += 1;
        Ok(())
    }
}

/// CRC-32 with polynomial 0x04c11db7, unreflected, from zero.
fn ogg_crc(bytes: &[u8]) -> u32 {
    let mut crc = 0_u32;
    for &byte in bytes {
        crc ^= u32::from(byte) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 == 0 {
                crc << 1
            } else {
                (crc << 1) ^ 0x04c1_1db7
            };
        }
    }
    crc
}
