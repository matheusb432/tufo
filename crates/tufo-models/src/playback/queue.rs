//! The music queue: which track plays now, and which plays after it.
//!
//! Repeating a track restarts it at its end, while "next" and "previous" still move through the
//! playlist; the playlist wraps unless repeat is off; and "previous" restarts the current track
//! once it has played for [`PREVIOUS_RESTART_THRESHOLD`].

use std::time::Duration;

use crate::{
    library::{CollectionEntryId, CollectionId},
    playback::AudioFilePath,
};

/// How long a track plays before "previous" restarts it instead of going back.
pub const PREVIOUS_RESTART_THRESHOLD: Duration = Duration::from_secs(5);

/// One track of a queue, as the library knew it when playback started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueEntry {
    pub entry_id: CollectionEntryId,
    pub path: AudioFilePath,
    pub title: String,
    /// The track's length when the library knows it.
    pub duration: Option<Duration>,
}

/// What happens when the queue moves past its current track.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, toasty::Embed)]
pub enum RepeatMode {
    /// The queue stops after its last track.
    Off,
    /// The queue wraps from its last track to its first. Kenku FM's default.
    #[default]
    Playlist,
    /// The current track plays again at its end.
    Track,
}

/// The player's shuffle and repeat settings, which carry over from one queue to the next.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct QueueModes {
    pub shuffle: bool,
    pub repeat: RepeatMode,
}

/// The tracks of one collection, in its order, and the one to start from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueStart {
    pub collection_id: CollectionId,
    pub entries: Vec<QueueEntry>,
    pub start_index: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum NewQueueError {
    #[error("a queue needs at least one track")]
    Empty,
    #[error("the start index {start_index} is past the queue's {length} tracks")]
    StartOutOfRange { start_index: usize, length: usize },
}

/// A collection's tracks in the order they play, with a current track.
///
/// The entries stay in the collection's order. With shuffle on, a separate play order starts at
/// the track that was current when shuffling began and visits the rest in random order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MusicQueue {
    collection_id: CollectionId,
    entries: Vec<QueueEntry>,
    /// Indices into `entries`, in play order.
    order: Vec<usize>,
    /// The current track's position in `order`.
    position: usize,
    modes: QueueModes,
}

impl MusicQueue {
    /// Starts at the start entry. `shuffle_seed` orders the other tracks when `modes` shuffles.
    pub fn new(
        start: QueueStart,
        modes: QueueModes,
        shuffle_seed: u64,
    ) -> Result<Self, NewQueueError> {
        let QueueStart {
            collection_id,
            entries,
            start_index,
        } = start;
        if entries.is_empty() {
            return Err(NewQueueError::Empty);
        }
        if start_index >= entries.len() {
            return Err(NewQueueError::StartOutOfRange {
                start_index,
                length: entries.len(),
            });
        }
        let mut queue = Self {
            collection_id,
            order: (0..entries.len()).collect(),
            entries,
            position: start_index,
            modes: QueueModes {
                shuffle: false,
                repeat: modes.repeat,
            },
        };
        queue.set_shuffle(modes.shuffle, shuffle_seed);
        Ok(queue)
    }

    #[must_use]
    pub fn collection_id(&self) -> CollectionId {
        self.collection_id
    }

    #[must_use]
    pub fn modes(&self) -> QueueModes {
        self.modes
    }

    #[must_use]
    pub fn current(&self) -> &QueueEntry {
        &self.entries[self.order[self.position]]
    }

    /// Turns shuffle on or off without changing the current track. Turning it on shuffles the
    /// other tracks after the current one; turning it off returns to the collection's order.
    pub fn set_shuffle(&mut self, shuffle: bool, seed: u64) {
        let current_index = self.order[self.position];
        if shuffle {
            let mut others = (0..self.entries.len())
                .filter(|&index| index != current_index)
                .collect::<Vec<_>>();
            shuffle_in_place(&mut others, seed);
            self.order = std::iter::once(current_index).chain(others).collect();
            self.position = 0;
        } else {
            self.order = (0..self.entries.len()).collect();
            self.position = current_index;
        }
        self.modes.shuffle = shuffle;
    }

    pub fn set_repeat(&mut self, repeat: RepeatMode) {
        self.modes.repeat = repeat;
    }

