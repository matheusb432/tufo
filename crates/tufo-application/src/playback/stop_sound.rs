use toasty::Db;
use tufo_models::library::CollectionEntryId;

use super::{player::Player, saved_loops};

/// Fades a playing sound out. Stopping a silent entry succeeds, so repeating it is harmless.
pub async fn execute(entry_id: CollectionEntryId, player: &impl Player, mut database: Db) {
    player.stop_sound(entry_id).await;
    saved_loops::forget(&mut database, entry_id).await;
}
