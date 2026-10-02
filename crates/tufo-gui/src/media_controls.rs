//! Publishes the music player to the desktop, including while the window is hidden.

use std::time::{Duration, SystemTime};

use tokio::sync::watch;
use tufo_client::library::LibrarySnapshot;
use tufo_models::{
    library::{CollectionEntryId, Volume},
    playback::player_state::{PlayerState, PositionSample},
};

use crate::{WindowRequests, presentation};

#[cfg(target_os = "linux")]
#[path = "media_controls/linux.rs"]
mod platform;
#[cfg(any(target_os = "macos", windows))]
#[path = "media_controls/native.rs"]
mod platform;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MediaState {
    track: Option<MediaTrack>,
    can_skip: bool,
    can_seek: bool,
    volume: Volume,
    muted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MediaTrack {
    metadata: TrackMetadata,
    paused: bool,
    position: Duration,
    position_sample: PositionSample,
}

#[cfg(target_os = "linux")]
impl MediaTrack {
    fn position_jumped_since(&self, previous: Option<&Self>) -> bool {
        let expected = previous
            .filter(|previous| self.metadata.entry_id == previous.metadata.entry_id)
            .map_or(Duration::ZERO, |previous| {
                previous
                    .position_sample
                    .at(self.position_sample.sampled_at, !previous.paused)
            });
        // Audio callback granularity can move a fresh sample by tens of milliseconds.
        self.position_sample.position.abs_diff(expected) > Duration::from_millis(100)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TrackMetadata {
    entry_id: CollectionEntryId,
    title: String,
    album: Option<String>,
    duration: Option<Duration>,
}

impl MediaState {
    pub(crate) fn from_player(
        snapshot: &LibrarySnapshot,
        player: &PlayerState,
        now: SystemTime,
    ) -> Self {
        let track = player.music_tracks().next().map(|music| {
            let duration = music.duration.or(music.entry.duration);
            let position = music.position.at(now, !music.paused);
            MediaTrack {
                metadata: TrackMetadata {
                    entry_id: music.entry.entry_id,
                    title: presentation::find_entry(snapshot, music.entry.entry_id).map_or_else(
                        || music.entry.title.clone(),
                        |(_, entry)| entry.title.as_str().to_owned(),
                    ),
                    album: presentation::find_collection(snapshot, music.collection_id)
                        .map(|collection| collection.title.as_str().to_owned()),
                    duration,
                },
                paused: music.paused,
                position: duration.map_or(position, |duration| position.min(duration)),
                position_sample: music.position,
            }
        });
        Self {
            track,
            can_skip: player.music.is_some(),
            can_seek: player
                .music
                .as_ref()
                .is_some_and(|music| music.duration.is_some()),
            volume: player.volume,
            muted: player.muted,
        }
    }
}

pub(crate) struct MediaSession {
    updates: watch::Sender<MediaState>,
}

impl MediaSession {
    pub(crate) fn publish(&self, state: MediaState) {
        self.updates.send_if_modified(|previous| {
            if *previous == state {
                return false;
            }
            *previous = state;
            true
        });
    }
}

pub(crate) fn install(requests: WindowRequests) -> Result<MediaSession, slint::EventLoopError> {
    let (updates, states) = watch::channel(requests.controller.media_state());
    slint::spawn_local(async move {
        if let Err(error) = platform::run(requests, states).await {
            tracing::warn!(%error, "desktop media controls are unavailable");
        }
    })?;
    Ok(MediaSession { updates })
}

#[cfg(test)]
mod tests;
