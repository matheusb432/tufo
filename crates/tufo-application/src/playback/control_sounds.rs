//! Pausing or resuming every sound together, leaving the music alone.

use toasty::Db;

use super::{player::Player, saved_loops};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoundsCommand {
    Pause,
    Resume,
}

/// Succeeds with nothing to pause or resume, so repeating it is harmless.
pub async fn execute(command: SoundsCommand, player: &impl Player, mut database: Db) {
    player.control_sounds(command).await;
    let paused = player.watch_player().borrow().sounds_paused;
    saved_loops::remember_paused(&mut database, paused).await;
}
