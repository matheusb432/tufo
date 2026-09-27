//! Collection backgrounds, drawn on the host.
//!
//! Slint's software renderer rounds corners only on solid colors, not on gradients or images, so
//! each background is composed here, off the UI thread: the collection's gradient or image, scaled
//! to cover the place it shows, then a card's shade under its title, then anti-aliased rounded
//! corners. Everything is pure pixel arithmetic, sized in physical pixels.

#![expect(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "pixel coordinates and sizes stay far inside f32's exact integer range"
)]

use crate::presentation::Rgb;

/// Straight-alpha RGBA pixels, row by row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Pixels {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) rgba: Vec<u8>,
}

/// An image scaled to cover the card and cropped at the center.
pub(crate) struct Photo<'a> {
    pub(crate) width: u32,
    pub(crate) height: u32,
    /// RGB pixels, row by row.
    pub(crate) rgb: &'a [u8],
}

/// What the card shows.
pub(crate) enum Base<'a> {
    Gradient(Rgb, Rgb),
    Photo(Photo<'a>),
}

/// Whether a card darkens its foot for the title written over it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Shade {
    /// A collection card's, with its title over the foot.
    TitleFoot,
    /// The picture as it is.
    None,
}

/// The title shade's color, and its opacity rising from the foot: `(height from the foot, alpha)`
/// stops, faded linearly between.
const TITLE_SHADE_COLOR: (f32, f32, f32) = (7.0, 21.0, 14.0);
const TITLE_SHADE_STOPS: [(f32, f32); 3] = [(0.0, 0.94), (0.34, 0.6), (0.62, 0.0)];

/// Draws a card of `width` × `height` physical pixels with corners of `radius` pixels.
pub(crate) fn card(base: &Base<'_>, shade: Shade, width: u32, height: u32, radius: f32) -> Pixels {
    let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        for x in 0..width {
            let (red, green, blue) = match base {
                Base::Gradient(start, end) => diagonal_gradient(*start, *end, x, y, width, height),
                Base::Photo(photo) => cover_sample(photo, x, y, width, height),
            };
            let (red, green, blue) = match shade {
                Shade::TitleFoot => title_shade(red, green, blue, y, height),
                Shade::None => (red, green, blue),
            };
            let alpha = corner_coverage(x, y, width, height, radius);
            rgba.extend_from_slice(&[
                to_byte(red),
                to_byte(green),
                to_byte(blue),
                to_byte(alpha * 255.0),
            ]);
        }
    }
    Pixels {
        width,
        height,
        rgba,
    }
}

/// The 135° gradient from `start` at the top left to `end` at the bottom right.
fn diagonal_gradient(
    start: Rgb,
    end: Rgb,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
) -> (f32, f32, f32) {
    let span = (width + height).max(1) as f32;
    let position = (x + y) as f32 / span;
    let mix = |from: u8, to: u8| f32::from(from) + (f32::from(to) - f32::from(from)) * position;
    (
        mix(start.0, end.0),
        mix(start.1, end.1),
        mix(start.2, end.2),
    )
}

/// Samples the photo scaled to cover the card, keeping its center, with bilinear filtering.
fn cover_sample(photo: &Photo<'_>, x: u32, y: u32, width: u32, height: u32) -> (f32, f32, f32) {
    if photo.width == 0 || photo.height == 0 {
        return (0.0, 0.0, 0.0);
    }
    let scale = (width as f32 / photo.width as f32).max(height as f32 / photo.height as f32);
    let offset_x = (photo.width as f32 * scale - width as f32) / 2.0;
    let offset_y = (photo.height as f32 * scale - height as f32) / 2.0;
    let source_x = ((x as f32 + 0.5 + offset_x) / scale - 0.5).clamp(0.0, (photo.width - 1) as f32);
    let source_y =
        ((y as f32 + 0.5 + offset_y) / scale - 0.5).clamp(0.0, (photo.height - 1) as f32);
    let (left, top) = (source_x.floor() as u32, source_y.floor() as u32);
    let (right, bottom) = (
        (left + 1).min(photo.width - 1),
        (top + 1).min(photo.height - 1),
    );
    let (across, down) = (source_x - left as f32, source_y - top as f32);
    let pixel = |column: u32, row: u32| {
        let index = (row as usize * photo.width as usize + column as usize) * 3;
        photo
            .rgb
            .get(index..index + 3)
            .map_or((0.0, 0.0, 0.0), |channels| {
                (
                    f32::from(channels[0]),
                    f32::from(channels[1]),
                    f32::from(channels[2]),
                )
            })
    };
    let lerp = |a: (f32, f32, f32), b: (f32, f32, f32), t: f32| {
        (
            a.0 + (b.0 - a.0) * t,
            a.1 + (b.1 - a.1) * t,
            a.2 + (b.2 - a.2) * t,
        )
    };
    lerp(
        lerp(pixel(left, top), pixel(right, top), across),
        lerp(pixel(left, bottom), pixel(right, bottom), across),
        down,
    )
}

