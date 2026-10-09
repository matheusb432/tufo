//! Sound effects on the audio thread, decoded in memory or streamed from a decoder like music.

use std::{
    convert::Infallible,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    time::Duration,
};

use kira::{
    Frame,
    info::Info,
    interpolate_frame,
    sound::{Sound, SoundData},
};
use tufo_models::library::SoundSettings;

use super::{
    buffered_track::{BufferedTrack, TrackHandle, TrackReader, frames_in},
    envelope::Envelope,
    sound_cache::DecodedSound,
};

/// How long a volume change takes, so moving a slider doesn't click.
const VOLUME_RAMP: Duration = Duration::from_millis(20);
const NOT_STOPPING: u32 = u32::MAX;

/// Where an effect's frames come from.
pub enum EffectSource {
    Decoded(DecodedSound),
    /// A track started with the effect's looping.
    Streamed(BufferedTrack),
}

/// Wraps `source` as a sound playing with `settings` on a mixer running at `sample_rate`.
#[must_use]
pub fn effect_sound(
    source: EffectSource,
    settings: SoundSettings,
    sample_rate: u32,
) -> (EffectSound, EffectSoundHandle) {
    let volume = settings.volume.into_inner();
    let shared = Arc::new(EffectSoundShared {
        volume_bits: AtomicU32::new(volume.to_bits()),
        stop_fade_frames: AtomicU32::new(NOT_STOPPING),
        looping: AtomicBool::new(settings.looping),
        finished: AtomicBool::new(false),
    });
    let frames = |duration| u32::try_from(frames_in(duration, sample_rate)).unwrap_or(u32::MAX);
    let (source, track) = match source {
        EffectSource::Decoded(sound) => (
            EffectFrames::Decoded(DecodedFrames::new(sound, sample_rate, settings.looping)),
            None,
        ),
        EffectSource::Streamed(track) => (EffectFrames::Streamed(track.reader), Some(track.handle)),
    };
    let sound = EffectSound {
        source,
        shared: Arc::clone(&shared),
        fade: Envelope::ramp(0.0, 1.0, frames(settings.fade_in.into_inner())),
        volume: Envelope::constant(volume),
        volume_ramp_frames: frames(VOLUME_RAMP),
        stopping: false,
        finished: false,
    };
    let handle = EffectSoundHandle {
        shared,
        track,
        sample_rate,
    };
    (sound, handle)
}

/// Controls a playing [`EffectSound`] from outside the audio thread.
pub struct EffectSoundHandle {
    shared: Arc<EffectSoundShared>,
    /// Decoded sounds loop through `shared` instead.
    track: Option<TrackHandle>,
    sample_rate: u32,
}

impl EffectSoundHandle {
    /// Fades the sound out over `fade`, then ends it.
    pub fn stop(&self, fade: Duration) {
        let frames = u32::try_from(frames_in(fade, self.sample_rate)).unwrap_or(NOT_STOPPING - 1);
        self.shared
            .stop_fade_frames
            .store(frames.min(NOT_STOPPING - 1), Ordering::Relaxed);
    }

    /// Moves the gain to the settings' volume over a few milliseconds and switches looping.
    /// Turning looping off lets the current pass finish.
    pub fn apply(&self, settings: SoundSettings) {
        self.shared
            .volume_bits
            .store(settings.volume.into_inner().to_bits(), Ordering::Relaxed);
        self.shared
            .looping
            .store(settings.looping, Ordering::Relaxed);
        if let Some(track) = &self.track {
            track.set_looping(settings.looping);
        }
    }

    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.shared.finished.load(Ordering::Acquire)
    }
}

struct EffectSoundShared {
    volume_bits: AtomicU32,
    stop_fade_frames: AtomicU32,
    looping: AtomicBool,
    finished: AtomicBool,
}

pub struct EffectSound {
    source: EffectFrames,
    shared: Arc<EffectSoundShared>,
    /// Fades in at the start and out when stopped.
    fade: Envelope,
    volume: Envelope,
    volume_ramp_frames: u32,
    stopping: bool,
    finished: bool,
}

