//! The process-neutral contract for changing the player's settings.

use tufo_models::{
    library::Volume,
    playback::{player_state::CrossfadeDuration, queue::RepeatMode},
};

/// Player settings to change; `None` leaves a setting as it is.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct UpdatePlayerSettings {
    pub volume: Option<Volume>,
    pub muted: Option<bool>,
    pub shuffle: Option<bool>,
    pub repeat: Option<RepeatMode>,
    pub crossfade: Option<CrossfadeDuration>,
}
