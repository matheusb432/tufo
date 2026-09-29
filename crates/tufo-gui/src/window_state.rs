//! The window's layout between launches, in `window.toml` in the state directory, and the switch
//! between the full window and the mini player.

use std::{io, path::PathBuf, time::Duration};

use slint::{ComponentHandle as _, LogicalSize};
use tokio::{runtime::Handle, sync::watch};

use crate::{
    files,
    ui::{Actions, AppWindow, Session, WindowFrame},
};

const WINDOW_STATE_FILE_NAME: &str = "window.toml";
/// Lets a drag that resizes the window settle before its size is written.
const SAVE_DELAY: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, Copy, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub(crate) struct WindowState {
    pub(crate) drawer_pinned: bool,
    /// The player shows as one slim row in a narrow window.
    pub(crate) player_collapsed: bool,
    pub(crate) form: WindowForm,
    /// The mini player stays above other windows.
    pub(crate) mini_player_pinned: bool,
    /// The size the full window returns to from the mini player.
    pub(crate) full_size: WindowSize,
    pub(crate) mini_player_size: WindowSize,
}

impl Default for WindowState {
    fn default() -> Self {
        Self {
            drawer_pinned: true,
            player_collapsed: false,
            form: WindowForm::Full,
            mini_player_pinned: true,
            full_size: WindowSize::FULL_DEFAULT,
            mini_player_size: WindowSize::MINI_PLAYER_DEFAULT,
        }
    }
}

/// What the window shows: every page beside the sidebar, or the player alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WindowForm {
    Full,
    MiniPlayer,
}

impl WindowForm {
    fn from_mini_player(mini_player: bool) -> Self {
        if mini_player {
            Self::MiniPlayer
        } else {
            Self::Full
        }
    }

    fn is_mini_player(self) -> bool {
        self == Self::MiniPlayer
    }
}

impl WindowState {
    /// Switches the window's form, returning the size it takes, or `None` if it already shows in
    /// that form.
    pub(crate) fn switch_form(&mut self, form: WindowForm) -> Option<WindowSize> {
        if self.form == form {
            return None;
        }
        self.form = form;
        Some(self.size_shown())
    }

    /// Remembers `size` for the form the window shows in. The mini player refuses a size larger
    /// than it can take, such as the size a desktop restores the window to after maximizing, and
    /// returns its own size to take back.
    pub(crate) fn record_size(&mut self, size: WindowSize) -> Option<WindowSize> {
        if self.form == WindowForm::Full {
            self.full_size = size;
            return None;
        }
        if size.fits_within(WindowSize::MINI_PLAYER_MAX) {
            self.mini_player_size = size;
            None
        } else {
            Some(self.mini_player_size)
        }
    }

    pub(crate) fn size_shown(&self) -> WindowSize {
        match self.form {
            WindowForm::Full => self.full_size,
            WindowForm::MiniPlayer => self.mini_player_size,
        }
    }
}

/// A window's size in logical pixels.
#[derive(Debug, Clone, Copy, PartialEq, serde::Deserialize, serde::Serialize)]
pub(crate) struct WindowSize {
    pub(crate) width: f32,
    pub(crate) height: f32,
}

impl WindowSize {
    /// Fits a 1280 × 800 screen at 125% scaling.
    const FULL_DEFAULT: Self = Self {
        width: 1000.0,
        height: 700.0,
    };
    const MINI_PLAYER_DEFAULT: Self = Self {
        width: 360.0,
        height: 168.0,
    };
    const MINI_PLAYER_MAX: Self = Self {
        width: 720.0,
        height: 400.0,
    };

    fn fits_within(self, bound: Self) -> bool {
        self.width <= bound.width && self.height <= bound.height
    }
}

#[derive(Debug, Clone)]
pub(crate) struct WindowStateFile {
    path: PathBuf,
}

impl WindowStateFile {
    pub(crate) fn from_environment() -> Option<Self> {
        let directories = directories::ProjectDirs::from("", "", "tufo")?;
        let folder = directories
            .state_dir()
            .unwrap_or_else(|| directories.data_local_dir());
        Some(Self::at(folder.join(WINDOW_STATE_FILE_NAME)))
    }

    pub(crate) fn at(path: PathBuf) -> Self {
        Self { path }
    }

