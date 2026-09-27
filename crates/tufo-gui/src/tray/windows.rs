//! The tray in the notification area, through tray-icon. The icon is `tufo-tray.ico`, embedded as
//! resource 2, loaded at the system's small-icon size so Windows picks the matching entry.

use std::cell::Cell;

use tray_icon::{
    Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSMICON};

use super::{TrayCommand, TrayPlayback, pause_sounds_item, play_pause_item};

const TRAY_ID: &str = "tufo";
/// `tufo-gui.rc` embeds `tufo-tray.ico` under this ID.
const TRAY_ICON_RESOURCE_ID: u16 = 2;
const OPEN_ITEM_ID: &str = "open";
const PLAY_PAUSE_ITEM_ID: &str = "play-pause";
const PAUSE_SOUNDS_ITEM_ID: &str = "pause-sounds";
const QUIT_ITEM_ID: &str = "quit";

pub(crate) struct Tray {
    _icon: TrayIcon,
    play_pause: MenuItem,
    pause_sounds: MenuItem,
    playback: Cell<TrayPlayback>,
}

impl Tray {
    pub(crate) fn show(
        on_command: impl Fn(TrayCommand) + Clone + Send + Sync + 'static,
    ) -> anyhow::Result<Self> {
        let open = MenuItem::with_id(OPEN_ITEM_ID, crate::i18n::tr("Open Tufo"), true, None);
        let playback = TrayPlayback::default();
        let (play_pause_label, play_pause_enabled) = play_pause_item(playback.music);
        let play_pause = MenuItem::with_id(
            PLAY_PAUSE_ITEM_ID,
            play_pause_label,
            play_pause_enabled,
            None,
        );
        let (pause_sounds_label, pause_sounds_enabled) = pause_sounds_item(playback.sounds);
        let pause_sounds = MenuItem::with_id(
            PAUSE_SOUNDS_ITEM_ID,
            pause_sounds_label,
            pause_sounds_enabled,
            None,
        );
        let quit = MenuItem::with_id(QUIT_ITEM_ID, crate::i18n::tr("Quit Tufo"), true, None);
        let menu = Menu::with_items(&[
            &open,
            &play_pause,
            &pause_sounds,
            &PredefinedMenuItem::separator(),
            &quit,
        ])?;
        let on_menu_command = on_command.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            if let Some(command) = menu_command(event.id.as_ref()) {
                on_menu_command(command);
            }
        }));
        TrayIconEvent::set_event_handler(Some(move |event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                on_command(TrayCommand::ShowWindow);
            }
        }));
        let icon = TrayIconBuilder::new()
            .with_id(TRAY_ID)
            .with_icon(tray_icon()?)
            .with_tooltip("Tufo")
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .build()?;
        Ok(Self {
            _icon: icon,
            play_pause,
            pause_sounds,
            playback: Cell::new(playback),
        })
    }

    pub(crate) fn show_playback(&self, playback: TrayPlayback) {
        if self.playback.replace(playback) == playback {
            return;
        }
        let (label, enabled) = play_pause_item(playback.music);
        self.play_pause.set_text(label);
        self.play_pause.set_enabled(enabled);
        let (label, enabled) = pause_sounds_item(playback.sounds);
        self.pause_sounds.set_text(label);
        self.pause_sounds.set_enabled(enabled);
    }
}

fn tray_icon() -> anyhow::Result<Icon> {
    // SAFETY: GetSystemMetrics has no preconditions.
    let small_icon_side = unsafe { GetSystemMetrics(SM_CXSMICON) };
    let dimensions = u32::try_from(small_icon_side)
        .ok()
        .filter(|&side| side > 0)
        .map(|side| (side, side));
    Ok(Icon::from_resource(TRAY_ICON_RESOURCE_ID, dimensions)?)
}

fn menu_command(item_id: &str) -> Option<TrayCommand> {
    match item_id {
        OPEN_ITEM_ID => Some(TrayCommand::ShowWindow),
        PLAY_PAUSE_ITEM_ID => Some(TrayCommand::TogglePlayPause),
        PAUSE_SOUNDS_ITEM_ID => Some(TrayCommand::ToggleSoundsPaused),
        QUIT_ITEM_ID => Some(TrayCommand::Quit),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_menu_item_maps_to_a_command() {
        assert_eq!(menu_command(OPEN_ITEM_ID), Some(TrayCommand::ShowWindow));
        assert_eq!(
            menu_command(PLAY_PAUSE_ITEM_ID),
            Some(TrayCommand::TogglePlayPause)
        );
        assert_eq!(
            menu_command(PAUSE_SOUNDS_ITEM_ID),
            Some(TrayCommand::ToggleSoundsPaused)
        );
        assert_eq!(menu_command(QUIT_ITEM_ID), Some(TrayCommand::Quit));
        assert_eq!(menu_command("separator"), None);
    }
}
