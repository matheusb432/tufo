//! Stereo frames held in half the memory of kira's, for audio decoded ahead of playback.

use kira::Frame;

/// The gap between neighbouring stored sample values. Samples span ±2.0: decoders and resamplers
/// overshoot full scale, and the limiter, not the storage, should tame that.
pub const PACKED_SAMPLE_STEP: f32 = 1.0 / 16_384.0;

/// A stereo frame as two 16-bit samples, whose rounding noise lies about 95 dB below full scale.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PackedFrame {
    left: i16,
    right: i16,
}

impl PackedFrame {
    #[must_use]
    pub fn new(left: f32, right: f32) -> Self {
        Self {
            left: pack_sample(left),
            right: pack_sample(right),
        }
    }
}

impl From<Frame> for PackedFrame {
    fn from(frame: Frame) -> Self {
        Self::new(frame.left, frame.right)
    }
}

impl From<PackedFrame> for Frame {
    fn from(frame: PackedFrame) -> Self {
        Self::new(unpack_sample(frame.left), unpack_sample(frame.right))
    }
}

/// Copies `packed` into the start of `out`, which must be at least as long.
pub(crate) fn unpack_into(packed: &[PackedFrame], out: &mut [Frame]) {
    for (out, packed) in out.iter_mut().zip(packed) {
        *out = Frame::from(*packed);
    }
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "the cast saturates out-of-range samples and maps NaN to silence"
)]
fn pack_sample(sample: f32) -> i16 {
    (sample / PACKED_SAMPLE_STEP).round() as i16
}

fn unpack_sample(sample: i16) -> f32 {
    f32::from(sample) * PACKED_SAMPLE_STEP
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_round_to_the_nearest_step_and_keep_overshoots() {
        let frame = Frame::from(PackedFrame::new(0.25 + PACKED_SAMPLE_STEP * 0.4, -1.5));

        assert_eq!(frame, Frame::new(0.25, -1.5));
    }

    #[test]
    fn samples_past_the_range_saturate_and_nan_becomes_silence() {
        let frame = Frame::from(PackedFrame::new(3.0, f32::NAN));

        assert_eq!(frame, Frame::new(32_767.0 * PACKED_SAMPLE_STEP, 0.0));
    }
}
