//! A brickwall limiter on the main track, so music plus overlapping sounds never clip.

use std::time::Duration;

use kira::{
    Frame,
    effect::{Effect, EffectBuilder},
    info::Info,
};

/// The loudest sample the limiter lets through: −0.3 dBFS.
pub const LIMITER_CEILING: f32 = 0.966_051_9;
/// How long the gain takes to recover about two thirds of the way after a peak.
const RELEASE_TIME: Duration = Duration::from_millis(150);

/// Adds a [`Limiter`] to a mixer track.
pub struct LimiterBuilder;

impl EffectBuilder for LimiterBuilder {
    type Handle = ();

    fn build(self) -> (Box<dyn Effect>, Self::Handle) {
        (
            Box::new(Limiter {
                gain: 1.0,
                release_step: 0.0,
            }),
            (),
        )
    }
}

/// Lowers the gain at once when a frame would exceed [`LIMITER_CEILING`], then lets it recover
/// exponentially. The attack is instant, so no sample ever passes the ceiling.
struct Limiter {
    gain: f32,
    /// The fraction of the remaining distance to full gain recovered per frame.
    release_step: f32,
}

impl Limiter {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "the step only needs f32 precision"
    )]
    fn set_sample_rate(&mut self, sample_rate: u32) {
        let release_frames = RELEASE_TIME.as_secs_f64() * f64::from(sample_rate);
        self.release_step = (1.0 - (-1.0 / release_frames).exp()) as f32;
    }
}

impl Effect for Limiter {
    fn init(&mut self, sample_rate: u32, _internal_buffer_size: usize) {
        self.set_sample_rate(sample_rate);
    }

    fn on_change_sample_rate(&mut self, sample_rate: u32) {
        self.set_sample_rate(sample_rate);
    }

    fn process(&mut self, input: &mut [Frame], _dt: f64, _info: &Info) {
        for frame in input {
            if !(frame.left.is_finite() && frame.right.is_finite()) {
                *frame = Frame::ZERO;
                continue;
            }
            let peak = frame.left.abs().max(frame.right.abs());
            self.gain += (1.0 - self.gain) * self.release_step;
            if peak * self.gain > LIMITER_CEILING {
                self.gain = LIMITER_CEILING / peak;
            }
            // Clamping only removes the division's rounding error.
            *frame = Frame::new(
                (frame.left * self.gain).clamp(-LIMITER_CEILING, LIMITER_CEILING),
                (frame.right * self.gain).clamp(-LIMITER_CEILING, LIMITER_CEILING),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use kira::info::MockInfoBuilder;

    use super::*;

    const SAMPLE_RATE: u32 = 48_000;

    fn limiter() -> Box<dyn Effect> {
        let (mut limiter, ()) = LimiterBuilder.build();
        limiter.init(SAMPLE_RATE, 128);
        limiter
    }

    fn process(limiter: &mut dyn Effect, frames: &mut [Frame]) {
        let info = MockInfoBuilder::new().build();
        limiter.process(frames, 1.0 / f64::from(SAMPLE_RATE), &info);
    }

    #[test]
    fn peaks_never_pass_the_ceiling() {
        let mut limiter = limiter();
        let mut frames = (0..4_800)
            .map(|index| {
                let loud = if index % 7 == 0 { 3.5 } else { 1.2 };
                Frame::new(loud, -loud * 0.5)
            })
            .collect::<Vec<_>>();

        process(limiter.as_mut(), &mut frames);

        assert!(
            frames
                .iter()
                .all(|frame| frame.left.abs().max(frame.right.abs()) <= LIMITER_CEILING)
        );
    }

    #[test]
    fn quiet_audio_passes_unchanged_and_gain_recovers_after_a_peak() {
        let mut limiter = limiter();
        let quiet = Frame::new(0.5, -0.25);
        let mut before = vec![quiet; 480];
        let mut peak = vec![Frame::new(2.0, 2.0)];
        let mut after = vec![quiet; 48_000];

        process(limiter.as_mut(), &mut before);
        process(limiter.as_mut(), &mut peak);
        process(limiter.as_mut(), &mut after);

        assert!(before.iter().all(|frame| *frame == quiet));
        assert!(after[0].left < 0.3);
        assert!((after[after.len() - 1].left - quiet.left).abs() < 1e-3);
    }

    #[test]
    fn non_finite_samples_become_silence() {
        let mut limiter = limiter();
        let mut frames = vec![Frame::new(f32::NAN, 0.1), Frame::new(0.2, 0.2)];

        process(limiter.as_mut(), &mut frames);

        assert_eq!(frames[0], Frame::ZERO);
        assert_eq!(frames[1], Frame::new(0.2, 0.2));
    }
}
