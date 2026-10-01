//! The tray icon. A left click shows the window; the menu opens the window or the mini player,
//! plays or pauses the music, pauses or resumes the sounds, and quits.
//!
//! Linux serves a `StatusNotifierItem` over D-Bus with ksni; Windows and macOS use tray-icon, in
//! the notification area and the menu bar. Each shows `icons/tufo-tray.svg` drawn at the size the
//! panel uses, so it never gets shrunk from a larger image. The tray belongs to the UI thread;
//! clicks and menu events arrive on any thread.

use crate::i18n::tr;

#[cfg(any(windows, target_os = "macos"))]
mod icon_menu;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

#[cfg(any(windows, target_os = "macos"))]
pub(crate) use icon_menu::Tray;
#[cfg(target_os = "linux")]
pub(crate) use linux::Tray;

#[cfg(any(target_os = "linux", target_os = "macos"))]
const TRAY_ICON_SVG: &[u8] = include_bytes!("../icons/tufo-tray.svg");

/// What the tray asks the window to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrayCommand {
    ShowWindow,
    ShowMiniPlayer,
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

/// The tray icon drawn `side` pixels square, as straight-alpha colors row by row.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn draw_icon(side: u32) -> anyhow::Result<Vec<resvg::tiny_skia::ColorU8>> {
    use anyhow::Context as _;
    use resvg::{tiny_skia, usvg};

    let tree = usvg::Tree::from_data(TRAY_ICON_SVG, &usvg::Options::default())
        .context("parse the tray icon")?;
    let mut pixmap = tiny_skia::Pixmap::new(side, side).context("allocate a tray pixmap")?;
    #[expect(
        clippy::cast_precision_loss,
        reason = "icon sides are far inside f32's exact integer range"
    )]
    let scale = side as f32 / tree.size().width();
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    Ok(pixmap
        .pixels()
        .iter()
        .map(tiny_skia::PremultipliedColorU8::demultiply)
        .collect())
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
