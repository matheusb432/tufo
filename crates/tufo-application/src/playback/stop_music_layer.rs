use tufo_models::library::CollectionEntryId;

use super::player::Player;

/// Stopping a song no longer in the mix succeeds.
pub async fn execute(entry_id: CollectionEntryId, player: &impl Player) {
    player.stop_music_layer(entry_id).await;
}