impl EffectSound {
    fn apply_controls(&mut self) {
        let volume = f32::from_bits(self.shared.volume_bits.load(Ordering::Relaxed));
        if volume.to_bits() != self.volume.target().to_bits() {
            self.volume.ramp_to(volume, self.volume_ramp_frames);
        }
        let stop_fade_frames = self.shared.stop_fade_frames.load(Ordering::Relaxed);
        if !self.stopping && stop_fade_frames != NOT_STOPPING {
            self.stopping = true;
            self.fade.ramp_to(0.0, stop_fade_frames);
        }
    }

    fn finish(&mut self) {
        self.finished = true;
        self.shared.finished.store(true, Ordering::Release);
    }
}

impl SoundData for EffectSound {
    type Error = Infallible;
    type Handle = ();

    fn into_sound(self) -> Result<(Box<dyn Sound>, Self::Handle), Self::Error> {
        Ok((Box::new(self), ()))
    }
}

impl Sound for EffectSound {
    fn process(&mut self, out: &mut [Frame], _dt: f64, _info: &Info) {
        if self.finished {
            out.fill(Frame::ZERO);
            return;
        }
        self.apply_controls();
        let looping = self.shared.looping.load(Ordering::Relaxed);
        let copied = self.source.read(out, looping);
        out[copied..].fill(Frame::ZERO);
        for frame in &mut out[..copied] {
            *frame *= self.fade.next_gain() * self.volume.next_gain();
        }
        let ended = copied < out.len() && self.source.is_exhausted();
        let faded_out = self.stopping && self.fade.is_settled();
        if ended || faded_out {
            self.finish();
        }
    }

    fn finished(&self) -> bool {
        self.finished
    }
}

enum EffectFrames {
    Decoded(DecodedFrames),
    Streamed(TrackReader),
}

impl EffectFrames {
    /// Copies up to `out.len()` frames into the start of `out` and returns how many it copied.
    fn read(&mut self, out: &mut [Frame], looping: bool) -> usize {
        match self {
            Self::Decoded(frames) => frames.read(out, looping),
            Self::Streamed(reader) => reader.read(out),
        }
    }

    fn is_exhausted(&self) -> bool {
        match self {
            Self::Decoded(frames) => frames.is_exhausted(),
            Self::Streamed(reader) => reader.is_exhausted(),
        }
    }
}

/// Converts a decoded sound to the mixer's rate as kira's static sounds do, by 4-point Hermite
/// interpolation, reading on from the sound's start while looping so the seam stays smooth.
struct DecodedFrames {
    sound: DecodedSound,
    source_frames_per_output_frame: f64,
    /// Output lies `fraction` of the way from the second frame to the third.
    window: [Frame; 4],
    fraction: f64,
    next_frame: usize,
    /// False once the last frame entered the window without looping.
    reading: bool,
    /// Frames to shift in before the window holds only the silence after the end.
    shifts_until_silent: u8,
}

impl DecodedFrames {
    fn new(sound: DecodedSound, output_sample_rate: u32, looping: bool) -> Self {
        let mut frames = Self {
            source_frames_per_output_frame: f64::from(sound.sample_rate())
                / f64::from(output_sample_rate),
            window: [Frame::ZERO; 4],
            fraction: 0.0,
            next_frame: 0,
            reading: !sound.frames().is_empty(),
            shifts_until_silent: 0,
            sound,
        };
        // Silence precedes the first frame, so the first output is exactly that frame.
        for _ in 1..frames.window.len() {
            frames.shift_in_next_frame(looping);
        }
        frames
    }

    fn read(&mut self, out: &mut [Frame], looping: bool) -> usize {
        for (index, frame) in out.iter_mut().enumerate() {
            if self.is_exhausted() {
                return index;
            }
            let [previous, current, next, after_next] = self.window;
            #[expect(
                clippy::cast_possible_truncation,
                reason = "the fraction stays between 0 and 1"
            )]
            let fraction = self.fraction as f32;
            *frame = interpolate_frame(previous, current, next, after_next, fraction);
            self.fraction += self.source_frames_per_output_frame;
            while self.fraction >= 1.0 {
                self.fraction -= 1.0;
                self.shift_in_next_frame(looping);
            }
        }
        out.len()
    }

    fn shift_in_next_frame(&mut self, looping: bool) {
        self.window.rotate_left(1);
        let frames = self.sound.frames();
        let entering = if self.reading {
            frames.get(self.next_frame).copied()
        } else {
            None
        };
        if let Some(entering) = entering {
            self.window[3] = Frame::from(entering);
            self.shifts_until_silent = 4;
            self.next_frame += 1;
            if self.next_frame >= frames.len() {
                self.next_frame = 0;
                self.reading = looping;
            }
        } else {
            self.window[3] = Frame::ZERO;
            self.shifts_until_silent = self.shifts_until_silent.saturating_sub(1);
        }
    }

    fn is_exhausted(&self) -> bool {
        !self.reading && self.shifts_until_silent == 0
    }
}

