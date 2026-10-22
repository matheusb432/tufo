//! Remembering when each playlist entry last started as the main music track, which the
//! recently played list shows.
//!
//! The recorder observes the player's published state rather than the engine, so playback never
//! waits on the database. Starts observed while a write is in progress join the next write.

use std::{collections::HashMap, future::Future};

use jiff::Timestamp;
use toasty_core::driver::operation::TransactionMode;
use tokio::sync::watch;
use tufo_models::{
    library::{CollectionEntry, CollectionEntryId},
    playback::player_state::PlayerState,
};

use super::{Library, LibraryStorageError, library_files::LibraryFiles};

/// The most starts kept for the next write; past it, the oldest start is forgotten.
const PENDING_STARTS_MAX: usize = 64;

/// Records each main-track start that `player_states` publishes until `shutdown` resolves or the
/// player stops publishing. A write in progress finishes first.
pub async fn run<F: LibraryFiles>(
    library: &Library<F>,
    mut player_states: watch::Receiver<PlayerState>,
    shutdown: impl Future<Output = ()>,
) {
    tokio::pin!(shutdown);
    let mut main_track = MainTrackStarts::default();
    let mut pending = PendingStarts::default();
    let mut player_publishing = true;
    let mut shutting_down = false;
    loop {
        observe(&mut player_states, &mut main_track, &mut pending);
        if pending.is_empty() {
            if !player_publishing {
                return;
            }
            tokio::select! {
                () = &mut shutdown => return,
                changed = player_states.changed() => player_publishing = changed.is_ok(),
            }
            continue;
        }
        let write = record(library, pending.take());
        tokio::pin!(write);
        loop {
            tokio::select! {
                () = &mut write => break,
                () = &mut shutdown, if !shutting_down => shutting_down = true,
                changed = player_states.changed(), if player_publishing => {
                    player_publishing = changed.is_ok();
                    observe(&mut player_states, &mut main_track, &mut pending);
                }
            }
        }
        if shutting_down {
            return;
        }
    }
}

fn observe(
    player_states: &mut watch::Receiver<PlayerState>,
    main_track: &mut MainTrackStarts,
    pending: &mut PendingStarts,
) {
    // The player waits for this borrow to end before it publishes again, so it ends at once.
    let started = main_track.observe(&player_states.borrow_and_update());
    if let Some(entry_id) = started {
        pending.insert(TrackStart {
            entry_id,
            started_at: Timestamp::now(),
        });
    }
}

async fn record<F: LibraryFiles>(library: &Library<F>, starts: Vec<TrackStart>) {
    if let Err(error) = record_starts(library, &starts).await {
        tracing::warn!(%error, "cannot record when playlist tracks last played");
    }
}

