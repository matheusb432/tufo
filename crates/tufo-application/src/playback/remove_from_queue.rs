use tufo_models::library::CollectionEntryId;

use super::player::Player;

/// Removing a song that isn't queued succeeds.
pub async fn execute(entry_id: CollectionEntryId, player: &impl Player) {
    player.remove_from_queue(entry_id).await;
}
