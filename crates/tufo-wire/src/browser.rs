//! The process-neutral contract between a view of the browser and the browser host: the view's
//! size and input going in, pixels and the pointer's shape coming out.

use std::num::NonZeroU32;

use nutype::nutype;

/// The widest or tallest view the host paints, in physical pixels: a 5K display. It bounds a
/// frame's memory and message size.
pub const VIEW_PHYSICAL_PIXELS_MAX: u32 = 5120;
const SCALE_FACTOR_MIN: f32 = 0.5;
const SCALE_FACTOR_MAX: f32 = 4.0;

/// Physical pixels per logical pixel.
#[nutype(
    validate(finite, greater_or_equal = SCALE_FACTOR_MIN, less_or_equal = SCALE_FACTOR_MAX),
    derive(Debug, Clone, Copy, PartialEq, PartialOrd)
)]
pub struct ScaleFactor(f32);

/// A view's size in logical pixels and its scale, whose physical size fits
/// [`VIEW_PHYSICAL_PIXELS_MAX`] both ways.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewSize {
    width: NonZeroU32,
    height: NonZeroU32,
    scale_factor: ScaleFactor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the view exceeds {VIEW_PHYSICAL_PIXELS_MAX} physical pixels in a direction")]
pub struct ViewTooLarge;

impl ViewSize {
    pub fn try_new(
        width: NonZeroU32,
        height: NonZeroU32,
        scale_factor: ScaleFactor,
    ) -> Result<Self, ViewTooLarge> {
        let size = Self {
            width,
            height,
            scale_factor,
        };
        let (physical_width, physical_height) = size.physical();
        if physical_width > VIEW_PHYSICAL_PIXELS_MAX || physical_height > VIEW_PHYSICAL_PIXELS_MAX {
            return Err(ViewTooLarge);
        }
        Ok(size)
    }

    #[must_use]
    pub fn width(self) -> NonZeroU32 {
        self.width
    }

    #[must_use]
    pub fn height(self) -> NonZeroU32 {
        self.height
    }

    #[must_use]
    pub fn scale_factor(self) -> ScaleFactor {
        self.scale_factor
    }

    /// The physical width and height, rounded up.
    #[must_use]
    pub fn physical(self) -> (u32, u32) {
        let scale = f64::from(self.scale_factor.into_inner());
        let physical = |logical: NonZeroU32| {
            // Bounded by the logical size, which is a u32, times at most 4.
            #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let pixels = (f64::from(logical.get()) * scale)
                .ceil()
                .min(f64::from(u32::MAX)) as u32;
            pixels
        };
        (physical(self.width), physical(self.height))
    }
}

/// A position in logical pixels from the view's top-left corner, possibly outside it while a
/// button is held.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ViewPoint {
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each modifier key is held independently"
)]
pub struct KeyModifiers {
    pub shift: bool,
    pub control: bool,
    pub alt: bool,
    pub meta: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
}

/// A key going down or up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyInput {
    /// The DOM keyCode, which matches the Windows virtual-key code.
    pub key_code: u32,
    /// What the key types, empty for keys that type nothing.
    pub text: String,
    pub pressed: bool,
    pub repeat: bool,
    pub modifiers: KeyModifiers,
}

/// What a view sends the browser.
#[derive(Debug, Clone, PartialEq)]
pub enum ViewInput {
    Resize(ViewSize),
    PointerMoved {
        position: ViewPoint,
        modifiers: KeyModifiers,
    },
    PointerLeft,
    PointerButton {
        position: ViewPoint,
        button: MouseButton,
        pressed: bool,
        /// 1 for a single click, 2 for the second press of a double click.
        click_count: u8,
        modifiers: KeyModifiers,
    },
    /// Logical pixels to scroll; positive values move the content right and down.
    PointerWheel {
        position: ViewPoint,
        delta_x: f32,
        delta_y: f32,
        modifiers: KeyModifiers,
    },
    Key(KeyInput),
    Focus(bool),
    /// Text typed into the focused field at once, such as pasted text.
    InsertText(String),
}

/// A rectangle in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixelRectangle {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl PixelRectangle {
    #[must_use]
    pub fn right(self) -> u32 {
        self.x.saturating_add(self.width)
    }

    #[must_use]
    pub fn bottom(self) -> u32 {
        self.y.saturating_add(self.height)
    }

    /// The smallest rectangle holding both.
    #[must_use]
    pub fn union(self, other: Self) -> Self {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        Self {
            x,
            y,
            width: self.right().max(other.right()) - x,
            height: self.bottom().max(other.bottom()) - y,
        }
    }

    /// The part inside a `width` × `height` frame.
    #[must_use]
    pub fn clamped_to(self, width: u32, height: u32) -> Self {
        let x = self.x.min(width);
        let y = self.y.min(height);
        Self {
            x,
            y,
            width: self.right().min(width) - x,
            height: self.bottom().min(height) - y,
        }
    }

    #[must_use]
    pub fn is_empty(self) -> bool {
        self.width == 0 || self.height == 0
    }
}

