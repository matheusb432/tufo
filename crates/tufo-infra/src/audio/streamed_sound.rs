//! Sound effects too long to keep decoded in memory, streamed from a decoder like music.

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
    sound::{Sound, SoundData},
};

use super::{
    buffered_track::{BufferedTrack, TrackHandle, TrackReader, frames_in},
    envelope::Envelope,
};

/// How long a volume change takes, so moving a slider doesn't click.
const VOLUME_RAMP: Duration = Duration::from_millis(20);
const NOT_STOPPING: u32 = u32::MAX;

/// Wraps a started track as a sound that fades in over `fade_in` at `volume`, a linear gain.
pub(crate) fn streamed_sound(
    track: BufferedTrack,
    volume: f32,
    fade_in: Duration,
    sample_rate: u32,
) -> (StreamedSound, StreamedSoundHandle) {
    let shared = Arc::new(StreamedSoundShared {
        volume_bits: AtomicU32::new(volume.to_bits()),
        stop_fade_frames: AtomicU32::new(NOT_STOPPING),
        finished: AtomicBool::new(false),
    });
    let frames = |duration| u32::try_from(frames_in(duration, sample_rate)).unwrap_or(u32::MAX);
    let sound = StreamedSound {
        reader: track.reader,
        shared: Arc::clone(&shared),
        fade: Envelope::ramp(0.0, 1.0, frames(fade_in)),
        volume: Envelope::constant(volume),
        volume_ramp_frames: frames(VOLUME_RAMP),
        stopping: false,
        finished: false,
    };
    let handle = StreamedSoundHandle {
        shared,
        track: track.handle,
        sample_rate,
    };
    (sound, handle)
}

/// Controls a playing [`StreamedSound`] from outside the audio thread.
pub struct StreamedSoundHandle {
    shared: Arc<StreamedSoundShared>,
    track: TrackHandle,
    sample_rate: u32,
}

impl StreamedSoundHandle {
    /// Fades the sound out over `fade`, then ends it.
    pub fn stop(&self, fade: Duration) {
        let frames = u32::try_from(frames_in(fade, self.sample_rate)).unwrap_or(NOT_STOPPING - 1);
        self.shared
            .stop_fade_frames
            .store(frames.min(NOT_STOPPING - 1), Ordering::Relaxed);
    }

    /// Moves the gain to `volume`, a linear gain, over a few milliseconds.
    pub fn set_volume(&self, volume: f32) {
        self.shared
            .volume_bits
            .store(volume.to_bits(), Ordering::Relaxed);
    }

    pub fn set_looping(&self, looping: bool) {
        self.track.set_looping(looping);
    }

    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.shared.finished.load(Ordering::Acquire)
    }
}

struct StreamedSoundShared {
    volume_bits: AtomicU32,
    stop_fade_frames: AtomicU32,
    finished: AtomicBool,
}

pub(crate) struct StreamedSound {
    reader: TrackReader,
    shared: Arc<StreamedSoundShared>,
    /// Fades in at the start and out when stopped.
    fade: Envelope,
    volume: Envelope,
    volume_ramp_frames: u32,
    stopping: bool,
    finished: bool,
}

impl StreamedSound {
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

impl SoundData for StreamedSound {
    type Error = Infallible;
    type Handle = ();

    fn into_sound(self) -> Result<(Box<dyn Sound>, Self::Handle), Self::Error> {
        Ok((Box::new(self), ()))
    }
}

impl Sound for StreamedSound {
    fn process(&mut self, out: &mut [Frame], _dt: f64, _info: &Info) {
        if self.finished {
            out.fill(Frame::ZERO);
            return;
        }
        self.apply_controls();
        let copied = self.reader.read(out);
        out[copied..].fill(Frame::ZERO);
        for frame in &mut out[..copied] {
            *frame *= self.fade.next_gain() * self.volume.next_gain();
        }
        let ended = copied < out.len() && self.reader.is_exhausted();
        let faded_out = self.stopping && self.fade.is_settled();
        if ended || faded_out {
            self.finish();
        }
    }

    fn finished(&self) -> bool {
        self.finished
    }
}