    /// The track that plays when the current one ends, or `None` when the queue stops there.
    #[must_use]
    pub fn following(&self) -> Option<&QueueEntry> {
        self.position_after_current()
            .map(|position| &self.entries[self.order[position]])
    }

    /// Moves to the track that plays after the current one ends. Returns the new current track,
    /// which is the same one when it repeats, or `None` when the queue stops; a stopped queue
    /// keeps its current track.
    pub fn advance(&mut self) -> Option<&QueueEntry> {
        self.position = self.position_after_current()?;
        Some(self.current())
    }

    /// Moves to the next track for "next", even when the current one repeats. Returns the new
    /// current track, or `None` when the queue stops at its end.
    pub fn skip_to_next(&mut self) -> Option<&QueueEntry> {
        self.position = self.next_position()?;
        Some(self.current())
    }

    /// Moves back for "previous", given how long the current track has played. Returns the new
    /// current track, or `None` when the queue stops at its start.
    pub fn skip_to_previous(&mut self, played: Duration) -> Option<&QueueEntry> {
        if played < PREVIOUS_RESTART_THRESHOLD {
            self.position = self.previous_position()?;
        }
        Some(self.current())
    }

    fn position_after_current(&self) -> Option<usize> {
        match self.modes.repeat {
            RepeatMode::Track => Some(self.position),
            RepeatMode::Playlist | RepeatMode::Off => self.next_position(),
        }
    }

    /// The position after the current one in play order, wrapping unless repeat is off.
    fn next_position(&self) -> Option<usize> {
        let next = self.position + 1;
        if next < self.order.len() {
            Some(next)
        } else if self.modes.repeat == RepeatMode::Off {
            None
        } else {
            Some(0)
        }
    }

    /// The position before the current one in play order, wrapping unless repeat is off.
    fn previous_position(&self) -> Option<usize> {
        match self.position.checked_sub(1) {
            Some(previous) => Some(previous),
            None if self.modes.repeat == RepeatMode::Off => None,
            None => Some(self.order.len() - 1),
        }
    }
}

/// Fisher–Yates with `SplitMix64`, so the order depends only on `seed`.
fn shuffle_in_place(indices: &mut [usize], seed: u64) {
    let mut state = seed;
    for last in (1..indices.len()).rev() {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut mixed = state;
        mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        mixed ^= mixed >> 31;
        let bound = u64::try_from(last + 1).unwrap_or(u64::MAX);
        let chosen = usize::try_from(mixed % bound).unwrap_or(0);
        indices.swap(last, chosen);
    }
}

#[cfg(test)]
mod tests {
    use toasty::stmt::Uuid;

    use super::*;

    fn entry(number: u128) -> QueueEntry {
        QueueEntry {
            entry_id: CollectionEntryId::from_uuid(Uuid::from_u128(number)),
            path: AudioFilePath::try_new(std::env::temp_dir().join(format!("{number}.flac")))
                .unwrap(),
            title: format!("Track {number}"),
            duration: None,
        }
    }

    fn start(entries: Vec<QueueEntry>, start_index: usize) -> QueueStart {
        QueueStart {
            collection_id: CollectionId::from_uuid(Uuid::from_u128(1)),
            entries,
            start_index,
        }
    }

    fn queue(track_count: u128, start_index: usize, modes: QueueModes) -> MusicQueue {
        MusicQueue::new(
            start((1..=track_count).map(entry).collect(), start_index),
            modes,
            7,
        )
        .unwrap()
    }

    fn repeat(repeat: RepeatMode) -> QueueModes {
        QueueModes {
            shuffle: false,
            repeat,
        }
    }

    fn title(entry: Option<&QueueEntry>) -> Option<&str> {
        entry.map(|entry| entry.title.as_str())
    }

    #[test]
    fn a_queue_needs_tracks_and_a_start_inside_them() {
        let empty = MusicQueue::new(start(Vec::new(), 0), QueueModes::default(), 0);
        let past_end = MusicQueue::new(start(vec![entry(1)], 1), QueueModes::default(), 0);

        assert_eq!(empty.unwrap_err(), NewQueueError::Empty);
        assert_eq!(
            past_end.unwrap_err(),
            NewQueueError::StartOutOfRange {
                start_index: 1,
                length: 1
            }
        );
    }

