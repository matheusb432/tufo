//! The music queue: which track plays now, and which plays after it.
//!
//! Repeating a track restarts it at its end, while "next" and "previous" still move through the
//! playlist; the playlist wraps unless repeat is off; and "previous" restarts the current track
//! once it has played for [`PREVIOUS_RESTART_THRESHOLD`].
//!
//! Songs added to the queue, from any playlist, play in the order they were added before the
//! playlist continues after the track they interrupted. With repeat off each plays once; with
//! repeat on they rotate until removed. Repeating one track excludes queued songs.

use std::{collections::VecDeque, time::Duration};

use crate::{
    library::{AudioFileId, CollectionEntryId, CollectionId},
    playback::AudioFilePath,
};

/// How long a track plays before "previous" restarts it instead of going back.
pub const PREVIOUS_RESTART_THRESHOLD: Duration = Duration::from_secs(5);

/// One track of a queue, as the library knew it when playback started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueEntry {
    pub entry_id: CollectionEntryId,
    pub audio_file_id: AudioFileId,
    pub path: AudioFilePath,
    pub title: String,
    /// The track's length when the library knows it.
    pub duration: Option<Duration>,
}

/// A song added to the queue, and the playlist it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedEntry {
    pub collection_id: CollectionId,
    pub entry: QueueEntry,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum EnqueueError {
    #[error("songs can't be queued while one track repeats")]
    RepeatingTrack,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SetRepeatError {
    #[error("one track can't repeat while songs are queued")]
    SongsQueued,
}

/// A collection's tracks in the order they play, with a current track, and the songs queued
/// ahead of them.
///
/// The entries stay in the collection's order. With shuffle on, a separate play order starts at
/// the track that was current when shuffling began and visits the rest in random order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MusicQueue {
    collection_id: CollectionId,
    entries: Vec<QueueEntry>,
    /// Indices into `entries`, in play order.
    order: Vec<usize>,
    /// The playlist track's position in `order`: the current track's, or while a queued song
    /// plays, the one it interrupted.
    position: usize,
    modes: QueueModes,
    /// In play order, each entry at most once, and never while one track repeats. With repeat
    /// on, a song goes back to the end as it starts, so a queued current song sits last.
    queued: VecDeque<QueuedEntry>,
    /// The current track when it came from `queued`.
    current_queued: Option<QueuedEntry>,
    /// With repeat off, the queued songs that played before the current one, for "previous".
    played_queued: Vec<QueuedEntry>,
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
            queued: VecDeque::new(),
            current_queued: None,
            played_queued: Vec::new(),
        };
        queue.set_shuffle(modes.shuffle, shuffle_seed);
        Ok(queue)
    }

    /// The playlist the current track belongs to.
    #[must_use]
    pub fn current_collection_id(&self) -> CollectionId {
        self.current_queued
            .as_ref()
            .map_or(self.collection_id, |song| song.collection_id)
    }

    #[must_use]
    pub fn modes(&self) -> QueueModes {
        self.modes
    }

    #[must_use]
    pub fn current(&self) -> &QueueEntry {
        match &self.current_queued {
            Some(song) => &song.entry,
            None => self.playlist_entry(self.position),
        }
    }

    /// The queued songs in the order they play.
    pub fn queued(&self) -> impl ExactSizeIterator<Item = &QueuedEntry> {
        self.queued.iter()
    }

    /// Queues `song` after the songs already queued. Queuing a queued song leaves it in place.
    pub fn enqueue(&mut self, song: QueuedEntry) -> Result<(), EnqueueError> {
        if self.modes.repeat == RepeatMode::Track {
            return Err(EnqueueError::RepeatingTrack);
        }
        if self.queued_index(song.entry.entry_id).is_some() {
            return Ok(());
        }
        let index = if self.current_rejoins() {
            self.queued.len() - 1
        } else {
            self.queued.len()
        };
        self.queued.insert(index, song);
        Ok(())
    }

    /// Takes a song out of the queue; a current queued song keeps playing but won't return.
    pub fn remove_queued(&mut self, entry_id: CollectionEntryId) {
        if let Some(index) = self.queued_index(entry_id) {
            self.queued.remove(index);
        }
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

    /// Turning repeat on brings the queued songs that already played back into the rotation;
    /// turning it off lets the current queued song end its turn.
    pub fn set_repeat(&mut self, repeat: RepeatMode) -> Result<(), SetRepeatError> {
        if repeat == self.modes.repeat {
            return Ok(());
        }
        match repeat {
            RepeatMode::Track if !self.queued.is_empty() => {
                return Err(SetRepeatError::SongsQueued);
            }
            RepeatMode::Track => {}
            RepeatMode::Playlist => self.rejoin_played(),
            RepeatMode::Off => {
                if self.current_rejoins() {
                    self.queued.pop_back();
                }
            }
        }
        self.played_queued.clear();
        self.modes.repeat = repeat;
        Ok(())
    }

    /// The track that plays when the current one ends, or `None` when the queue stops there.
    #[must_use]
    pub fn following(&self) -> Option<&QueueEntry> {
        if self.modes.repeat == RepeatMode::Track {
            return Some(self.current());
        }
        match self.queued.front() {
            Some(song) => Some(&song.entry),
            None => self
                .next_position()
                .map(|position| self.playlist_entry(position)),
        }
    }

    /// Moves to the track that plays after the current one ends. Returns the new current track,
    /// which is the same one when it repeats, or `None` when the queue stops; a stopped queue
    /// keeps its current track.
    pub fn advance(&mut self) -> Option<&QueueEntry> {
        if self.modes.repeat == RepeatMode::Track {
            return Some(self.current());
        }
        self.skip_to_next()
    }

    /// Moves to the next track for "next", even when the current one repeats: the first queued
    /// song, else the playlist's next track. Returns the new current track, or `None` when the
    /// queue stops at its end.
    pub fn skip_to_next(&mut self) -> Option<&QueueEntry> {
        if let Some(next) = self.queued.pop_front() {
            let left = self.current_queued.replace(next.clone());
            if self.modes.repeat == RepeatMode::Playlist {
                self.queued.push_back(next);
            } else {
                self.played_queued.extend(left);
            }
            return Some(self.current());
        }
        self.position = self.next_position()?;
        self.current_queued = None;
        self.played_queued.clear();
        Some(self.current())
    }

    /// Moves back for "previous", given how long the current track has played. From a queued
    /// song it goes to the one before it, or to the track the queue interrupted, and the song
    /// returns to the front of the queue. Returns the new current track, or `None` when the
    /// queue stops at its start.
    pub fn skip_to_previous(&mut self, played: Duration) -> Option<&QueueEntry> {
        if played >= PREVIOUS_RESTART_THRESHOLD {
            return Some(self.current());
        }
        let Some(left) = self.current_queued.take() else {
            self.position = self.previous_position()?;
            return Some(self.current());
        };
        match self.modes.repeat {
            RepeatMode::Playlist => {
                if self.is_last_queued(&left)
                    && let Some(song) = self.queued.pop_back()
                {
                    self.queued.push_front(song);
                }
                self.current_queued = self.queued.back().cloned();
            }
            RepeatMode::Off => {
                self.remove_queued(left.entry.entry_id);
                self.queued.push_front(left);
                self.current_queued = self.played_queued.pop();
            }
            RepeatMode::Track => {}
        }
        Some(self.current())
    }

    fn playlist_entry(&self, position: usize) -> &QueueEntry {
        &self.entries[self.order[position]]
    }

    fn queued_index(&self, entry_id: CollectionEntryId) -> Option<usize> {
        self.queued
            .iter()
            .position(|song| song.entry.entry_id == entry_id)
    }

    fn is_last_queued(&self, song: &QueuedEntry) -> bool {
        self.queued
            .back()
            .is_some_and(|last| last.entry.entry_id == song.entry.entry_id)
    }

    /// Whether the current track is a queued song that comes around again.
    fn current_rejoins(&self) -> bool {
        self.modes.repeat == RepeatMode::Playlist
            && self
                .current_queued
                .as_ref()
                .is_some_and(|current| self.is_last_queued(current))
    }

    /// Queues the songs that played this pass after the upcoming ones, and the current queued
    /// song last.
    fn rejoin_played(&mut self) {
        for song in std::mem::take(&mut self.played_queued) {
            if self.queued_index(song.entry.entry_id).is_none() {
                self.queued.push_back(song);
            }
        }
        if let Some(current) = self.current_queued.clone() {
            self.remove_queued(current.entry.entry_id);
            self.queued.push_back(current);
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
            audio_file_id: AudioFileId::from_uuid(Uuid::from_u128(number)),
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

    /// A song from another playlist.
    fn song(number: u128) -> QueuedEntry {
        QueuedEntry {
            collection_id: CollectionId::from_uuid(Uuid::from_u128(2)),
            entry: entry(number),
        }
    }

    fn with_songs(mut queue: MusicQueue, numbers: impl IntoIterator<Item = u128>) -> MusicQueue {
        for number in numbers {
            queue.enqueue(song(number)).unwrap();
        }
        queue
    }

    fn queued_titles(queue: &MusicQueue) -> Vec<&str> {
        queue
            .queued()
            .map(|song| song.entry.title.as_str())
            .collect()
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

    #[test]
    fn queued_songs_play_once_then_the_playlist_resumes_after_the_interrupted_track() {
        let mut queue = with_songs(queue(4, 1, repeat(RepeatMode::Off)), [10, 11]);

        assert_eq!(title(queue.following()), Some("Track 10"));
        assert_eq!(title(queue.advance()), Some("Track 10"));
        assert_eq!(queue.current_collection_id(), song(10).collection_id);
        assert_eq!(queued_titles(&queue), ["Track 11"]);
        assert_eq!(title(queue.advance()), Some("Track 11"));
        assert_eq!(title(queue.advance()), Some("Track 3"));
        assert_eq!(
            queue.current_collection_id(),
            start(Vec::new(), 0).collection_id
        );
        assert_eq!(queue.queued().len(), 0);
    }

    #[test]
    fn with_repeat_on_queued_songs_rotate_until_removed() {
        let mut queue = with_songs(queue(4, 1, repeat(RepeatMode::Playlist)), [10, 11]);

        assert_eq!(title(queue.advance()), Some("Track 10"));
        assert_eq!(title(queue.advance()), Some("Track 11"));
        assert_eq!(queued_titles(&queue), ["Track 10", "Track 11"]);
        assert_eq!(title(queue.advance()), Some("Track 10"));
        queue.remove_queued(song(11).entry.entry_id);
        assert_eq!(title(queue.advance()), Some("Track 10"));
        queue.remove_queued(song(10).entry.entry_id);
        assert_eq!(title(queue.advance()), Some("Track 3"));
    }

    #[test]
    fn songs_queued_during_a_rotation_play_before_the_current_one_returns() {
        let mut queue = with_songs(queue(4, 1, repeat(RepeatMode::Playlist)), [10, 11]);
        queue.advance();

        queue.enqueue(song(12)).unwrap();
        queue.enqueue(song(11)).unwrap();

        assert_eq!(queued_titles(&queue), ["Track 11", "Track 12", "Track 10"]);
    }

    #[test]
    fn repeating_one_track_excludes_queued_songs() {
        let mut repeating = queue(3, 0, repeat(RepeatMode::Track));
        let mut queuing = with_songs(queue(3, 0, repeat(RepeatMode::Playlist)), [10]);

        assert_eq!(
            repeating.enqueue(song(10)),
            Err(EnqueueError::RepeatingTrack)
        );
        assert_eq!(
            queuing.set_repeat(RepeatMode::Track),
            Err(SetRepeatError::SongsQueued)
        );
        queuing.remove_queued(song(10).entry.entry_id);
        assert_eq!(queuing.set_repeat(RepeatMode::Track), Ok(()));
    }

    #[test]
    fn next_plays_the_first_queued_song_even_when_a_track_repeats() {
        let mut queue = with_songs(queue(3, 0, repeat(RepeatMode::Off)), [10]);
        queue.advance();
        queue.set_repeat(RepeatMode::Track).unwrap();

        assert_eq!(title(queue.advance()), Some("Track 10"));
        assert_eq!(title(queue.skip_to_next()), Some("Track 2"));
    }

    #[test]
    fn previous_steps_back_through_queued_songs_to_the_interrupted_track() {
        let mut queue = with_songs(queue(4, 1, repeat(RepeatMode::Off)), [10, 11]);
        queue.advance();
        queue.advance();

        assert_eq!(
            title(queue.skip_to_previous(Duration::ZERO)),
            Some("Track 10")
        );
        assert_eq!(queued_titles(&queue), ["Track 11"]);
        assert_eq!(
            title(queue.skip_to_previous(Duration::ZERO)),
            Some("Track 2")
        );
        assert_eq!(queued_titles(&queue), ["Track 10", "Track 11"]);
        assert_eq!(
            title(queue.skip_to_previous(Duration::ZERO)),
            Some("Track 1")
        );
    }

    #[test]
    fn previous_wraps_within_rotating_queued_songs() {
        let mut queue = with_songs(queue(4, 1, repeat(RepeatMode::Playlist)), [10, 11]);
        queue.advance();

        assert_eq!(
            title(queue.skip_to_previous(Duration::ZERO)),
            Some("Track 11")
        );
        assert_eq!(queued_titles(&queue), ["Track 10", "Track 11"]);
        assert_eq!(title(queue.advance()), Some("Track 10"));
    }

    #[test]
    fn turning_repeat_on_rotates_the_songs_that_already_played() {
        let mut queue = with_songs(queue(4, 1, repeat(RepeatMode::Off)), [10, 11, 12]);
        queue.advance();
        queue.advance();

        queue.set_repeat(RepeatMode::Playlist).unwrap();
        assert_eq!(queued_titles(&queue), ["Track 12", "Track 10", "Track 11"]);
        queue.set_repeat(RepeatMode::Off).unwrap();
        assert_eq!(queued_titles(&queue), ["Track 12", "Track 10"]);
    }
}
