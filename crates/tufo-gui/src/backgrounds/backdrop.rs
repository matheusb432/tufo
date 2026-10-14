//! The felt's static radial gradient and the rim's recess, rasterized by the background worker.
//! One bitmap stays available during resizing; one job draws only the newest requested size.

use std::cell::RefCell;

use slint::{Color, ComponentHandle as _, Image, Rgba8Pixel, SharedPixelBuffer};

use super::Job;
use crate::{card_art, presentation::Rgb, ui::Art};

const SIDE_MAX: u32 = 2048;

#[derive(Clone, PartialEq)]
pub(super) struct Key {
    width: u32,
    height: u32,
    corner_radius: u32,
    recess_depth: u32,
    colors: [Color; 3],
}

impl Key {
    fn new(
        width: u32,
        height: u32,
        corner_radius: u32,
        recess_depth: u32,
        colors: [Color; 3],
    ) -> Self {
        let side = width.max(height).max(SIDE_MAX);
        Self {
            width: (width * SIDE_MAX / side).max(1),
            height: (height * SIDE_MAX / side).max(1),
            corner_radius: corner_radius * SIDE_MAX / side,
            recess_depth: recess_depth * SIDE_MAX / side,
            colors,
        }
    }
}

#[derive(Default)]
struct Cache {
    available: Option<(Key, Image)>,
    wanted: Option<Key>,
    drawing: bool,
}

impl Cache {
    fn next(&mut self) -> Option<Key> {
        if self.drawing || self.available.as_ref().map(|(key, _)| key) == self.wanted.as_ref() {
            return None;
        }
        let next = self.wanted.clone()?;
        self.drawing = true;
        Some(next)
    }
}

thread_local! {
    static CACHE: RefCell<Cache> = RefCell::default();
}

pub(crate) fn image(
    width: u32,
    height: u32,
    corner_radius: u32,
    recess_depth: u32,
    colors: [Color; 3],
) -> Image {
    let (image, next) = CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        cache.wanted = Some(Key::new(width, height, corner_radius, recess_depth, colors));
        let next = cache.next();
        let image = cache
            .available
            .as_ref()
            .map(|(_, image)| image.clone())
            .unwrap_or_default();
        (image, next)
    });
    if let Some(key) = next {
        super::send(Job::DrawBackdrop(key));
    }
    image
}

pub(super) fn draw(key: &Key) -> SharedPixelBuffer<Rgba8Pixel> {
    let colors = key
        .colors
        .map(|color| Rgb(color.red(), color.green(), color.blue()));
    #[expect(
        clippy::cast_precision_loss,
        reason = "a corner radius and a recess stay far inside f32's exact integer range"
    )]
    let (corner_radius, recess) = (
        key.corner_radius as f32,
        card_art::Recess {
            depth: key.recess_depth as f32,
        },
    );
    let pixels = card_art::backdrop(colors, key.width, key.height, corner_radius, recess);
    SharedPixelBuffer::clone_from_slice(&pixels.rgba, pixels.width, pixels.height)
}

pub(super) fn drawn(key: Key, pixels: SharedPixelBuffer<Rgba8Pixel>) {
    let (changed, next) = CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        cache.drawing = false;
        let changed = cache.wanted.as_ref() == Some(&key);
        if changed {
            cache.available = Some((key, Image::from_rgba8(pixels)));
        }
        (changed, cache.next())
    });
    if let Some(key) = next {
        super::send(Job::DrawBackdrop(key));
    }
    if changed {
        super::with_window(|window| {
            let art = window.global::<Art>();
            art.set_backdrop_revision(art.get_backdrop_revision().wrapping_add(1));
        });
    }
}

#[cfg(feature = "benchmark")]
pub(super) fn is_idle() -> bool {
    CACHE.with(|cache| !cache.borrow().drawing)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_large_backdrop_stays_below_sixteen_mebibytes_and_keeps_its_aspect() {
        let key = Key::new(6000, 3000, 24, 66, [Color::default(); 3]);
        assert_eq!(
            (key.width, key.height, key.corner_radius, key.recess_depth),
            (2048, 1024, 8, 22)
        );
        assert!(key.width * key.height * 4 <= 16 * 1024 * 1024);
    }

    #[test]
    fn rapid_resize_only_draws_the_newest_requested_size_after_the_current_job() {
        let mut cache = Cache {
            wanted: Some(Key::new(1000, 700, 0, 0, [Color::default(); 3])),
            ..Cache::default()
        };
        assert!(cache.next().is_some());
        cache.wanted = Some(Key::new(1100, 700, 0, 0, [Color::default(); 3]));
        assert!(cache.next().is_none());
        cache.wanted = Some(Key::new(1200, 800, 0, 0, [Color::default(); 3]));
        cache.drawing = false;
        let next = cache.next().unwrap();
        assert_eq!((next.width, next.height), (1200, 800));
    }
}
