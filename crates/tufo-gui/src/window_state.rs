//! The window's layout between launches, in `window.toml` in the state directory, and the switch
//! between the full window and the mini player.

use std::{io, path::PathBuf, time::Duration};

use slint::{
    ComponentHandle as _, LogicalSize,
    winit_030::{WinitWindowAccessor as _, winit},
};
use tokio::{runtime::Handle, sync::watch};
use tufo_client::desktop::files;

use crate::ui::{Actions, AppWindow, Session, Theme, WindowFrame};

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

    /// Returns a correction for an out-of-bounds size. A late desktop restore must not replace
    /// the mini player's remembered width.
    fn record_size(&mut self, size: WindowSize, limits: WindowSizeLimits) -> Option<WindowSize> {
        if self.form == WindowForm::Full {
            self.full_size = size.bounded(limits.full_minimum, None, self.full_size);
            return (size != self.full_size).then_some(self.full_size);
        }
        if !size.fits_within(limits.mini_player_maximum) {
            return Some(self.mini_player_size);
        }
        self.mini_player_size = size.bounded(
            limits.mini_player_minimum,
            Some(limits.mini_player_maximum),
            self.mini_player_size,
        );
        (size != self.mini_player_size).then_some(self.mini_player_size)
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
    fn fits_within(self, bound: Self) -> bool {
        self.width <= bound.width && self.height <= bound.height
    }

    fn bounded(self, minimum: Self, maximum: Option<Self>, fallback: Self) -> Self {
        let width = if self.width.is_finite() {
            self.width
        } else {
            fallback.width
        };
        let height = if self.height.is_finite() {
            self.height
        } else {
            fallback.height
        };
        Self {
            width: width
                .max(minimum.width)
                .min(maximum.map_or(f32::MAX, |size| size.width)),
            height: height
                .max(minimum.height)
                .min(maximum.map_or(f32::MAX, |size| size.height)),
        }
    }
}

#[derive(Clone, Copy)]
struct WindowSizeLimits {
    full_minimum: WindowSize,
    mini_player_minimum: WindowSize,
    mini_player_maximum: WindowSize,
}

impl WindowSizeLimits {
    fn from_window(window: &AppWindow) -> Self {
        let theme = window.global::<Theme>();
        Self {
            full_minimum: WindowSize {
                width: theme.get_window_width_min(),
                height: theme.get_window_height_min(),
            },
            mini_player_minimum: WindowSize {
                width: theme.get_mini_player_width_min(),
                height: theme.get_mini_player_height_min(),
            },
            mini_player_maximum: WindowSize {
                width: theme.get_mini_player_width_max(),
                height: theme.get_mini_player_height_min(),
            },
        }
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

    fn load(&self, limits: WindowSizeLimits) -> WindowState {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(error) => {
                if error.kind() != io::ErrorKind::NotFound {
                    tracing::warn!(%error, path = %self.path.display(), "cannot read the window's layout");
                }
                return WindowState::default();
            }
        };
        let mut state: WindowState = toml::from_str(&text).unwrap_or_else(|error| {
            tracing::warn!(%error, path = %self.path.display(), "the window's layout is not valid; using the defaults");
            WindowState::default()
        });
        state.full_size =
            state
                .full_size
                .bounded(limits.full_minimum, None, WindowSize::FULL_DEFAULT);
        state.mini_player_size = state.mini_player_size.bounded(
            limits.mini_player_minimum,
            Some(limits.mini_player_maximum),
            WindowSize::MINI_PLAYER_DEFAULT,
        );
        state
    }

    pub(crate) fn save(&self, state: WindowState) -> io::Result<()> {
        let text = toml::to_string(&state).map_err(io::Error::other)?;
        files::write_atomically(&self.path, &text)
    }
}

/// Restores the layout before the window first shows, switches between the full window and the
/// mini player, and saves each change off the UI thread when `file` is given.
pub(crate) fn install(window: &AppWindow, runtime: &Handle, file: Option<WindowStateFile>) {
    let limits = WindowSizeLimits::from_window(window);
    let state = file
        .as_ref()
        .map(|file| file.load(limits))
        .unwrap_or_default();
    let session = window.global::<Session>();
    session.set_drawer_pinned(state.drawer_pinned);
    session.set_player_collapsed(state.player_collapsed);
    session.set_mini_player(state.form.is_mini_player());
    session.set_mini_player_pinned(state.mini_player_pinned);
    session.set_always_on_top_supported(always_on_top_supported());
    set_size(window, state.size_shown());
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
            apply_size_limits(&window);
            set_size(&window, size);
        }
    });
    frame.on_resized({
        let window = window.as_weak();
        move |width, height| {
            let mut refused = None;
            layout_sender.send_if_modified(|state| {
                let before = *state;
                refused = state.record_size(WindowSize { width, height }, limits);
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

/// Slint 1.18 recreates a hidden Wayland window with default size hints while retaining its
/// constraint cache. Reapply the hints to the new native window on every show.
pub(crate) fn apply_size_limits(window: &AppWindow) {
    if window
        .window()
        .with_winit_window(|native_window| set_native_size_limits(window, native_window))
        .is_some()
    {
        return;
    }
    let window = window.as_weak();
    if let Err(error) = slint::spawn_local(async move {
        let Some(window) = window.upgrade() else {
            return;
        };
        match window.window().winit_window().await {
            Ok(native_window) => set_native_size_limits(&window, &native_window),
            Err(error) => tracing::warn!(%error, "cannot apply the window's size limits"),
        }
    }) {
        tracing::warn!(%error, "cannot await the native window for its size limits");
    }
}

fn set_native_size_limits(window: &AppWindow, native_window: &winit::window::Window) {
    let limits = WindowSizeLimits::from_window(window);
    let mini_player = window.global::<Session>().get_mini_player();
    let minimum = if mini_player {
        limits.mini_player_minimum
    } else {
        limits.full_minimum
    };
    let maximum = mini_player.then_some(limits.mini_player_maximum);
    let scale_factor = f64::from(window.window().scale_factor());
    let physical_size = |size: WindowSize| {
        winit::dpi::LogicalSize::new(size.width, size.height).to_physical::<u32>(scale_factor)
    };
    native_window.set_min_inner_size(Some(physical_size(minimum)));
    native_window.set_max_inner_size(maximum.map(physical_size));
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
    let window = window.window();
    let size = LogicalSize::new(size.width, size.height);
    if window.has_winit_window() {
        window.set_size(size.to_physical(window.scale_factor()));
    } else {
        // Before native creation, winit must choose the monitor's scale factor.
        window.set_size(size);
    }
}

/// Wayland leaves keeping a window above the others to the desktop's window menu, and the window
/// runs natively on Wayland in a Wayland session.
fn always_on_top_supported() -> bool {
    !cfg!(target_os = "linux") || std::env::var_os("WAYLAND_DISPLAY").is_none()
}

#[cfg(test)]
mod tests;