/// New pixels in a frame: premultiplied RGBA covering `rectangle`, row by row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewFrame {
    width: u32,
    height: u32,
    rectangle: PixelRectangle,
    pixels: Vec<u8>,
}

/// RGBA bytes per pixel.
pub const PIXEL_BYTES: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ViewFrameError {
    #[error("the frame exceeds the largest view")]
    TooLarge,
    #[error("the changed rectangle leaves the frame")]
    RectangleOutside,
    #[error("the pixels do not cover the changed rectangle")]
    PixelCount,
}

impl ViewFrame {
    pub fn try_new(
        width: u32,
        height: u32,
        rectangle: PixelRectangle,
        pixels: Vec<u8>,
    ) -> Result<Self, ViewFrameError> {
        if width > VIEW_PHYSICAL_PIXELS_MAX || height > VIEW_PHYSICAL_PIXELS_MAX {
            return Err(ViewFrameError::TooLarge);
        }
        if rectangle.right() > width || rectangle.bottom() > height {
            return Err(ViewFrameError::RectangleOutside);
        }
        let expected = rectangle.width as usize * rectangle.height as usize * PIXEL_BYTES;
        if pixels.len() != expected {
            return Err(ViewFrameError::PixelCount);
        }
        Ok(Self {
            width,
            height,
            rectangle,
            pixels,
        })
    }

    #[must_use]
    pub fn width(&self) -> u32 {
        self.width
    }

    #[must_use]
    pub fn height(&self) -> u32 {
        self.height
    }

    #[must_use]
    pub fn rectangle(&self) -> PixelRectangle {
        self.rectangle
    }

    #[must_use]
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    #[must_use]
    pub fn into_pixels(self) -> Vec<u8> {
        self.pixels
    }

    #[must_use]
    pub fn covers_frame(&self) -> bool {
        self.rectangle.x == 0
            && self.rectangle.y == 0
            && self.rectangle.width == self.width
            && self.rectangle.height == self.height
    }
}

/// The pointer's shape over the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PointerShape {
    #[default]
    Default,
    Hand,
    Text,
    Wait,
    Progress,
    Crosshair,
    Move,
    NotAllowed,
    Grab,
    Grabbing,
    Help,
    ColumnResize,
    RowResize,
    NeswResize,
    NwseResize,
    None,
}

/// What the browser sends a view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewOutput {
    Frame(ViewFrame),
    PointerShape(PointerShape),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn size(width: u32, height: u32, scale: f32) -> Result<ViewSize, ViewTooLarge> {
        ViewSize::try_new(
            NonZeroU32::new(width).unwrap(),
            NonZeroU32::new(height).unwrap(),
            ScaleFactor::try_new(scale).unwrap(),
        )
    }

    #[test]
    fn views_round_physical_sizes_up_and_bound_them() {
        assert_eq!(size(801, 600, 1.25).unwrap().physical(), (1002, 750));
        assert_eq!(size(2560, 100, 2.0).unwrap().physical(), (5120, 200));
        assert_eq!(size(2561, 100, 2.0), Err(ViewTooLarge));
    }

    #[test]
    fn frames_hold_exactly_their_rectangle() {
        let rectangle = PixelRectangle {
            x: 2,
            y: 1,
            width: 3,
            height: 2,
        };
        assert!(ViewFrame::try_new(5, 3, rectangle, vec![0; 3 * 2 * PIXEL_BYTES]).is_ok());
        assert_eq!(
            ViewFrame::try_new(5, 3, rectangle, vec![0; 5]),
            Err(ViewFrameError::PixelCount)
        );
        assert_eq!(
            ViewFrame::try_new(4, 3, rectangle, vec![0; 3 * 2 * PIXEL_BYTES]),
            Err(ViewFrameError::RectangleOutside)
        );
    }

    #[test]
    fn rectangles_unite_and_clamp() {
        let left = PixelRectangle {
            x: 0,
            y: 5,
            width: 2,
            height: 2,
        };
        let right = PixelRectangle {
            x: 8,
            y: 1,
            width: 4,
            height: 1,
        };
        assert_eq!(
            left.union(right),
            PixelRectangle {
                x: 0,
                y: 1,
                width: 12,
                height: 6
            }
        );
        assert_eq!(
            left.union(right).clamped_to(10, 4),
            PixelRectangle {
                x: 0,
                y: 1,
                width: 10,
                height: 3
            }
        );
    }
}