    pub(crate) fn load(&self) -> WindowState {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(error) => {
                if error.kind() != io::ErrorKind::NotFound {
                    tracing::warn!(%error, path = %self.path.display(), "cannot read the window's layout");
                }
                return WindowState::default();
            }
        };
        toml::from_str(&text).unwrap_or_else(|error| {
            tracing::warn!(%error, path = %self.path.display(), "the window's layout is not valid; using the defaults");
            WindowState::default()
        })
    }

    pub(crate) fn save(&self, state: WindowState) -> io::Result<()> {
        let text = toml::to_string(&state).map_err(io::Error::other)?;
        files::write_atomically(&self.path, &text)
    }
}

/// Restores the layout before the window first shows, switches between the full window and the
/// mini player, and saves each change off the UI thread when `file` is given.
pub(crate) fn install(window: &AppWindow, runtime: &Handle, file: Option<WindowStateFile>) {
    let state = file.as_ref().map(WindowStateFile::load).unwrap_or_default();
    let session = window.global::<Session>();
    session.set_drawer_pinned(state.drawer_pinned);
    session.set_player_collapsed(state.player_collapsed);
    session.set_mini_player(state.form.is_mini_player());
    session.set_mini_player_pinned(state.mini_player_pinned);
    session.set_always_on_top_supported(always_on_top_supported());
    if state.form == WindowForm::MiniPlayer {
        set_size(window, state.mini_player_size);
    }
    let (layout_sender, layout_receiver) = watch::channel(state);
    let actions = window.global::<Actions>();
    actions.on_set_drawer_pinned({
        let sender = layout_sender.clone();
        move |drawer_pinned| sender.send_modify(|state| state.drawer_pinned = drawer_pinned)
    });
    actions.on_set_player_collapsed({
        let sender = layout_sender.clone();
        move |player_collapsed| {
            sender.send_modify(|state| state.player_collapsed = player_collapsed);
        }
    });
    actions.on_set_mini_player_pinned({
        let sender = layout_sender.clone();
        move |pinned| sender.send_modify(|state| state.mini_player_pinned = pinned)
    });
    let frame = window.global::<WindowFrame>();
    frame.on_set_mini_player({
        let sender = layout_sender.clone();
        let window = window.as_weak();
        move |mini_player| {
            let Some(window) = window.upgrade() else {
                return;
            };
            let mut size = None;
            sender.send_if_modified(|state| {
                size = state.switch_form(WindowForm::from_mini_player(mini_player));
                size.is_some()
            });
            let Some(size) = size else {
                return;
            };
            let slint_window = window.window();
            slint_window.set_fullscreen(false);
            slint_window.set_maximized(false);
            // The limits on the window's size follow the form it shows in, so they must change
            // before the size does.
            window.global::<Session>().set_mini_player(mini_player);
            set_size(&window, size);
        }
    });
    frame.on_resized({
        let window = window.as_weak();
        move |width, height| {
            let mut refused = None;
            layout_sender.send_if_modified(|state| {
                let before = *state;
                refused = state.record_size(WindowSize { width, height });
                *state != before
            });
            if let (Some(size), Some(window)) = (refused, window.upgrade()) {
                set_size(&window, size);
            }
        }
    });
    if let Some(file) = file {
        runtime.spawn(save_changes(file, layout_receiver));
    }
}

async fn save_changes(file: WindowStateFile, mut layout_receiver: watch::Receiver<WindowState>) {
    while layout_receiver.changed().await.is_ok() {
        tokio::time::sleep(SAVE_DELAY).await;
        let state = *layout_receiver.borrow_and_update();
        let file = file.clone();
        let saved = tokio::task::spawn_blocking(move || file.save(state))
            .await
            .unwrap_or_else(|error| Err(io::Error::other(error)));
        if let Err(error) = saved {
            tracing::warn!(%error, "cannot save the window's layout");
        }
    }
}

fn set_size(window: &AppWindow, size: WindowSize) {
    window
        .window()
        .set_size(LogicalSize::new(size.width, size.height));
}

/// Wayland leaves keeping a window above the others to the desktop's window menu, and the window
/// runs natively on Wayland in a Wayland session.
fn always_on_top_supported() -> bool {
    !cfg!(target_os = "linux") || std::env::var_os("WAYLAND_DISPLAY").is_none()
}

#[cfg(test)]
mod tests;