fn title_shade(red: f32, green: f32, blue: f32, y: u32, height: u32) -> (f32, f32, f32) {
    let from_foot = 1.0 - (y as f32 + 0.5) / height.max(1) as f32;
    let alpha = TITLE_SHADE_STOPS
        .windows(2)
        .find(|stops| from_foot <= stops[1].0)
        .map_or(0.0, |stops| {
            let ((low, low_alpha), (high, high_alpha)) = (stops[0], stops[1]);
            let blend = ((from_foot - low) / (high - low)).clamp(0.0, 1.0);
            low_alpha + (high_alpha - low_alpha) * blend
        });
    let (shade_red, shade_green, shade_blue) = TITLE_SHADE_COLOR;
    let over = |channel: f32, shade: f32| channel * (1.0 - alpha) + shade * alpha;
    (
        over(red, shade_red),
        over(green, shade_green),
        over(blue, shade_blue),
    )
}

/// How much of the pixel lies inside the rounded rectangle, from 0 to 1.
fn corner_coverage(x: u32, y: u32, width: u32, height: u32, radius: f32) -> f32 {
    let radius = radius.min(width as f32 / 2.0).min(height as f32 / 2.0);
    let (center_x, center_y) = (x as f32 + 0.5, y as f32 + 0.5);
    let corner_x = if center_x < radius {
        radius
    } else if center_x > width as f32 - radius {
        width as f32 - radius
    } else {
        return 1.0;
    };
    let corner_y = if center_y < radius {
        radius
    } else if center_y > height as f32 - radius {
        height as f32 - radius
    } else {
        return 1.0;
    };
    let distance = ((center_x - corner_x).powi(2) + (center_y - corner_y).powi(2)).sqrt();
    (radius - distance + 0.5).clamp(0.0, 1.0)
}

fn to_byte(value: f32) -> u8 {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the value is clamped to 0..=255 first"
    )]
    let byte = value.round().clamp(0.0, 255.0) as u8;
    byte
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alpha_at(pixels: &Pixels, x: u32, y: u32) -> u8 {
        pixels.rgba[(y as usize * pixels.width as usize + x as usize) * 4 + 3]
    }

    #[test]
    fn corners_are_transparent_and_the_body_is_opaque() {
        let pixels = card(
            &Base::Gradient(Rgb(120, 80, 200), Rgb(20, 30, 60)),
            Shade::TitleFoot,
            64,
            40,
            12.0,
        );

        assert_eq!(pixels.rgba.len(), 64 * 40 * 4);
        for (x, y) in [(0, 0), (63, 0), (0, 39), (63, 39)] {
            assert_eq!(alpha_at(&pixels, x, y), 0, "corner ({x}, {y})");
        }
        assert_eq!(alpha_at(&pixels, 32, 20), 255);
        assert_eq!(alpha_at(&pixels, 32, 0), 255);
    }

    #[test]
    fn the_title_shade_darkens_the_foot_and_leaves_the_top() {
        let grey = Rgb(128, 128, 128);
        let pixels = card(&Base::Gradient(grey, grey), Shade::TitleFoot, 20, 100, 0.0);
        let red_at = |y: u32| pixels.rgba[(y as usize * 20 + 10) * 4];

        assert!(red_at(98) < 30);
        assert_eq!(red_at(2), 128);
    }

    #[test]
    fn a_photo_covers_the_card_from_its_center() {
        // A 4 × 1 photo, black on the outer columns and white in the middle.
        let rgb = [0, 0, 0, 255, 255, 255, 255, 255, 255, 0, 0, 0];
        let photo = Photo {
            width: 4,
            height: 1,
            rgb: &rgb,
        };

        let (red, _, _) = cover_sample(&photo, 0, 0, 2, 2);

        assert!(red > 200.0, "the crop keeps the white center, got {red}");
    }
}
