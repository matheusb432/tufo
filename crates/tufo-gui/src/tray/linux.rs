//! The tray as a `StatusNotifierItem` served by ksni. The panel finds the icon by name in a
//! private icon folder and draws the SVG at its exact size; pixmaps at the common panel sizes
//! cover panels that don't read icon folders, which pick the smallest one at least as large as
//! their icon.

use std::{cell::Cell, sync::Arc};

use anyhow::{Context as _, anyhow};
use ksni::blocking::TrayMethods as _;
use tufo_local_transport::LocalEndpoint;

use super::{
    TRAY_ICON_SVG, TrayCommand, TrayPlayback, draw_icon, pause_sounds_item, play_pause_item,
};

/// The icon's file name, without `.svg`, in the folder the tray publishes.
const TRAY_ICON_NAME: &str = "tufo-tray";
/// Panel icon sizes at 100% and 200% scaling.
const PIXMAP_SIDES_PX: [u32; 9] = [16, 20, 22, 24, 32, 40, 44, 48, 64];

type OnCommand = Arc<dyn Fn(TrayCommand) + Send + Sync>;

struct StatusNotifier {
    playback: TrayPlayback,
    on_command: OnCommand,
    pixmaps: Vec<ksni::Icon>,
    /// The folder holding the icon, or empty when it couldn't be written.
    icon_folder: String,
}

impl ksni::Tray for StatusNotifier {
    fn id(&self) -> String {
        "tufo".to_owned()
    }

    fn title(&self) -> String {
        "Tufo".to_owned()
    }

    fn icon_name(&self) -> String {
        if self.icon_folder.is_empty() {
            String::new()
        } else {
            TRAY_ICON_NAME.to_owned()
        }
    }

    fn icon_theme_path(&self) -> String {
        self.icon_folder.clone()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        self.pixmaps.clone()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: "Tufo".to_owned(),
            ..ksni::ToolTip::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        (self.on_command)(TrayCommand::ShowWindow);
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        let (play_pause_label, play_pause_enabled) = play_pause_item(self.playback.music);
        let (pause_sounds_label, pause_sounds_enabled) = pause_sounds_item(self.playback.sounds);
        vec![
            menu_item(crate::i18n::tr("Open"), true, TrayCommand::ShowWindow),
            menu_item(
                crate::i18n::tr("Mini Player"),
                true,
                TrayCommand::ShowMiniPlayer,
            ),
            menu_item(
                play_pause_label,
                play_pause_enabled,
                TrayCommand::TogglePlayPause,
            ),
            menu_item(
                pause_sounds_label,
                pause_sounds_enabled,
                TrayCommand::ToggleSoundsPaused,
            ),
            ksni::MenuItem::Separator,
            menu_item(crate::i18n::tr("Close"), true, TrayCommand::Quit),
        ]
    }
}

fn menu_item(label: &str, enabled: bool, command: TrayCommand) -> ksni::MenuItem<StatusNotifier> {
    ksni::menu::StandardItem {
        label: label.to_owned(),
        enabled,
        activate: Box::new(move |tray: &mut StatusNotifier| (tray.on_command)(command)),
        ..ksni::menu::StandardItem::default()
    }
    .into()
}

pub(crate) struct Tray {
    handle: ksni::blocking::Handle<StatusNotifier>,
    playback: Cell<TrayPlayback>,
}

impl Tray {
    /// Shows the icon. It fails when the session has no `StatusNotifierItem` host, such as GNOME
    /// without an `AppIndicator` extension.
    pub(crate) fn show(
        on_command: impl Fn(TrayCommand) + Clone + Send + Sync + 'static,
    ) -> anyhow::Result<Self> {
        let icon_folder = publish_icon().unwrap_or_else(|error| {
            tracing::warn!(%error, "the tray icon falls back to pixmaps");
            String::new()
        });
        let handle = StatusNotifier {
            playback: TrayPlayback::default(),
            on_command: Arc::new(on_command),
            pixmaps: pixmaps()?,
            icon_folder,
        }
        .spawn()?;
        Ok(Self {
            handle,
            playback: Cell::new(TrayPlayback::default()),
        })
    }

