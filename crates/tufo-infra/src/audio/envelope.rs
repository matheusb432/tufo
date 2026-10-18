//! Gain ramps the audio thread applies frame by frame.

use std::f32::consts::FRAC_PI_2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FadeCurve {
    EqualPower,
    Linear,
}

/// A gain that moves to a target over a whole number of frames.
///
/// Equal-power ramps follow a quarter sine or cosine to preserve power between different
/// tracks. Linear ramps preserve the combined gain when crossfading aligned copies of a track.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Envelope {
    from: f32,
    to: f32,
    frames_total: u32,
    frames_done: u32,
    curve: FadeCurve,
}

impl Envelope {
    pub(crate) const fn constant(gain: f32) -> Self {
        Self {
            from: gain,
            to: gain,
            frames_total: 0,
            frames_done: 0,
            curve: FadeCurve::EqualPower,
        }
    }

    /// Starts at `from` and reaches `to` after `frames`; zero frames jumps straight to `to`.
    pub(crate) fn ramp(from: f32, to: f32, frames: u32) -> Self {
        Self::ramp_with_curve(from, to, frames, FadeCurve::EqualPower)
    }

    pub(super) fn ramp_with_curve(from: f32, to: f32, frames: u32, curve: FadeCurve) -> Self {
        Self {
            from,
            to,
            frames_total: frames,
            frames_done: 0,
            curve,
        }
    }

    /// Ramps from the current gain to `to` over `frames`.
    pub(crate) fn ramp_to(&mut self, to: f32, frames: u32) {
        *self = Self::ramp(self.gain(), to, frames);
    }

    pub(super) fn ramp_to_with_curve(&mut self, to: f32, frames: u32, curve: FadeCurve) {
        *self = Self::ramp_with_curve(self.gain(), to, frames, curve);
    }

    /// The gain for the next frame, advancing the ramp by one frame.
    pub(crate) fn next_gain(&mut self) -> f32 {
        let gain = self.gain();
        if self.frames_done < self.frames_total {
            self.frames_done += 1;
        }
        gain
    }

    /// The gain the ramp has reached.
    pub(crate) fn gain(&self) -> f32 {
        if self.frames_done >= self.frames_total {
            return self.to;
        }
        #[expect(
            clippy::cast_precision_loss,
            reason = "ramps are far shorter than 2^24 frames"
        )]
        let progress = self.frames_done as f32 / self.frames_total as f32;
        let shaped = match self.curve {
            FadeCurve::Linear => progress,
            FadeCurve::EqualPower if self.to >= self.from => (progress * FRAC_PI_2).sin(),
            FadeCurve::EqualPower => 1.0 - (progress * FRAC_PI_2).cos(),
        };
        self.from + (self.to - self.from) * shaped
    }

    /// Whether the ramp has reached its target.
    pub(crate) fn is_settled(&self) -> bool {
        self.frames_done >= self.frames_total
    }

    pub(crate) fn target(&self) -> f32 {
        self.to
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gains(mut envelope: Envelope, frames: usize) -> Vec<f32> {
        (0..frames).map(|_| envelope.next_gain()).collect()
    }

    #[test]
    fn a_crossfade_keeps_constant_power() {
        let fade_out = gains(Envelope::ramp(1.0, 0.0, 100), 101);
        let fade_in = gains(Envelope::ramp(0.0, 1.0, 100), 101);

        for (out, into) in fade_out.iter().zip(&fade_in) {
            assert!((out * out + into * into - 1.0).abs() < 1e-5);
        }
        assert_eq!((fade_out[0], fade_out[100]), (1.0, 0.0));
        assert_eq!((fade_in[0], fade_in[100]), (0.0, 1.0));
    }

    #[test]
    fn ramps_between_partial_levels_never_overshoot() {
        let rising = gains(Envelope::ramp(0.3, 0.8, 50), 51);
        let falling = gains(Envelope::ramp(0.8, 0.3, 50), 51);

        assert!(rising.windows(2).all(|pair| pair[0] <= pair[1]));
        assert!(falling.windows(2).all(|pair| pair[0] >= pair[1]));
        assert!(
            rising
                .iter()
                .chain(&falling)
                .all(|gain| (0.3..=0.8).contains(gain))
        );
    }

    #[test]
    fn a_ramp_redirected_midway_starts_from_where_it_was() {
        let mut envelope = Envelope::ramp(0.0, 1.0, 10);
        for _ in 0..5 {
            envelope.next_gain();
        }
        let midway = envelope.gain();

        envelope.ramp_to(0.0, 10);

        assert_eq!(envelope.next_gain().to_bits(), midway.to_bits());
        assert!(!envelope.is_settled());
    }
}
