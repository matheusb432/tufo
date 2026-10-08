//! The browser's part in playback: whether it runs, and how loud its pages play.

use crate::library::Volume;

/// The browser as the player mixes it.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct BrowserPlayback {
    pub status: BrowserStatus,
    /// Scales every page's audio, independently of the music and effects.
    pub volume: Volume,
    pub muted: bool,
    /// Some page sends audio now.
    pub audible: bool,
}

/// Whether the browser runs. Pages load only while it is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BrowserStatus {
    /// Never opened, or closed on request.
    #[default]
    Closed,
    Starting,
    Open,
    /// Closed without a request; opening it again clears the reason.
    Stopped(BrowserStopReason),
}

/// Why the browser closed on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserStopReason {
    /// No page played and no view showed the browser for a while, so it released its memory.
    Idle,
    /// The browser exited unexpectedly.
    Crashed,
    /// The browser could not start, or did not become ready in time.
    StartFailed,
    /// This installation has no browser.
    Unavailable,
}
