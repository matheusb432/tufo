//! What the player is doing, as every client observes it.

use std::time::{Duration, SystemTime};

use nutype::nutype;

use crate::{
    library::{CollectionEntryId, CollectionId, SoundSettings, Volume, song_loop::LoopRegion},
    playback::{
        AudioFilePath, OutputTarget,
        music_layers::{MusicLayerState, MusicLayers},
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
    /// The songs added to the queue, in the order they play.
    pub queued_entry_ids: Vec<CollectionEntryId>,
    pub music_layers: MusicLayers<MusicLayerState>,
    pub volume: Volume,
    pub effects_volume: Volume,
    pub muted: bool,
    pub modes: QueueModes,
    pub crossfade: CrossfadeDuration,
    /// Sounds playing now, ordered by entry id.
    pub sounds: Vec<ActiveSound>,
    pub sounds_paused: bool,
}

impl PlayerState {
    /// The main queue track first, then extra songs in slot order.
    pub fn music_tracks(&self) -> impl Iterator<Item = &MusicState> {
        self.music
            .iter()
            .chain(self.music_layers.iter().map(|(_, layer)| &layer.music))
    }
}

impl Default for PlayerState {
    fn default() -> Self {
        Self {
            output: OutputTarget::Local,
            voice: VoiceConnectionState::NotConfigured,
            music: None,
            queued_entry_ids: Vec::new(),
            music_layers: MusicLayers::default(),
            volume: Volume::default(),
            effects_volume: Volume::default(),
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
    /// The bot is configured, but its Discord connection is disabled, so it can't join a channel.
    Disabled,
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
    /// The region the track started with, which later edits don't change.
    pub loop_region: Option<LoopRegion>,
}

impl MusicState {
    /// Advances the sample while playing, wrapping at the loop end while `looping` and stopping
    /// there otherwise.
    #[must_use]
    pub fn position_at(&self, now: SystemTime, looping: bool) -> Duration {
        let unwrapped = self.position.at(now, !self.paused);
        let Some(region) = self.loop_region else {
            return unwrapped;
        };
        if looping {
            region.position_after(
                self.position.position,
                unwrapped.saturating_sub(self.position.position),
            )
        } else {
            unwrapped.min(region.loop_end())
        }
    }
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
    fn positions_wrap_inside_a_loop_only_while_it_loops() {
        use std::num::NonZeroU32;

        use crate::library::{
            AudioFileId, CollectionEntryId, CollectionId,
            song_loop::{LoopMarkers, LoopRegion, SourceFrames, SourceTiming},
        };

        let sampled_at = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
        let region = LoopRegion::try_new(
            SourceTiming {
                sample_rate: NonZeroU32::new(1_000).unwrap(),
                frame_count: SourceFrames(10_000),
            },
            LoopMarkers {
                playback_start: SourceFrames::ZERO,
                loop_start: SourceFrames(2_000),
                loop_end: SourceFrames(6_000),
                crossfade: SourceFrames::ZERO,
            },
        )
        .unwrap();
        let id = toasty::stmt::Uuid::from_u128(1);
        let music = MusicState {
            collection_id: CollectionId::from_uuid(id),
            entry: QueueEntry {
                entry_id: CollectionEntryId::from_uuid(id),
                audio_file_id: AudioFileId::from_uuid(id),
                path: AudioFilePath::try_new(std::env::temp_dir().join("boss.ogg")).unwrap(),
                title: "Boss".to_owned(),
                duration: None,
            },
            position: PositionSample {
                position: Duration::from_secs(5),
                sampled_at,
            },
            duration: Some(Duration::from_secs(10)),
            paused: false,
            loop_region: Some(region),
        };
        let later = sampled_at + Duration::from_secs(3);

        assert_eq!(music.position_at(later, true), Duration::from_secs(4));
        assert_eq!(music.position_at(later, false), Duration::from_secs(6));
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
