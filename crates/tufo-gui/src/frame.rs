//! The window's own frame: the desktop draws none, so the window's buttons and title row reach
//! the window here.

#[cfg(target_os = "linux")]
use std::cell::RefCell;
use std::time::Duration;

use slint::{
    ComponentHandle as _, LogicalPosition, Timer,
    platform::{PointerEventButton, WindowEvent},
    winit_030::{WinitWindowAccessor as _, winit},
};

use crate::ui::{AppWindow, WindowFrame};

#[cfg(target_os = "linux")]
thread_local! {
    /// The window whose corners follow the native window's state.
    static WINDOW: RefCell<Option<slint::Weak<AppWindow>>> = const { RefCell::new(None) };
}

/// Keeps the shadow and rounded corners that Windows drops from a window without decorations.
#[cfg(windows)]
pub(crate) fn with_shadow(
    attributes: winit::window::WindowAttributes,
) -> winit::window::WindowAttributes {
    use winit::platform::windows::WindowAttributesExtWindows as _;
    attributes.with_undecorated_shadow(true)
}

pub(crate) fn install(window: &AppWindow) {
    #[cfg(target_os = "linux")]
    WINDOW.with(|slot| *slot.borrow_mut() = Some(window.as_weak()));
    let frame = window.global::<WindowFrame>();
    let window = window.as_weak();
    frame.on_minimize({
        let window = window.clone();
        move || {
            if let Some(window) = window.upgrade() {
                minimize(window.window());
            }
        }
    });
    frame.on_toggle_maximized({
        let window = window.clone();
        move || {
            if let Some(window) = window.upgrade() {
                let window = window.window();
                window.set_maximized(!window.is_maximized());
            }
        }
    });
    frame.on_show_menu(move |x, y| show_menu(&window, LogicalPosition::new(x, y)));
}

/// Rounds the window's corners on Wayland while it doesn't fill the screen, letting the desktop
/// show through them. Slint calls it on the UI thread before it handles each event.
///
/// Wayland surfaces are opaque unless winit marks them transparent, which also costs the
/// compositor the blending of the whole window, so a window that fills the screen stays opaque.
#[cfg(target_os = "linux")]
pub(crate) fn window_event(
    winit_window: &winit::window::Window,
    event: &winit::event::WindowEvent,
) {
    if !matches!(event, winit::event::WindowEvent::Resized(_))
        || !crate::wayland::is_wayland(winit_window)
    {
        return;
    }
    let corners_rounded = !winit_window.is_maximized() && winit_window.fullscreen().is_none();
    winit_window.set_transparent(corners_rounded);
    WINDOW.with(|slot| {
        if let Some(window) = slot.borrow().as_ref().and_then(slint::Weak::upgrade) {
            window
                .global::<WindowFrame>()
                .set_corners_rounded(corners_rounded);
        }
    });
}

#[cfg(not(target_os = "linux"))]
fn minimize(window: &slint::Window) {
    window.set_minimized(true);
}

/// Minimizes `window` through winit on Wayland, and through Slint on X11.
///
/// Wayland never tells a client that its minimized window was restored, so Slint's `minimized`
/// property would stay set there and ignore every later minimize.
#[cfg(target_os = "linux")]
fn minimize(window: &slint::Window) {
    if window
        .with_winit_window(crate::wayland::is_wayland)
        .unwrap_or(false)
    {
        window.with_winit_window(|winit_window| winit_window.set_minimized(true));
    } else {
        window.set_minimized(true);
    }
}

/// Opens the desktop's window menu at `position` in the window.
///
/// The menu takes the pointer while the right button that opened it is still down, so the window
/// never sees that button come up and would hand its next click to the title row. A release sent
/// once the menu is open ends that press.
fn show_menu(window: &slint::Weak<AppWindow>, position: LogicalPosition) {
    let Some(app_window) = window.upgrade() else {
        return;
    };
    app_window.window().with_winit_window(|winit_window| {
        winit_window.show_window_menu(winit::dpi::LogicalPosition::new(
            f64::from(position.x),
            f64::from(position.y),
        ));
    });
    let window = window.clone();
    Timer::single_shot(Duration::ZERO, move || {
        if let Some(window) = window.upgrade() {
            window
                .window()
                .dispatch_event(WindowEvent::PointerReleased {
                    position,
                    button: PointerEventButton::Right,
                });
        }
    });
}
