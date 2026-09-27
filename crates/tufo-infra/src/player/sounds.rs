//! Sound effects the engine plays over the music.

use std::time::{Duration, SystemTime};

use kira::{
    StartTime, Tween,
    sound::{
        PlaybackState, Region,
        static_sound::{StaticSoundData, StaticSoundHandle},
    },
};
use tufo_models::{
    library::{SoundSettings, Volume},
    playback::player_state::{ActiveSound, SoundCue},
};

use crate::audio::{decibels, streamed_sound::StreamedSoundHandle};

/// How long a volume change on a playing sound takes.
const SOUND_VOLUME_TWEEN: Duration = Duration::from_millis(20);

/// A sound the engine fired, kept until it ends. Only looping sounds survive a change of output,
/// with `playback` absent until the new output plays them again.
pub(super) struct FiredSound {
    pub(super) cue: SoundCue,
    pub(super) started_at: SystemTime,
    pub(super) duration: Duration,
    pub(super) playback: Option<SoundPlayback>,
}

impl FiredSound {
    pub(super) fn active_sound(&self) -> ActiveSound {
        ActiveSound {
            entry_id: self.cue.entry_id,
            started_at: self.started_at,
            duration: self.duration,
            looping: self.cue.settings.looping,
        }
    }
}

/// A playing sound, decoded in memory or streamed.
pub(super) enum SoundPlayback {
    Decoded(StaticSoundHandle),
    Streamed(StreamedSoundHandle),
}

impl SoundPlayback {
    pub(super) fn stop(&mut self, fade: Duration) {
        match self {
            Self::Decoded(handle) => handle.stop(tween(fade)),
            Self::Streamed(handle) => handle.stop(fade),
        }
    }

    pub(super) fn apply(&mut self, settings: SoundSettings) {
        match self {
            Self::Decoded(handle) => {
                handle.set_volume(
                    decibels(settings.volume.into_inner()),
                    tween(SOUND_VOLUME_TWEEN),
                );
                handle.set_loop_region(settings.looping.then_some(Region::from(..)));
            }
            Self::Streamed(handle) => {
                handle.set_volume(settings.volume.into_inner());
                handle.set_looping(settings.looping);
            }
        }
    }

    pub(super) fn is_finished(&self) -> bool {
        match self {
            Self::Decoded(handle) => handle.state() == PlaybackState::Stopped,
            Self::Streamed(handle) => handle.is_finished(),
        }
    }
}

/// `data` set up to play with `settings`.
pub(super) fn decoded_sound(data: &StaticSoundData, settings: SoundSettings) -> StaticSoundData {
    let fade_in = settings.fade_in.into_inner();
    let mut data = data.volume(decibels(settings.volume.into_inner()));
    if !fade_in.is_zero() {
        data = data.fade_in_tween(tween(fade_in));
    }
    if settings.looping {
        data = data.loop_region(..);
    }
    data
}

pub(super) fn volume_gain(volume: Volume, muted: bool) -> f32 {
    if muted { 0.0 } else { volume.into_inner() }
}

pub(super) fn tween(duration: Duration) -> Tween {
    Tween {
        start_time: StartTime::Immediate,
        duration,
        easing: kira::Easing::Linear,
    }
}
