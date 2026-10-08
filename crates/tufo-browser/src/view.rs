//! What the attached view shows: the selected tab's canvas and the pointer's shape, and the
//! view's geometry. The UI thread paints into it, and the view's stream takes from it.

use std::sync::{Mutex, MutexGuard, PoisonError};

use tokio::sync::watch;
use tufo_wire::browser::{PixelRectangle, PointerShape, ViewOutput, ViewSize};

use crate::canvas::FrameCanvas;

/// Each attached view's number; a newer view replaces an older one.
pub(crate) type ViewerId = u64;

/// The view's size in logical pixels and its scale, as Chromium lays pages out.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ViewGeometry {
    pub(crate) width: i32,
    pub(crate) height: i32,
    pub(crate) scale_factor: f32,
}

impl Default for ViewGeometry {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 720,
            scale_factor: 1.0,
        }
    }
}

impl ViewGeometry {
    /// Whether a paint of `width` × `height` physical pixels has this geometry, allowing for
    /// Chromium rounding fractional scales either way. A tab shown after a resize first paints
    /// once at its old size.
    fn fits_physical(self, width: u32, height: u32) -> bool {
        let fits = |logical: i32, physical: u32| {
            let expected = f64::from(logical) * f64::from(self.scale_factor);
            (f64::from(physical) - expected).abs() <= 1.0
        };
        fits(self.width, width) && fits(self.height, height)
    }
}

impl From<ViewSize> for ViewGeometry {
    fn from(size: ViewSize) -> Self {
        Self {
            width: i32::try_from(size.width().get()).unwrap_or(i32::MAX),
            height: i32::try_from(size.height().get()).unwrap_or(i32::MAX),
            scale_factor: size.scale_factor().into_inner(),
        }
    }
}

/// A browser's CEF identifier.
pub(crate) type BrowserNumber = i32;

#[derive(Debug, thiserror::Error)]
#[error("a newer view replaced this one")]
pub(crate) struct ViewReplaced;

#[derive(Default)]
struct Shared {
    geometry: ViewGeometry,
    viewer: Option<ViewerId>,
    shown_browser: Option<BrowserNumber>,
    canvas: FrameCanvas,
    pointer_shape: Option<PointerShape>,
}

pub(crate) struct ViewHub {
    shared: Mutex<Shared>,
    changes: watch::Sender<u64>,
}

impl Default for ViewHub {
    fn default() -> Self {
        Self {
            shared: Mutex::default(),
            changes: watch::Sender::new(0),
        }
    }
}

impl ViewHub {
    fn lock(&self) -> MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn notify(&self) {
        self.changes.send_modify(|revision| *revision += 1);
    }

    pub(crate) fn geometry(&self) -> ViewGeometry {
        self.lock().geometry
    }

    pub(crate) fn viewer(&self) -> Option<ViewerId> {
        self.lock().viewer
    }

    /// Makes `viewer` the attached view at `size`, ending the previous one.
    pub(crate) fn attach(&self, viewer: ViewerId, size: ViewSize) {
        let mut shared = self.lock();
        shared.viewer = Some(viewer);
        shared.geometry = size.into();
        shared.canvas.clear();
        drop(shared);
        self.notify();
    }

    /// Resizes the view, reporting whether `viewer` is still the attached one.
    pub(crate) fn resize(&self, viewer: ViewerId, size: ViewSize) -> bool {
        let mut shared = self.lock();
        if shared.viewer != Some(viewer) {
            return false;
        }
        shared.geometry = size.into();
        true
    }

    /// Detaches `viewer`, reporting whether it was the attached one.
    pub(crate) fn detach(&self, viewer: ViewerId) -> bool {
        let mut shared = self.lock();
        if shared.viewer != Some(viewer) {
            return false;
        }
        shared.viewer = None;
        shared.canvas.clear();
        drop(shared);
        self.notify();
        true
    }

    /// Shows another browser's paints, which repaint the whole view.
    pub(crate) fn show_browser(&self, browser: Option<BrowserNumber>) {
        let mut shared = self.lock();
        if shared.shown_browser != browser {
            shared.shown_browser = browser;
            shared.canvas.clear();
        }
    }

