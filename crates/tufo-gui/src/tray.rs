//! The tray icon. A left click shows the window; the menu opens the window, plays or pauses the
//! music, pauses or resumes the sounds, and quits.
//!
//! Linux serves a `StatusNotifierItem` over D-Bus with ksni, and Windows uses the notification
//! area through tray-icon. Both show `icons/tufo-tray.svg` drawn at the size the panel uses, so it
//! never gets shrunk from a larger image. The tray belongs to the UI thread; clicks and menu events
//! arrive on any thread.

use crate::i18n::tr;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
pub(crate) use linux::Tray;
#[cfg(windows)]
pub(crate) use windows::Tray;

/// What the tray asks the window to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrayCommand {
    ShowWindow,
    TogglePlayPause,
    ToggleSoundsPaused,
    Quit,
}

/// The music's state, which decides what the Play/Pause item offers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum TrayMusic {
    #[default]
    Stopped,
    Playing,
    Paused,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum TraySounds {
    #[default]
    Silent,
    Playing,
    Paused,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct TrayPlayback {
    pub(crate) music: TrayMusic,
    pub(crate) sounds: TraySounds,
}

/// The Play/Pause item's label, and whether it can be clicked.
fn play_pause_item(music: TrayMusic) -> (&'static str, bool) {
    match music {
        TrayMusic::Stopped => (tr("Play"), false),
        TrayMusic::Playing => (tr("Pause"), true),
        TrayMusic::Paused => (tr("Play"), true),
    }
}

fn pause_sounds_item(sounds: TraySounds) -> (&'static str, bool) {
    match sounds {
        TraySounds::Silent => (tr("Pause Sounds"), false),
        TraySounds::Playing => (tr("Pause Sounds"), true),
        TraySounds::Paused => (tr("Resume Sounds"), true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_play_pause_item_offers_the_opposite_of_the_music() {
        assert_eq!(play_pause_item(TrayMusic::Playing), ("Pause", true));
        assert_eq!(play_pause_item(TrayMusic::Paused), ("Play", true));
        assert_eq!(play_pause_item(TrayMusic::Stopped), ("Play", false));
    }

    #[test]
    fn the_sounds_item_offers_the_opposite_of_the_sounds() {
        assert_eq!(
            pause_sounds_item(TraySounds::Playing),
            ("Pause Sounds", true)
        );
        assert_eq!(
            pause_sounds_item(TraySounds::Paused),
            ("Resume Sounds", true)
        );
        assert_eq!(
            pause_sounds_item(TraySounds::Silent),
            ("Pause Sounds", false)
        );
    }
}
