//! Lets the accents on whatever plays move only while the window has focus. The window's clock
//! only runs while something plays, too.

use slint::{
    ComponentHandle as _,
    winit_030::{EventResult, WinitWindowAccessor as _, winit::event::WindowEvent},
};

use crate::ui::{AppWindow, Clock};

pub(crate) fn install(window: &AppWindow) {
    window.global::<Clock>().set_enabled(true);
    let window_handle = window.as_weak();
    window.window().on_winit_window_event(move |_, event| {
        if let WindowEvent::Focused(focused) = event
            && let Some(window) = window_handle.upgrade()
        {
            window.global::<Clock>().set_enabled(*focused);
        }
        EventResult::Propagate
    });
}
