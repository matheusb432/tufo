//! Recording the looping sounds left playing. A failed write only logs, since the sound itself
//! already changed.

use toasty::Db;
use tufo_models::{
    library::CollectionEntryId,
    playback::{OutputTarget, player_state::PlayerState, saved_loops::SavedLoop},
};

use crate::library::LibraryStorageError;

const PAUSE_ALL: &str = "UPDATE saved_loops SET paused = 1";
const UNPAUSE_ALL: &str = "UPDATE saved_loops SET paused = 0";

pub(super) async fn remember_if_looping_locally(
    database: &mut Db,
    entry_id: CollectionEntryId,
    state: &PlayerState,
) {
    let looping_locally = state.output == OutputTarget::Local
        && state
            .sounds
            .iter()
            .any(|sound| sound.entry_id == entry_id && sound.looping);
    if looping_locally {
        let saved = replace(database, entry_id, state.sounds_paused).await;
        warn_unless_saved(saved);
    }
}

pub(super) async fn forget(database: &mut Db, entry_id: CollectionEntryId) {
    let deleted = SavedLoop::filter_by_collection_entry_id(entry_id)
        .delete()
        .exec(database)
        .await;
    warn_unless_saved(deleted.map_err(LibraryStorageError::from));
}

pub(super) async fn remember_paused(database: &mut Db, paused: bool) {
    let statement = if paused { PAUSE_ALL } else { UNPAUSE_ALL };
    let updated = toasty::sql::statement(statement).exec(database).await;
    warn_unless_saved(updated.map(drop).map_err(LibraryStorageError::from));
}

async fn replace(
    database: &mut Db,
    entry_id: CollectionEntryId,
    paused: bool,
) -> Result<(), LibraryStorageError> {
    SavedLoop::filter_by_collection_entry_id(entry_id)
        .delete()
        .exec(database)
        .await?;
    toasty::create!(SavedLoop {
        collection_entry_id: entry_id,
        paused,
    })
    .exec(database)
    .await?;
    Ok(())
}

fn warn_unless_saved(result: Result<(), LibraryStorageError>) {
    if let Err(error) = result {
        tracing::warn!(%error, "cannot save the looping sounds");
    }
}