async fn record_starts<F: LibraryFiles>(
    library: &Library<F>,
    starts: &[TrackStart],
) -> Result<(), LibraryStorageError> {
    let _change = library.begin_change().await;
    let mut database = library.database();
    let mut transaction = database
        .transaction_builder()
        .mode(TransactionMode::Immediate)
        .begin()
        .await?;
    for start in starts {
        // An entry deleted since it started matches nothing.
        CollectionEntry::filter_by_id(start.entry_id)
            .update()
            .last_played_at(Some(start.started_at))
            .exec(&mut transaction)
            .await?;
    }
    transaction.commit().await?;
    library.publish().await
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TrackStart {
    entry_id: CollectionEntryId,
    started_at: Timestamp,
}

/// Tells when the main track becomes a different entry. Loop wraps, seeks, pauses, and other
/// changes to the same entry are not starts; stopping and playing it again is.
#[derive(Debug, Default)]
struct MainTrackStarts {
    current_entry_id: Option<CollectionEntryId>,
}

impl MainTrackStarts {
    /// The entry that just started as the main track, if `state` shows one.
    fn observe(&mut self, state: &PlayerState) -> Option<CollectionEntryId> {
        let entry_id = state.music.as_ref().map(|music| music.entry.entry_id);
        let previous = std::mem::replace(&mut self.current_entry_id, entry_id);
        entry_id.filter(|entry_id| previous != Some(*entry_id))
    }
}

/// The latest start of each entry not yet written, at most [`PENDING_STARTS_MAX`] entries.
#[derive(Debug, Default)]
struct PendingStarts {
    started_at_by_entry: HashMap<CollectionEntryId, Timestamp>,
}

impl PendingStarts {
    fn is_empty(&self) -> bool {
        self.started_at_by_entry.is_empty()
    }

    fn insert(&mut self, start: TrackStart) {
        let starts = &mut self.started_at_by_entry;
        if starts.len() >= PENDING_STARTS_MAX && !starts.contains_key(&start.entry_id) {
            let oldest = starts
                .iter()
                .min_by_key(|(_, started_at)| **started_at)
                .map(|(entry_id, _)| *entry_id);
            if let Some(oldest) = oldest {
                starts.remove(&oldest);
            }
        }
        starts.insert(start.entry_id, start.started_at);
    }

    fn take(&mut self) -> Vec<TrackStart> {
        self.started_at_by_entry
            .drain()
            .map(|(entry_id, started_at)| TrackStart {
                entry_id,
                started_at,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use toasty::stmt::Uuid;
    use tufo_models::{
        library::{AudioFileId, CollectionId},
        playback::{
            AudioFilePath,
            player_state::{MusicState, PositionSample},
            queue::QueueEntry,
        },
    };

    use super::*;

    fn entry_id(seed: u128) -> CollectionEntryId {
        CollectionEntryId::from_uuid(Uuid::from_u128(seed))
    }

    fn playing(seed: u128, position_seconds: u64, paused: bool) -> PlayerState {
        PlayerState {
            music: Some(MusicState {
                collection_id: CollectionId::from_uuid(Uuid::from_u128(1)),
                entry: QueueEntry {
                    entry_id: entry_id(seed),
                    audio_file_id: AudioFileId::from_uuid(Uuid::from_u128(seed)),
                    path: AudioFilePath::try_new(std::env::temp_dir().join("tavern.ogg")).unwrap(),
                    title: "Tavern".to_owned(),
                    duration: Some(Duration::from_secs(180)),
                },
                position: PositionSample {
                    position: Duration::from_secs(position_seconds),
                    sampled_at: SystemTime::UNIX_EPOCH,
                },
                duration: Some(Duration::from_secs(180)),
                paused,
                loop_region: None,
            }),
            ..PlayerState::default()
        }
    }

    #[test]
    fn only_a_different_main_entry_is_a_start() {
        let mut main_track = MainTrackStarts::default();

        let starts = [
            PlayerState::default(),
            playing(2, 0, false),
            playing(2, 90, false),
            playing(2, 90, true),
            playing(2, 3, false),
            playing(3, 0, false),
            PlayerState::default(),
            playing(3, 0, false),
        ]
        .iter()
        .map(|state| main_track.observe(state))
        .collect::<Vec<_>>();

        assert_eq!(
            starts,
            [
                None,
                Some(entry_id(2)),
                None,
                None,
                None,
                Some(entry_id(3)),
                None,
                Some(entry_id(3)),
            ]
        );
    }

    #[test]
    fn pending_starts_keep_each_entry_once_and_forget_the_oldest_past_the_cap() {
        let mut pending = PendingStarts::default();
        let at = |second| Timestamp::from_second(second).unwrap();
        let max = u128::try_from(PENDING_STARTS_MAX).unwrap();
        pending.insert(TrackStart {
            entry_id: entry_id(0),
            started_at: at(0),
        });
        pending.insert(TrackStart {
            entry_id: entry_id(0),
            started_at: at(1_000),
        });
        for seed in 1..=max {
            pending.insert(TrackStart {
                entry_id: entry_id(seed),
                started_at: at(i64::try_from(seed).unwrap()),
            });
        }

        let mut starts = pending.take();
        starts.sort_by_key(|start| start.started_at);

        assert_eq!(starts.len(), PENDING_STARTS_MAX);
        assert_eq!(starts[0].entry_id, entry_id(2));
        assert_eq!(
            starts.last(),
            Some(&TrackStart {
                entry_id: entry_id(0),
                started_at: at(1_000),
            })
        );
        assert!(pending.is_empty());
    }
}
