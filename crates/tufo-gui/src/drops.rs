//! Files and folders dropped on the window.
//!
//! winit reports drops on X11, Windows, and macOS, one event per path, so paths gather until the
//! drop goes quiet and then go to the controller together. winit 0.30 reports no drops on
//! Wayland, so there [`wayland`] listens on winit's own Wayland connection instead.

#[cfg(target_os = "linux")]
mod wayland;

use std::{cell::RefCell, path::PathBuf, rc::Rc, time::Duration};

use slint::{
    Timer, TimerMode,
    winit_030::{
        CustomApplicationHandler, EventResult,
        winit::{
            event::WindowEvent,
            event_loop::ActiveEventLoop,
            window::{Window, WindowId},
        },
    },
};

use crate::controller::Controller;

/// How long after the last dropped path the drop counts as complete.
const DROP_SETTLE_DELAY: Duration = Duration::from_millis(100);

thread_local! {
    /// The controller that receives drops, once the window has one.
    static CONTROLLER: RefCell<Option<Controller>> = const { RefCell::new(None) };
}

/// Sends the window's drops to `controller`.
pub(crate) fn install(controller: Controller) {
    CONTROLLER.with(|slot| *slot.borrow_mut() = Some(controller));
}

/// A drop's progress, from whichever source reported it.
pub(crate) enum DropEvent {
    /// Files are over the window, or no longer are.
    Hovering(bool),
    Dropped(Vec<PathBuf>),
}

/// Hands a drop's progress to the controller. Runs on the UI thread.
fn deliver(event: DropEvent) {
    let controller = CONTROLLER.with(|slot| slot.borrow().clone());
    let Some(controller) = controller else {
        return;
    };
    match event {
        DropEvent::Hovering(hovering) => controller.show_drop_target(hovering),
        DropEvent::Dropped(paths) => {
            controller.show_drop_target(false);
            controller.drop_paths(paths);
        }
    }
}

/// Watches winit for drops. Slint calls it on the UI thread before it handles each event.
#[derive(Default)]
pub(crate) struct DropHandler {
    dropped: Rc<RefCell<Vec<PathBuf>>>,
    settle: Rc<Timer>,
    #[cfg(target_os = "linux")]
    wayland: wayland::WaylandDrops,
}

impl CustomApplicationHandler for DropHandler {
    fn window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        _winit_window: Option<&Window>,
        _slint_window: Option<&slint::Window>,
        event: &WindowEvent,
    ) -> EventResult {
        match event {
            WindowEvent::HoveredFile(_) => deliver(DropEvent::Hovering(true)),
            WindowEvent::HoveredFileCancelled => deliver(DropEvent::Hovering(false)),
            WindowEvent::DroppedFile(path) => {
                deliver(DropEvent::Hovering(false));
                self.dropped.borrow_mut().push(path.clone());
                let dropped = Rc::clone(&self.dropped);
                self.settle
                    .start(TimerMode::SingleShot, DROP_SETTLE_DELAY, move || {
                        let paths = std::mem::take(&mut *dropped.borrow_mut());
                        deliver(DropEvent::Dropped(paths));
                    });
            }
            _ => {}
        }
        EventResult::Propagate
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) -> EventResult {
        #[cfg(target_os = "linux")]
        self.wayland.dispatch(event_loop, deliver);
        #[cfg(not(target_os = "linux"))]
        let _ = event_loop;
        EventResult::Propagate
    }
}