#[cfg(test)]
mod tests {
    use kira::{
        Frame,
        info::MockInfoBuilder,
        sound::static_sound::{StaticSoundData, StaticSoundSettings},
    };

    use super::*;
    use crate::audio::packed_frame::PackedFrame;

    const SOURCE_SAMPLE_RATE: u32 = 8_000;
    const OUTPUT_SAMPLE_RATE: u32 = 44_100;

    /// Packed and back again, so kira's sound plays the same samples.
    fn tone(frames: usize) -> Vec<Frame> {
        (0..frames)
            .map(|index| {
                #[expect(clippy::cast_precision_loss, reason = "fixtures stay short")]
                let phase = index as f32 * 0.3;
                Frame::from(PackedFrame::new(phase.sin() * 0.5, phase.cos() * 0.5))
            })
            .collect()
    }

    fn kira_output(frames: &[Frame], looping: bool, output_frames: usize) -> Vec<Frame> {
        let settings = StaticSoundSettings::default();
        let data = StaticSoundData {
            sample_rate: SOURCE_SAMPLE_RATE,
            frames: frames.into(),
            settings: if looping {
                settings.loop_region(..)
            } else {
                settings
            },
            slice: None,
        };
        let (mut sound, _handle) = data.into_sound().unwrap();
        let mut out = vec![Frame::ZERO; output_frames];
        let dt = 1.0 / f64::from(OUTPUT_SAMPLE_RATE);
        let info = MockInfoBuilder::new().build();
        for chunk in out.chunks_mut(128) {
            sound.on_start_processing();
            sound.process(chunk, dt, &info);
        }
        out
    }

    fn decoded_output(frames: &[Frame], looping: bool, output_frames: usize) -> Vec<Frame> {
        let packed = frames
            .iter()
            .map(|&frame| PackedFrame::from(frame))
            .collect();
        let sound = DecodedSound::new(SOURCE_SAMPLE_RATE, packed);
        let mut decoded = DecodedFrames::new(sound, OUTPUT_SAMPLE_RATE, looping);
        let mut out = vec![Frame::ZERO; output_frames];
        for chunk in out.chunks_mut(128) {
            decoded.read(chunk, looping);
        }
        out
    }

    fn largest_difference(left: &[Frame], right: &[Frame]) -> f32 {
        left.iter()
            .zip(right)
            .map(|(left, right)| {
                (left.left - right.left)
                    .abs()
                    .max((left.right - right.right).abs())
            })
            .fold(0.0, f32::max)
    }

    #[test]
    fn a_looping_sound_matches_kira_across_its_seams() {
        let frames = tone(1_000);
        let output_frames = 3 * 1_000 * 44_100 / 8_000;

        let difference = largest_difference(
            &decoded_output(&frames, true, output_frames),
            &kira_output(&frames, true, output_frames),
        );

        assert!(difference < 1e-5, "differs by {difference}");
    }

    #[test]
    fn a_one_shot_matches_kira_and_ends_after_its_last_frame() {
        let frames = tone(1_000);
        let output_frames = 1_000 * 44_100 / 8_000 + 100;
        let sound = DecodedSound::new(
            SOURCE_SAMPLE_RATE,
            frames
                .iter()
                .map(|&frame| PackedFrame::from(frame))
                .collect(),
        );
        let mut decoded = DecodedFrames::new(sound, OUTPUT_SAMPLE_RATE, false);
        let mut out = vec![Frame::ZERO; output_frames];

        let copied = decoded.read(&mut out, false);

        assert!(copied < output_frames);
        assert!(decoded.is_exhausted());
        let difference = largest_difference(&out, &kira_output(&frames, false, output_frames));
        assert!(difference < 1e-5, "differs by {difference}");
    }
}