    #[test]
    fn a_playlist_wraps_from_its_last_track_to_its_first() {
        let mut queue = queue(3, 1, repeat(RepeatMode::Playlist));

        assert_eq!(title(queue.following()), Some("Track 3"));
        assert_eq!(title(queue.advance()), Some("Track 3"));
        assert_eq!(title(queue.following()), Some("Track 1"));
        assert_eq!(title(queue.advance()), Some("Track 1"));
    }

    #[test]
    fn repeat_off_stops_after_the_last_track_and_before_the_first() {
        let mut at_end = queue(2, 1, repeat(RepeatMode::Off));
        let mut at_start = queue(2, 0, repeat(RepeatMode::Off));

        assert_eq!(at_end.following(), None);
        assert_eq!(at_end.advance(), None);
        assert_eq!(at_end.current().title, "Track 2");
        assert_eq!(at_end.skip_to_next(), None);
        assert_eq!(at_start.skip_to_previous(Duration::ZERO), None);
    }

    #[test]
    fn repeating_a_track_plays_it_again_at_its_end() {
        let mut queue = queue(3, 1, repeat(RepeatMode::Track));

        assert_eq!(title(queue.following()), Some("Track 2"));
        assert_eq!(title(queue.advance()), Some("Track 2"));
    }

    #[test]
    fn next_and_previous_leave_a_repeating_track_and_wrap() {
        let mut queue = queue(3, 2, repeat(RepeatMode::Track));

        assert_eq!(title(queue.skip_to_next()), Some("Track 1"));
        assert_eq!(
            title(queue.skip_to_previous(Duration::ZERO)),
            Some("Track 3")
        );
        assert_eq!(
            title(queue.skip_to_previous(PREVIOUS_RESTART_THRESHOLD)),
            Some("Track 3")
        );
        assert_eq!(title(queue.following()), Some("Track 3"));
    }

    #[test]
    fn previous_goes_back_early_in_a_track_and_restarts_it_later() {
        let mut queue = queue(3, 1, repeat(RepeatMode::Playlist));

        assert_eq!(
            title(queue.skip_to_previous(PREVIOUS_RESTART_THRESHOLD)),
            Some("Track 2")
        );
        assert_eq!(
            title(queue.skip_to_previous(Duration::from_millis(4_999))),
            Some("Track 1")
        );
        assert_eq!(
            title(queue.skip_to_previous(Duration::ZERO)),
            Some("Track 3")
        );
    }

    #[test]
    fn shuffling_keeps_the_current_track_and_visits_every_other_once() {
        let mut queue = queue(
            6,
            2,
            QueueModes {
                shuffle: true,
                repeat: RepeatMode::Off,
            },
        );

        let mut played = vec![queue.current().title.clone()];
        while let Some(next) = queue.advance() {
            played.push(next.title.clone());
        }

        assert_eq!(played[0], "Track 3");
        played.sort();
        assert_eq!(
            played,
            [
                "Track 1", "Track 2", "Track 3", "Track 4", "Track 5", "Track 6"
            ]
        );
    }

    #[test]
    fn unshuffling_resumes_the_collection_order_from_the_current_track() {
        let mut queue = queue(
            6,
            0,
            QueueModes {
                shuffle: true,
                repeat: RepeatMode::Playlist,
            },
        );
        let current = queue.advance().unwrap().title.clone();
        let current_number = current.trim_start_matches("Track ").parse::<u32>().unwrap();

        queue.set_shuffle(false, 0);

        assert_eq!(queue.current().title, current);
        assert_eq!(
            queue.following().unwrap().title,
            format!("Track {}", current_number % 6 + 1)
        );
    }

    #[test]
    fn the_same_seed_shuffles_the_same_way() {
        let modes = QueueModes {
            shuffle: true,
            repeat: RepeatMode::Off,
        };
        let play_order = |mut queue: MusicQueue| {
            let mut titles = vec![queue.current().title.clone()];
            while let Some(next) = queue.advance() {
                titles.push(next.title.clone());
            }
            titles
        };

        assert_eq!(
            play_order(queue(8, 0, modes)),
            play_order(queue(8, 0, modes))
        );
    }
}
