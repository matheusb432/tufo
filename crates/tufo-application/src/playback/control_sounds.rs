//! Controlling every sound together, leaving the music alone.

use toasty::Db;
use tufo_models::library::Volume;

use super::{player::Player, saved_loops};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SoundsCommand {
    Pause,
    Resume,
    SetVolume(Volume),
}

/// Applies with or without playing sounds. Repeating a command is harmless.
pub async fn execute(command: SoundsCommand, player: &impl Player, mut database: Db) {
    player.control_sounds(command).await;
    saved_loops::remember(&mut database, player).await;
}
