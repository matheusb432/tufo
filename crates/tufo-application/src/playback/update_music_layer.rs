use tufo_models::library::CollectionEntryId;
use tufo_wire::player::UpdateMusicLayer;

use super::player::Player;

pub async fn execute(entry_id: CollectionEntryId, update: UpdateMusicLayer, player: &impl Player) {
    player.update_music_layer(entry_id, update).await;
}
