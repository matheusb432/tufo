//! What the player is doing, as every client observes it.

use std::time::{Duration, SystemTime};

use nutype::nutype;

use crate::{
    library::{CollectionEntryId, CollectionId, SoundSettings, Volume},
    playback::{
        AudioFilePath, OutputTarget,
        queue::{QueueEntry, QueueModes},
    },
};

/// The longest crossfade Tufo accepts.
pub const CROSSFADE_DURATION_MAX: Duration = Duration::from_secs(10);

/// How long one track fades into the next, at most [`CROSSFADE_DURATION_MAX`]. Zero plays tracks
/// back to back without a gap.
#[nutype(
    validate(predicate = |duration| *duration <= CROSSFADE_DURATION_MAX),
    default = Duration::from_secs(2),
    derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)
)]
pub struct CrossfadeDuration(Duration);

/// A sound effect to fire: the soundboard entry it belongs to, its file, and how it plays.
#[derive(Debug, Clone, PartialEq)]
pub struct SoundCue {
    pub entry_id: CollectionEntryId,
    pub path: AudioFilePath,
    pub settings: SoundSettings,
}

/// The player's whole observable state. The server publishes it when anything in it changes,
/// never once per audio period, so positions are samples that clients advance on their own.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerState {
    pub output: OutputTarget,
    pub voice: VoiceConnectionState,
    pub music: Option<MusicState>,
    pub volume: Volume,
    pub muted: bool,
    pub modes: QueueModes,
    pub crossfade: CrossfadeDuration,
    /// Sounds playing now, ordered by entry id.
    pub sounds: Vec<ActiveSound>,
    pub sounds_paused: bool,
}

impl Default for PlayerState {
    fn default() -> Self {
        Self {
            output: OutputTarget::Local,
            voice: VoiceConnectionState::NotConfigured,
            music: None,
            volume: Volume::default(),
            muted: false,
            modes: QueueModes::default(),
            crossfade: CrossfadeDuration::default(),
            sounds: Vec::new(),
            sounds_paused: false,
        }
    }
}

/// The Discord voice connection behind the output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoiceConnectionState {
    /// The server has no Discord credentials, so only local output exists.
    NotConfigured,
    /// The bot is configured but paused: its gateway connection is stopped, so it can't join a
    /// channel until resumed.
    Paused,
    /// No voice channel is joined. `reason` explains a connection Tufo gave up on.
    Disconnected { reason: Option<String> },
    /// Joining the output's voice channel.
    Connecting,
    /// Streaming into the output's voice channel.
    Connected,
    /// The voice connection dropped, and Tufo is joining again.
    Reconnecting { attempt: u32 },
}

/// The track playing from a collection's queue.
#[derive(Debug, Clone, PartialEq)]
pub struct MusicState {
    pub collection_id: CollectionId,
    pub entry: QueueEntry,
    pub position: PositionSample,
    /// The decoded length when known, which can differ slightly from the library's.
    pub duration: Option<Duration>,
    pub paused: bool,
}

/// A playback position and the wall-clock time it was read. While playback runs, the position
/// at `now` is `position + (now - sampled_at)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PositionSample {
    pub position: Duration,
    pub sampled_at: SystemTime,
}

impl PositionSample {
    /// The position at `now`, advancing only while `running`.
    #[must_use]
    pub fn at(self, now: SystemTime, running: bool) -> Duration {
        if !running {
            return self.position;
        }
        self.position + now.duration_since(self.sampled_at).unwrap_or_default()
    }
}

/// A sound effect playing now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActiveSound {
    pub entry_id: CollectionEntryId,
    /// When the sound started. A looping sound's position is the time since then, modulo its
    /// duration.
    pub started_at: SystemTime,
    pub duration: Duration,
    pub looping: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_advance_only_while_running() {
        let sampled_at = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
        let sample = PositionSample {
            position: Duration::from_secs(3),
            sampled_at,
        };
        let later = sampled_at + Duration::from_millis(1_500);

        assert_eq!(sample.at(later, true), Duration::from_millis(4_500));
        assert_eq!(sample.at(later, false), Duration::from_secs(3));
        assert_eq!(
            sample.at(sampled_at - Duration::from_secs(1), true),
            Duration::from_secs(3)
        );
    }

    #[test]
    fn crossfades_are_capped_at_ten_seconds() {
        assert!(CrossfadeDuration::try_new(Duration::ZERO).is_ok());
        assert!(CrossfadeDuration::try_new(CROSSFADE_DURATION_MAX).is_ok());
        assert!(
            CrossfadeDuration::try_new(CROSSFADE_DURATION_MAX + Duration::from_millis(1)).is_err()
        );
    }
}
