//! Changing how loud the browser's pages play.

use tufo_models::library::Volume;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BrowserCommand {
    SetVolume(Volume),
    SetMuted(bool),
}
