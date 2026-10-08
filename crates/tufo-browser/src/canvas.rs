//! The selected tab's pixels as a view receives them. Chromium paints the page and an open
//! select element's popup separately, both as BGRA; the canvas lays the popup over the page and
//! hands out the changed rectangle as premultiplied RGBA.

use tufo_wire::browser::{PIXEL_BYTES, PixelRectangle, ViewFrame};

#[derive(Default)]
pub(crate) struct FrameCanvas {
    width: u32,
    height: u32,
    /// The page's latest BGRA pixels.
    page: Vec<u8>,
    popup: Option<Popup>,
    /// Page and popup together, as RGBA.
    composed: Vec<u8>,
    /// Pixels changed since the view last took them.
    changed: Option<PixelRectangle>,
}

struct Popup {
    rectangle: PixelRectangle,
    /// BGRA pixels covering the rectangle, once painted.
    pixels: Vec<u8>,
}

impl FrameCanvas {
    /// Takes a page paint of `width` × `height` BGRA pixels, of which `changed` differ from the
    /// last paint. A new size replaces every pixel.
    pub(crate) fn paint_page(
        &mut self,
        width: u32,
        height: u32,
        pixels: &[u8],
        changed: &[PixelRectangle],
    ) {
        if pixels.len() != pixel_count(width, height) * PIXEL_BYTES {
            return;
        }
        if (width, height) != (self.width, self.height) {
            self.width = width;
            self.height = height;
            self.page = pixels.to_vec();
            self.composed = vec![0; pixels.len()];
            self.compose(self.whole());
            return;
        }
        for rectangle in changed {
            let rectangle = rectangle.clamped_to(width, height);
            copy_rectangle(pixels, width, &mut self.page, width, rectangle, rectangle);
            self.compose(rectangle);
        }
    }

    /// Shows a popup over `rectangle`, in physical pixels, or hides it with `None`.
    pub(crate) fn place_popup(&mut self, rectangle: Option<PixelRectangle>) {
        let previous = self.popup.take().map(|popup| popup.rectangle);
        self.popup = rectangle.map(|rectangle| Popup {
            rectangle,
            pixels: Vec::new(),
        });
        if let Some(previous) = previous {
            self.compose(previous);
        }
    }

    /// Takes the popup's BGRA paint; one of another size than its rectangle is ignored.
    pub(crate) fn paint_popup(&mut self, width: u32, height: u32, pixels: &[u8]) {
        let Some(popup) = &mut self.popup else {
            return;
        };
        if (width, height) != (popup.rectangle.width, popup.rectangle.height)
            || pixels.len() != pixel_count(width, height) * PIXEL_BYTES
        {
            return;
        }
        popup.pixels = pixels.to_vec();
        let rectangle = popup.rectangle;
        self.compose(rectangle);
    }

    /// Forgets the pixels, for a tab that will paint all of them anew.
    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }

    /// The pixels changed since the last call, if any.
    pub(crate) fn take_changes(&mut self) -> Option<ViewFrame> {
        let rectangle = self.changed.take()?;
        let mut pixels = vec![0; pixel_count(rectangle.width, rectangle.height) * PIXEL_BYTES];
        let origin = PixelRectangle {
            x: 0,
            y: 0,
            ..rectangle
        };
        copy_rectangle(
            &self.composed,
            self.width,
            &mut pixels,
            rectangle.width,
            rectangle,
            origin,
        );
        ViewFrame::try_new(self.width, self.height, rectangle, pixels).ok()
    }

    fn whole(&self) -> PixelRectangle {
        PixelRectangle {
            x: 0,
            y: 0,
            width: self.width,
            height: self.height,
        }
    }

    fn mark_changed(&mut self, rectangle: PixelRectangle) {
        if rectangle.is_empty() {
            return;
        }
        self.changed = Some(match self.changed {
            Some(changed) => changed.union(rectangle),
            None => rectangle,
        });
    }

    /// Rebuilds `rectangle` of the composed pixels from the page and the popup.
    fn compose(&mut self, rectangle: PixelRectangle) {
        let rectangle = rectangle.clamped_to(self.width, self.height);
        if rectangle.is_empty() {
            return;
        }
        let stride = self.width as usize * PIXEL_BYTES;
        let popup = self.popup.as_ref().filter(|popup| !popup.pixels.is_empty());
        for y in rectangle.y..rectangle.bottom() {
            let row = y as usize * stride;
            let start = row + rectangle.x as usize * PIXEL_BYTES;
            let end = row + rectangle.right() as usize * PIXEL_BYTES;
            to_rgba(&self.page[start..end], &mut self.composed[start..end]);
            if let Some(popup) = popup {
                overlay_popup_row(popup, y, rectangle, row, &mut self.composed);
            }
        }
        self.mark_changed(rectangle);
    }
}

/// Lays the popup's part of row `y` within `rectangle` over the composed row starting at byte
/// `row`.
fn overlay_popup_row(
    popup: &Popup,
    y: u32,
    rectangle: PixelRectangle,
    row: usize,
    composed: &mut [u8],
) {
    let area = popup.rectangle;
    let left = rectangle.x.max(area.x);
    let right = rectangle.right().min(area.right());
    if !(area.y..area.bottom()).contains(&y) || left >= right {
        return;
    }
    let popup_row = (y - area.y) as usize * area.width as usize * PIXEL_BYTES;
    let from = popup_row + (left - area.x) as usize * PIXEL_BYTES;
    let to = popup_row + (right - area.x) as usize * PIXEL_BYTES;
    let target = row + left as usize * PIXEL_BYTES;
    to_rgba(
        &popup.pixels[from..to],
        &mut composed[target..target + (to - from)],
    );
}

