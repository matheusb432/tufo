//! Sound effects the engine plays over the music.

use std::time::{Duration, SystemTime};

use tufo_models::{
    library::Volume,
    playback::player_state::{ActiveSound, SoundCue},
};

use crate::audio::effect_sound::EffectSoundHandle;

/// A sound the engine fired, kept until it ends. Only looping sounds survive a change of output,
/// with `playback` absent until the new output plays them again.
pub(super) struct FiredSound {
    pub(super) cue: SoundCue,
    pub(super) started_at: SystemTime,
    pub(super) duration: Duration,
    pub(super) playback: Option<EffectSoundHandle>,
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

pub(super) fn volume_gain(volume: Volume, muted: bool) -> f32 {
    if muted { 0.0 } else { volume.into_inner() }
}