    pub(crate) fn show_playback(&self, playback: TrayPlayback) {
        if self.playback.replace(playback) != playback {
            self.handle.update(|tray| tray.playback = playback);
        }
    }
}

/// Writes the icon into an `icons` folder in the private runtime directory, which the panel,
/// running as the same user, can read.
fn publish_icon() -> anyhow::Result<String> {
    let endpoint = LocalEndpoint::gui_from_environment()?;
    let runtime = endpoint
        .path()
        .parent()
        .context("the GUI's endpoint has no directory")?;
    let folder = runtime.join("icons");
    std::fs::create_dir_all(&folder).with_context(|| format!("create {}", folder.display()))?;
    let icon = folder.join(format!("{TRAY_ICON_NAME}.svg"));
    std::fs::write(&icon, TRAY_ICON_SVG).with_context(|| format!("write {}", icon.display()))?;
    folder
        .into_os_string()
        .into_string()
        .map_err(|_| anyhow!("the icon folder's path is not UTF-8"))
}

/// The icon at every panel size, as straight-alpha ARGB in network byte order.
fn pixmaps() -> anyhow::Result<Vec<ksni::Icon>> {
    PIXMAP_SIDES_PX
        .into_iter()
        .map(|side| {
            let data = draw_icon(side)?
                .into_iter()
                .flat_map(|color| [color.alpha(), color.red(), color.green(), color.blue()])
                .collect();
            let side = i32::try_from(side)?;
            Ok(ksni::Icon {
                width: side,
                height: side,
                data,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use ksni::Tray as _;

    use super::*;
    use crate::tray::{TrayMusic, TraySounds};

    fn status_notifier(music: TrayMusic, sounds: TraySounds) -> StatusNotifier {
        StatusNotifier {
            playback: TrayPlayback { music, sounds },
            on_command: Arc::new(|_| {}),
            pixmaps: Vec::new(),
            icon_folder: String::new(),
        }
    }

    fn labels(tray: &StatusNotifier) -> Vec<(String, bool)> {
        tray.menu()
            .into_iter()
            .filter_map(|item| match item {
                ksni::MenuItem::Standard(item) => Some((item.label, item.enabled)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn every_panel_size_gets_a_pixmap_of_that_size() {
        let pixmaps = pixmaps().unwrap();

        assert_eq!(pixmaps.len(), PIXMAP_SIDES_PX.len());
        for (pixmap, side) in pixmaps.iter().zip(PIXMAP_SIDES_PX) {
            let side = i32::try_from(side).unwrap();
            assert_eq!((pixmap.width, pixmap.height), (side, side));
            assert_eq!(pixmap.data.len(), usize::try_from(side * side * 4).unwrap());
        }
    }

    #[test]
    fn pixmaps_put_alpha_first() {
        let pixmap = pixmaps().unwrap().pop().unwrap();
        let (x, y) = (pixmap.width / 2, pixmap.height / 4);
        let forehead = usize::try_from((y * pixmap.width + x) * 4).unwrap();

        // The forehead's fur is the opaque light green, #95d5b2.
        assert_eq!(
            &pixmap.data[forehead..forehead + 4],
            &[255, 0x95, 0xd5, 0xb2]
        );
    }

    #[test]
    fn the_menu_follows_the_music_and_the_sounds() {
        assert_eq!(
            labels(&status_notifier(TrayMusic::Playing, TraySounds::Paused)),
            [
                ("Open".to_owned(), true),
                ("Mini Player".to_owned(), true),
                ("Pause".to_owned(), true),
                ("Resume Sounds".to_owned(), true),
                ("Close".to_owned(), true)
            ]
        );
        assert_eq!(
            labels(&status_notifier(TrayMusic::Stopped, TraySounds::Silent))[2..4],
            [
                ("Play".to_owned(), false),
                ("Pause Sounds".to_owned(), false)
            ]
        );
    }

    #[test]
    fn without_an_icon_folder_the_panel_uses_the_pixmaps() {
        assert_eq!(
            status_notifier(TrayMusic::Stopped, TraySounds::Silent).icon_name(),
            ""
        );
    }
}