fn to_rgba(bgra: &[u8], rgba: &mut [u8]) {
    let (sources, _) = bgra.as_chunks::<PIXEL_BYTES>();
    let (targets, _) = rgba.as_chunks_mut::<PIXEL_BYTES>();
    for (source, target) in sources.iter().zip(targets) {
        *target = [source[2], source[1], source[0], source[3]];
    }
}

fn pixel_count(width: u32, height: u32) -> usize {
    width as usize * height as usize
}

/// Copies the pixels of `from` in a source `source_width` pixels wide to `to` in a target
/// `target_width` pixels wide; both rectangles have the same size.
fn copy_rectangle(
    source: &[u8],
    source_width: u32,
    target: &mut [u8],
    target_width: u32,
    from: PixelRectangle,
    to: PixelRectangle,
) {
    let row_bytes = from.width as usize * PIXEL_BYTES;
    for row in 0..from.height as usize {
        let source_start =
            ((from.y as usize + row) * source_width as usize + from.x as usize) * PIXEL_BYTES;
        let target_start =
            ((to.y as usize + row) * target_width as usize + to.x as usize) * PIXEL_BYTES;
        target[target_start..target_start + row_bytes]
            .copy_from_slice(&source[source_start..source_start + row_bytes]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLUE_BGRA: [u8; 4] = [255, 0, 0, 255];
    const BLUE_RGBA: [u8; 4] = [0, 0, 255, 255];
    const RED_BGRA: [u8; 4] = [0, 0, 255, 255];
    const RED_RGBA: [u8; 4] = [255, 0, 0, 255];

    fn filled(width: u32, height: u32, pixel: [u8; 4]) -> Vec<u8> {
        pixel.repeat(pixel_count(width, height))
    }

    fn rectangle(x: u32, y: u32, width: u32, height: u32) -> PixelRectangle {
        PixelRectangle {
            x,
            y,
            width,
            height,
        }
    }

    fn pixel(frame: &ViewFrame, x: u32, y: u32) -> [u8; 4] {
        let offset = (y * frame.rectangle().width + x) as usize * PIXEL_BYTES;
        frame.pixels()[offset..offset + PIXEL_BYTES]
            .try_into()
            .unwrap()
    }

    #[test]
    fn a_first_paint_covers_the_frame_as_rgba() {
        let mut canvas = FrameCanvas::default();
        canvas.paint_page(4, 3, &filled(4, 3, BLUE_BGRA), &[rectangle(0, 0, 1, 1)]);

        let frame = canvas.take_changes().unwrap();

        assert!(frame.covers_frame());
        assert_eq!(pixel(&frame, 3, 2), BLUE_RGBA);
        assert!(canvas.take_changes().is_none());
    }

    #[test]
    fn later_paints_send_only_their_changed_rectangles() {
        let mut canvas = FrameCanvas::default();
        canvas.paint_page(4, 3, &filled(4, 3, BLUE_BGRA), &[]);
        canvas.take_changes();
        let mut repainted = filled(4, 3, BLUE_BGRA);
        repainted[(4 + 2) * PIXEL_BYTES..(4 + 3) * PIXEL_BYTES].copy_from_slice(&RED_BGRA);

        canvas.paint_page(4, 3, &repainted, &[rectangle(2, 1, 1, 1)]);
        canvas.paint_page(4, 3, &repainted, &[rectangle(0, 0, 1, 1)]);
        let frame = canvas.take_changes().unwrap();

        assert_eq!(frame.rectangle(), rectangle(0, 0, 3, 2));
        assert_eq!(pixel(&frame, 2, 1), RED_RGBA);
        assert_eq!(pixel(&frame, 0, 0), BLUE_RGBA);
    }

    #[test]
    fn a_popup_covers_the_page_until_it_hides() {
        let mut canvas = FrameCanvas::default();
        canvas.paint_page(4, 4, &filled(4, 4, BLUE_BGRA), &[]);
        canvas.take_changes();

        canvas.place_popup(Some(rectangle(1, 1, 2, 2)));
        canvas.paint_popup(2, 2, &filled(2, 2, RED_BGRA));
        let shown = canvas.take_changes().unwrap();
        canvas.place_popup(None);
        let hidden = canvas.take_changes().unwrap();

        assert_eq!(shown.rectangle(), rectangle(1, 1, 2, 2));
        assert_eq!(pixel(&shown, 1, 1), RED_RGBA);
        assert_eq!(hidden.rectangle(), rectangle(1, 1, 2, 2));
        assert_eq!(pixel(&hidden, 0, 0), BLUE_RGBA);
    }

    #[test]
    fn paints_of_the_wrong_size_are_ignored() {
        let mut canvas = FrameCanvas::default();

        canvas.paint_page(4, 3, &filled(2, 2, BLUE_BGRA), &[]);

        assert!(canvas.take_changes().is_none());
    }
}
