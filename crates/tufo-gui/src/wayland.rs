//! Telling a Wayland window from an X11 one: on Linux, winit opens the window on either.

use slint::winit_030::winit::{
    raw_window_handle::{HasWindowHandle as _, RawWindowHandle},
    window::Window,
};

/// Whether `window` is a Wayland surface rather than an X11 window.
pub(crate) fn is_wayland(window: &Window) -> bool {
    window
        .window_handle()
        .is_ok_and(|handle| matches!(handle.as_raw(), RawWindowHandle::Wayland(_)))
}
