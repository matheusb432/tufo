//! Recording the looping sounds left playing. A failed write only logs, since the sound itself
//! already changed.

use toasty::Db;
use toasty_core::driver::operation::TransactionMode;
use tufo_models::{
    library::{CollectionEntry, CollectionEntryId},
    playback::saved_loops::SavedLoop,
};

use super::player::Player;
use crate::library::LibraryStorageError;

pub(super) async fn remember(database: &mut Db, player: &impl Player) {
    warn_unless_saved(replace(database, player).await);
}

pub(super) async fn forget(database: &mut Db, entry_id: CollectionEntryId) {
    let deleted = SavedLoop::filter_by_collection_entry_id(entry_id)
        .delete()
        .exec(database)
        .await;
    warn_unless_saved(deleted.map_err(LibraryStorageError::from));
}

async fn replace(database: &mut Db, player: &impl Player) -> Result<(), LibraryStorageError> {
    let mut transaction = database
        .transaction_builder()
        .mode(TransactionMode::Immediate)
        .begin()
        .await?;
    // Read after acquiring the write lock, so a delayed request cannot save an older session.
    let state = player.sounds_to_resume().await;
    SavedLoop::all().delete().exec(&mut transaction).await?;
    for entry_id in state.entry_ids {
        // Library deletion may have removed an entry while its sound was still playing.
        if CollectionEntry::filter_by_id(entry_id)
            .first()
            .exec(&mut transaction)
            .await?
            .is_none()
        {
            continue;
        }
        toasty::create!(SavedLoop {
            collection_entry_id: entry_id,
            paused: state.paused,
        })
        .exec(&mut transaction)
        .await?;
    }
    transaction.commit().await?;
    Ok(())
}

fn warn_unless_saved(result: Result<(), LibraryStorageError>) {
    if let Err(error) = result {
        tracing::warn!(%error, "cannot save the looping sounds");
    }
}
