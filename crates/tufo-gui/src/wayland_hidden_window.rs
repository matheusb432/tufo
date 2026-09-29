//! A window that starts hidden on Wayland.
//!
//! Slint opens the native window of every component once its event loop starts, even a hidden
//! one. Wayland can't hide a window, so that one stays on the desktop as an empty frame, and Slint
//! never draws into it once shown.

use slint::{ComponentHandle as _, winit_030::WinitWindowAccessor as _};

use crate::{ui::AppWindow, wayland};

/// Closes the native window Slint opened for the still hidden `window`. Hiding a shown window
/// closes it on Wayland, so the first real show opens a fresh one.
pub(crate) async fn close_native_window(window: slint::Weak<AppWindow>) {
    let Some(window) = window.upgrade() else {
        return;
    };
    let on_wayland = window
        .window()
        .winit_window()
        .await
        .is_ok_and(|winit_window| wayland::is_wayland(&winit_window));
    if on_wayland && !window.window().is_visible() {
        let _ = window.show();
        let _ = window.hide();
    }
}
