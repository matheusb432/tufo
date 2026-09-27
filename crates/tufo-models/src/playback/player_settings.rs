//! The player settings that survive a server restart.

use std::time::Duration;

use super::{player_state::CrossfadeDuration, queue::RepeatMode};
use crate::{
    library::{Volume, milliseconds_saturating},
    storage::checked_column,
};

/// The one row of saved player settings.
pub const PLAYER_SETTINGS_ID: i64 = 1;

/// Volume, mute, shuffle, repeat, and crossfade, as the player last had them. The table holds
/// at most one row, with id [`PLAYER_SETTINGS_ID`].
#[derive(Debug, toasty::Model)]
#[table = "player_settings"]
pub struct SavedPlayerSettings {
    #[key]
    pub id: i64,
    pub volume: Volume,
    pub muted: bool,
    pub shuffle: bool,
    pub repeat: RepeatMode,
    pub crossfade: CrossfadeDuration,
}

checked_column!(
    CrossfadeDuration,
    u32,
    decode =
        |milliseconds| CrossfadeDuration::try_new(Duration::from_millis(u64::from(milliseconds))),
    encode = |crossfade| milliseconds_saturating(crossfade.into_inner()),
);