    pub(crate) fn paint_page(
        &self,
        browser: BrowserNumber,
        width: u32,
        height: u32,
        pixels: &[u8],
        changed: &[PixelRectangle],
    ) {
        let mut shared = self.lock();
        if !shared.shows(browser) || !shared.geometry.fits_physical(width, height) {
            return;
        }
        shared.canvas.paint_page(width, height, pixels, changed);
        drop(shared);
        self.notify();
    }

    /// Places a popup at a rectangle in logical pixels, or hides it with `None`.
    pub(crate) fn place_popup(&self, browser: BrowserNumber, logical: Option<PixelRectangle>) {
        let mut shared = self.lock();
        if !shared.shows(browser) {
            return;
        }
        let scale = f64::from(shared.geometry.scale_factor);
        let physical = logical.map(|rectangle| scaled(rectangle, scale));
        shared.canvas.place_popup(physical);
        drop(shared);
        self.notify();
    }

    pub(crate) fn paint_popup(
        &self,
        browser: BrowserNumber,
        width: u32,
        height: u32,
        pixels: &[u8],
    ) {
        let mut shared = self.lock();
        if !shared.shows(browser) {
            return;
        }
        shared.canvas.paint_popup(width, height, pixels);
        drop(shared);
        self.notify();
    }

    pub(crate) fn set_pointer_shape(&self, browser: BrowserNumber, shape: PointerShape) {
        let mut shared = self.lock();
        if !shared.shows(browser) {
            return;
        }
        shared.pointer_shape = Some(shape);
        drop(shared);
        self.notify();
    }

    pub(crate) fn subscribe(&self) -> watch::Receiver<u64> {
        self.changes.subscribe()
    }

    /// What changed for `viewer` since it last took its output.
    pub(crate) fn take_output(&self, viewer: ViewerId) -> Result<Vec<ViewOutput>, ViewReplaced> {
        let mut shared = self.lock();
        if shared.viewer != Some(viewer) {
            return Err(ViewReplaced);
        }
        let shape = shared.pointer_shape.take().map(ViewOutput::PointerShape);
        let frame = shared.canvas.take_changes().map(ViewOutput::Frame);
        Ok(shape.into_iter().chain(frame).collect())
    }
}

impl Shared {
    fn shows(&self, browser: BrowserNumber) -> bool {
        self.viewer.is_some() && self.shown_browser == Some(browser)
    }
}

fn scaled(rectangle: PixelRectangle, scale: f64) -> PixelRectangle {
    let scale = |logical: u32| {
        // Logical sizes fit the view, which fits a u32 at most 4 times over.
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let physical = (f64::from(logical) * scale).round() as u32;
        physical
    };
    PixelRectangle {
        x: scale(rectangle.x),
        y: scale(rectangle.y),
        width: scale(rectangle.width),
        height: scale(rectangle.height),
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use tufo_wire::browser::ScaleFactor;

    use super::*;

    fn size(width: u32, height: u32) -> ViewSize {
        ViewSize::try_new(
            NonZeroU32::new(width).unwrap(),
            NonZeroU32::new(height).unwrap(),
            ScaleFactor::try_new(2.0).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn only_the_attached_view_receives_the_shown_browsers_paints() {
        let hub = ViewHub::default();
        hub.show_browser(Some(3));
        hub.paint_page(3, 2, 2, &[255; 16], &[]);
        hub.attach(1, size(1, 1));
        hub.paint_page(4, 2, 2, &[255; 16], &[]);
        hub.paint_page(3, 8, 8, &[255; 256], &[]);
        assert_eq!(hub.take_output(1).unwrap().len(), 0);

        hub.paint_page(3, 2, 2, &[255; 16], &[]);
        assert_eq!(hub.take_output(1).unwrap().len(), 1);
        hub.attach(2, size(1, 1));

        assert!(hub.take_output(1).is_err());
        assert_eq!(hub.take_output(2).unwrap().len(), 0);
        assert_eq!(hub.geometry().scale_factor, 2.0);
    }

    #[test]
    fn popups_scale_to_physical_pixels() {
        assert_eq!(
            scaled(
                PixelRectangle {
                    x: 10,
                    y: 3,
                    width: 7,
                    height: 2
                },
                1.5
            ),
            PixelRectangle {
                x: 15,
                y: 5,
                width: 11,
                height: 3
            }
        );
    }
}
