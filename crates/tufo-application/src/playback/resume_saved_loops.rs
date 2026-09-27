//! Playing the looping sounds left playing before, as a login launch does.

use tufo_models::playback::saved_loops::SavedLoop;

use super::{play_entry_sound, player::Player, saved_loops};
use crate::library::{Library, LibraryStorageError, library_files::LibraryFiles};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeSavedLoopsOutcome {
    NothingSaved,
    LeftPaused,
    Resumed { loops: usize },
}

pub async fn execute<F: LibraryFiles>(
    library: &Library<F>,
    player: &impl Player,
) -> Result<ResumeSavedLoopsOutcome, LibraryStorageError> {
    let mut database = library.database();
    let saved = SavedLoop::all().exec(&mut database).await?;
    if saved.is_empty() {
        return Ok(ResumeSavedLoopsOutcome::NothingSaved);
    }
    if saved.iter().any(|saved_loop| saved_loop.paused) {
        return Ok(ResumeSavedLoopsOutcome::LeftPaused);
    }
    let mut loops = 0;
    for saved_loop in saved {
        let entry_id = saved_loop.collection_entry_id;
        match play_entry_sound::execute(entry_id, library, player).await {
            Ok(()) => loops += 1,
            Err(error) => {
                tracing::warn!(%error, %entry_id, "cannot resume a saved loop");
                saved_loops::forget(&mut database, entry_id).await;
            }
        }
    }
    Ok(ResumeSavedLoopsOutcome::Resumed { loops })
}
