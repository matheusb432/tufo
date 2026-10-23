use toasty::Db;
use tufo_models::library::{CollectionEntryId, SoundSettings};

use super::{player::Player, saved_loops};

/// Applies a sound's new volume and looping to it while it plays.
pub async fn execute(
    entry_id: CollectionEntryId,
    settings: SoundSettings,
    player: &impl Player,
    mut database: Db,
) {
    player.update_sound(entry_id, settings).await;
    saved_loops::remember(&mut database, player).await;
}
