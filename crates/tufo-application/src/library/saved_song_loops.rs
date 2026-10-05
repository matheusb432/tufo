//! The loops songs start with, read from the latest snapshot so playback never waits on the
//! database.

use std::sync::Arc;

use tokio::sync::watch;
use tufo_models::library::{AudioFileId, song_loop::LoopRegion};
use tufo_wire::library::LibrarySnapshot;

use super::{Library, library_files::LibraryFiles};
use crate::playback::player::SongLoops;

pub struct SavedSongLoops {
    snapshots: watch::Receiver<Arc<LibrarySnapshot>>,
}

impl<F: LibraryFiles> Library<F> {
    #[must_use]
    pub fn saved_song_loops(&self) -> SavedSongLoops {
        SavedSongLoops {
            snapshots: self.watch(),
        }
    }
}

impl SongLoops for SavedSongLoops {
    fn loop_region(&self, audio_file_id: AudioFileId) -> Option<LoopRegion> {
        self.snapshots
            .borrow()
            .saved_song_loop(audio_file_id)
            .filter(|saved| !saved.needs_review)
            .map(|saved| saved.region)
    }
}
