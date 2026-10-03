//! The pictures behind collections: the preset paintings Tufo ships and the image files
//! collections point at.
//!
//! A worker thread decodes pictures and draws them at the size and corner radius each place
//! shows them, so the window never waits on a picture. Until a picture is ready, the window shows
//! the same picture drawn at another size, or the collection's gradient, and then draws again when
//! the worker delivers. Pictures decode at most [`PICTURE_SIDE_MAX`] pixels on a side, the size of
//! the presets' small renditions. Every cache drops its least recently used entries past its byte
//! budget.

use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet, hash_map::RandomState},
    num::NonZeroUsize,
    path::{Path, PathBuf},
    sync::mpsc,
    time::Duration,
};

use clru::{CLruCache, CLruCacheConfig, WeightScale};
use image::{DynamicImage, ImageFormat, ImageReader, imageops::FilterType};
use slint::{ComponentHandle as _, Image, PhysicalSize, Rgb8Pixel, Rgba8Pixel, SharedPixelBuffer};
use tufo_client::library::Background;
use tufo_models::library::{BackgroundPreset, CollectionId};

use crate::{
    card_art::{self, Shade},
    presentation::{self, Rgb},
    ui::{AppWindow, Art},
};

pub(crate) mod backdrop;

#[cfg(feature = "benchmark")]
pub(crate) mod metrics;

/// The longest side of an image file decoded, in pixels: sharp on the largest plate at twice the
/// window's scale. The presets' small renditions match it.
const PICTURE_SIDE_MAX: u32 = 800;
const MEBIBYTE: usize = 1024 * 1024;
/// About a dozen small renditions or image files; the worker decodes any other again.
const CARD_PICTURES_BYTES_MAX: usize = 16 * MEBIBYTE;
const CARD_ART_BYTES_MIN: usize = 24 * MEBIBYTE;
const CARD_ART_BYTES_MAX: usize = 256 * MEBIBYTE;
/// Tufo's own gradient, for a card without a collection yet.
const DEFAULT_GRADIENT: (Rgb, Rgb) = (Rgb(45, 106, 79), Rgb(1, 50, 32));

/// A picture a background decodes from.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum Picture {
    Preset(BackgroundPreset),
    File(PathBuf),
}

impl Picture {
    /// The picture `background` shows, or `None` for a gradient.
    pub(crate) fn of(background: &Background) -> Option<Self> {
        match background {
            Background::Gradient => None,
            Background::Preset(preset) => Some(Self::Preset(*preset)),
            Background::Image(path) => Some(Self::File(path.as_ref().clone())),
        }
    }
}

/// How a card looks, apart from its size.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CardLook {
    picture: Option<Picture>,
    /// The collection's gradient, which shows without a picture or until it's ready.
    gradient: (Rgb, Rgb),
    shade: Shade,
    radius: u32,
}

/// A card at a size in physical pixels.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CardKey {
    look: CardLook,
    width: u32,
    height: u32,
}

enum Job {
    DrawCard(CardKey),
    DrawBackdrop(backdrop::Key),
    /// Forget the pictures that failed to decode, so they're tried again.
    ForgetFailures,
}

/// A cached value's size in bytes, which the caches' capacities budget.
struct ByteWeight;

impl<K> WeightScale<K, Option<SharedPixelBuffer<Rgb8Pixel>>> for ByteWeight {
    fn weight(&self, _: &K, pixels: &Option<SharedPixelBuffer<Rgb8Pixel>>) -> usize {
        pixels.as_ref().map_or(0, |pixels| pixels.as_bytes().len())
    }
}

impl WeightScale<CardKey, Image> for ByteWeight {
    fn weight(&self, _: &CardKey, art: &Image) -> usize {
        let size = art.size();
        size.width as usize * size.height as usize * 4
    }
}

type ByteBudgetCache<K, V> = CLruCache<K, V, RandomState, ByteWeight>;

fn byte_budget_cache<K, V>(bytes_max: usize) -> ByteBudgetCache<K, V>
where
    K: Eq + std::hash::Hash,
    ByteWeight: WeightScale<K, V>,
{
    let capacity = NonZeroUsize::new(bytes_max).unwrap_or(NonZeroUsize::MIN);
    CLruCache::with_config(CLruCacheConfig::new(capacity).with_scale(ByteWeight))
}

// Everything below lives on the UI thread.
thread_local! {
    static WINDOW: RefCell<Option<slint::Weak<AppWindow>>> = const { RefCell::new(None) };
    static JOBS: RefCell<Option<mpsc::Sender<Job>>> = const { RefCell::new(None) };
    static CARDS: RefCell<ByteBudgetCache<CardKey, Image>> =
        RefCell::new(byte_budget_cache(CARD_ART_BYTES_MIN));
    /// The looks the worker is drawing, with the size the window wants each at now.
    static DRAWING: RefCell<HashMap<CardLook, (u32, u32)>> = RefCell::new(HashMap::new());
    /// Pictures that failed to decode, whose cards show the gradient.
    static MISSING: RefCell<HashSet<Picture>> = RefCell::new(HashSet::new());
    static LAYOUT_MOVING: Cell<bool> = const { Cell::new(false) };
    static REDRAW: slint::Timer = slint::Timer::default();
}

/// Reuses existing artwork while the sidebar slides, then refreshes its settled dimensions.
pub(crate) fn set_layout_moving(moving: bool) {
    let was_moving = LAYOUT_MOVING.with(|state| state.replace(moving));
    if was_moving && !moving {
        invalidate_cards();
    }
}

fn invalidate_cards() {
    if LAYOUT_MOVING.with(Cell::get) {
        return;
    }
    with_window(|window| {
        let art = window.global::<Art>();
        art.set_pictures_revision(art.get_pictures_revision().wrapping_add(1));
    });
}

/// Coalesces completions delivered together into one binding refresh.
fn redraw_cards() {
    if LAYOUT_MOVING.with(Cell::get) {
        return;
    }
    REDRAW.with(|timer| {
        if !timer.running() {
            timer.start(
                slint::TimerMode::SingleShot,
                Duration::from_millis(1),
                invalidate_cards,
            );
        }
    });
}

/// Starts the worker that decodes pictures for `window`.
pub(crate) fn install(window: &AppWindow) {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    map_large_buffers_separately();
    let (jobs, received) = mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("backgrounds".to_owned())
        .spawn(move || work(&received));
    if let Err(error) = spawned {
        tracing::error!(%error, "cannot start the background worker, so cards show gradients");
        return;
    }
    WINDOW.with(|slot| *slot.borrow_mut() = Some(window.as_weak()));
    JOBS.with(|slot| *slot.borrow_mut() = Some(jobs));
}

/// glibc raises its mmap threshold to the size of each large buffer freed, so later pictures that
/// size come from the heap, which it doesn't return to the system: every collection page opened
/// would keep its picture's memory. A fixed threshold keeps each picture in its own mapping, which
/// dropping the picture releases.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn map_large_buffers_separately() {
    const MAPPED_BUFFER_BYTES_MIN: libc::c_int = 1 << 20;
    // SAFETY: mallopt only tunes the allocator, and every allocator call stays valid under it.
    unsafe {
        libc::mallopt(libc::M_MMAP_THRESHOLD, MAPPED_BUFFER_BYTES_MIN);
    }
}

/// A background at `width` × `height` physical pixels with corners of `radius` pixels: its
/// picture, or the gradient of the collection `id`, or Tufo's own gradient without one.
pub(crate) fn card(
    id: Option<CollectionId>,
    background: &Background,
    shade: Shade,
    width: u32,
    height: u32,
    radius: u32,
) -> Image {
    #[cfg(feature = "benchmark")]
    metrics::requested();
    let key = CardKey {
        look: CardLook {
            picture: Picture::of(background),
            gradient: id.map_or(DEFAULT_GRADIENT, presentation::gradient),
            shade,
            radius,
        },
        width,
        height,
    };
    if let Some(art) = CARDS.with(|cards| {
        let mut cards = cards.borrow_mut();
        if let Some(size) = WINDOW.with(|window| {
            window
                .borrow()
                .as_ref()
                .and_then(slint::Weak::upgrade)
                .map(|window| window.window().size())
        }) {
            let budget = card_cache_budget(size);
            if cards.capacity() != budget.get() {
                cards.resize(budget);
            }
        }
        cards.get(&key).cloned()
    }) {
        return art;
    }
    #[cfg(feature = "benchmark")]
    metrics::missed();
    let cached = same_look_at_any_size(&key.look);
    if LAYOUT_MOVING.with(Cell::get)
        && let Some(art) = &cached
    {
        return art.clone();
    }
    let missing = key
        .look
        .picture
        .as_ref()
        .is_none_or(|picture| MISSING.with(|missing| missing.borrow().contains(picture)));
    if missing {
        let art = gradient_card(&key);
        CARDS.with(|cards| {
            let _ = cards.borrow_mut().put_with_weight(key, art.clone());
        });
        return art;
    }
    let placeholder = cached.unwrap_or_else(|| gradient_card(&key));
    request_card(key);
    placeholder
}

fn card_cache_budget(size: PhysicalSize) -> NonZeroUsize {
    // Keep two framebuffers of artwork: a fixed small cache continually evicts large visible cards.
    let pixels = u64::from(size.width) * u64::from(size.height);
    let bytes = pixels
        .saturating_mul(8)
        .clamp(CARD_ART_BYTES_MIN as u64, CARD_ART_BYTES_MAX as u64);
    NonZeroUsize::new(usize::try_from(bytes).unwrap_or(CARD_ART_BYTES_MAX))
        .unwrap_or(NonZeroUsize::MIN)
}

/// Whether the background's image file failed to decode, so its gradient shows instead.
pub(crate) fn picture_missing(background: &Background) -> bool {
    Picture::of(background)
        .is_some_and(|picture| MISSING.with(|missing| missing.borrow().contains(&picture)))
}

/// Forgets the pictures that failed to decode and the cards drawn without them, so a file that
/// came back shows again. Returns whether it forgot any.
pub(crate) fn retry_missing_pictures() -> bool {
    let missing = MISSING.with(|missing| std::mem::take(&mut *missing.borrow_mut()));
    if missing.is_empty() {
        return false;
    }
    CARDS.with(|cards| {
        cards.borrow_mut().retain(|key, _| {
            key.look
                .picture
                .as_ref()
                .is_none_or(|picture| !missing.contains(picture))
        });
    });
    send(Job::ForgetFailures);
    true
}

fn request_card(key: CardKey) {
    let size = (key.width, key.height);
    let already_drawing = DRAWING.with(|drawing| {
        drawing
            .borrow_mut()
            .insert(key.look.clone(), size)
            .is_some()
    });
    if !already_drawing {
        send(Job::DrawCard(key));
    }
}

/// Keeps a card the worker drew, draws the size the window wants now if that changed while it
/// worked, and has the window draw its cards again.
fn card_drawn(key: CardKey, pixels: Option<SharedPixelBuffer<Rgba8Pixel>>) {
    #[cfg(feature = "benchmark")]
    metrics::completed();
    let wanted = DRAWING.with(|drawing| drawing.borrow_mut().remove(&key.look));
    match pixels {
        Some(pixels) => {
            CARDS.with(|cards| {
                let _ = cards
                    .borrow_mut()
                    .put_with_weight(key.clone(), Image::from_rgba8(pixels));
            });
            if let Some((width, height)) = wanted
                && (width, height) != (key.width, key.height)
                && !LAYOUT_MOVING.with(Cell::get)
            {
                request_card(CardKey {
                    look: key.look,
                    width,
                    height,
                });
            }
        }
        None => {
            if let Some(picture) = key.look.picture {
                MISSING.with(|missing| missing.borrow_mut().insert(picture));
            }
        }
    }
    redraw_cards();
}

fn same_look_at_any_size(look: &CardLook) -> Option<Image> {
    CARDS.with(|cards| {
        cards
            .borrow()
            .iter()
            .find(|(key, _)| key.look == *look)
            .map(|(_, art)| art.clone())
    })
}

fn gradient_card(key: &CardKey) -> Image {
    let (start, end) = key.look.gradient;
    let drawn = draw(
        &card_art::Base::Gradient(start, end),
        &key.look,
        key.width,
        key.height,
    );
    Image::from_rgba8(drawn)
}

fn draw(
    base: &card_art::Base<'_>,
    look: &CardLook,
    width: u32,
    height: u32,
) -> SharedPixelBuffer<Rgba8Pixel> {
    #[cfg(feature = "benchmark")]
    metrics::drawn(width, height);
    #[expect(
        clippy::cast_precision_loss,
        reason = "corner radii are a few dozen pixels"
    )]
    let drawn = card_art::card(base, look.shade, width, height, look.radius as f32);
    SharedPixelBuffer::clone_from_slice(&drawn.rgba, drawn.width, drawn.height)
}

#[cfg(feature = "benchmark")]
pub(crate) fn is_idle() -> bool {
    DRAWING.with(|drawing| drawing.borrow().is_empty())
        && REDRAW.with(|timer| !timer.running())
        && backdrop::is_idle()
}

fn send(job: Job) {
    JOBS.with(|jobs| {
        if let Some(jobs) = jobs.borrow().as_ref() {
            let _ = jobs.send(job);
        }
    });
}

fn with_window(action: impl FnOnce(&AppWindow)) {
    let window = WINDOW.with(|slot| slot.borrow().as_ref().and_then(slint::Weak::upgrade));
    if let Some(window) = window {
        action(&window);
    }
}

/// The worker: decodes pictures and draws cards until the window drops its sender.
fn work(jobs: &mpsc::Receiver<Job>) {
    let mut pictures = byte_budget_cache(CARD_PICTURES_BYTES_MAX);
    for job in jobs {
        let delivered = match job {
            Job::DrawCard(key) => {
                let pixels = draw_card(&mut pictures, &key);
                slint::invoke_from_event_loop(move || card_drawn(key, pixels))
            }
            Job::ForgetFailures => {
                pictures.retain(|_, pixels| pixels.is_some());
                Ok(())
            }
            Job::DrawBackdrop(key) => {
                let pixels = backdrop::draw(&key);
                slint::invoke_from_event_loop(move || backdrop::drawn(key, pixels))
            }
        };
        if delivered.is_err() {
            // The event loop ended, so the window is gone.
            return;
        }
    }
}

/// Draws a card with its picture, or `None` when the picture can't be decoded.
fn draw_card(
    pictures: &mut ByteBudgetCache<Picture, Option<SharedPixelBuffer<Rgb8Pixel>>>,
    key: &CardKey,
) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
    let picture = key.look.picture.as_ref()?;
    let pixels = if let Some(pixels) = pictures.get(picture) {
        pixels.clone()
    } else {
        let pixels = decode(picture);
        let _ = pictures.put_with_weight(picture.clone(), pixels.clone());
        pixels
    }?;
    let photo = card_art::Base::Photo(card_art::Photo {
        width: pixels.width(),
        height: pixels.height(),
        rgb: pixels.as_bytes(),
    });
    Some(draw(&photo, &key.look, key.width, key.height))
}

/// Decodes `picture`, at most [`PICTURE_SIDE_MAX`] pixels on a side.
fn decode(picture: &Picture) -> Option<SharedPixelBuffer<Rgb8Pixel>> {
    let decoded = match picture {
        Picture::Preset(preset) => {
            image::load_from_memory_with_format(preset_webp(*preset), ImageFormat::WebP)
        }
        Picture::File(path) => decode_file(path),
    };
    let decoded = match decoded {
        Ok(decoded) => decoded,
        Err(error) => {
            tracing::warn!(?picture, %error, "cannot decode a background picture");
            return None;
        }
    };
    let fitted = if decoded.width().max(decoded.height()) > PICTURE_SIDE_MAX {
        decoded.resize(PICTURE_SIDE_MAX, PICTURE_SIDE_MAX, FilterType::Triangle)
    } else {
        decoded
    };
    let rgb = fitted.into_rgb8();
    Some(SharedPixelBuffer::clone_from_slice(
        rgb.as_raw(),
        rgb.width(),
        rgb.height(),
    ))
}

fn decode_file(path: &Path) -> image::ImageResult<DynamicImage> {
    ImageReader::open(path)?.with_guessed_format()?.decode()
}

/// The preset's small WebP rendition; `assets/backgrounds/CREDITS.md` credits each.
fn preset_webp(preset: BackgroundPreset) -> &'static [u8] {
    macro_rules! rendition {
        ($name:literal) => {
            include_bytes!(concat!(
                "../../../assets/backgrounds/small/",
                $name,
                ".webp"
            ))
            .as_slice()
        };
    }
    match preset {
        BackgroundPreset::Autumn => rendition!("autumn"),
        BackgroundPreset::Battle => rendition!("battle"),
        BackgroundPreset::Castle => rendition!("castle"),
        BackgroundPreset::Cathedral => rendition!("cathedral"),
        BackgroundPreset::Coast => rendition!("coast"),
        BackgroundPreset::Desert => rendition!("desert"),
        BackgroundPreset::Dragon => rendition!("dragon"),
        BackgroundPreset::Fire => rendition!("fire"),
        BackgroundPreset::Forest => rendition!("forest"),
        BackgroundPreset::Haunted => rendition!("haunted"),
        BackgroundPreset::Mountain => rendition!("mountain"),
        BackgroundPreset::Night => rendition!("night"),
        BackgroundPreset::Sky => rendition!("sky"),
        BackgroundPreset::Skulls => rendition!("skulls"),
        BackgroundPreset::Storm => rendition!("storm"),
        BackgroundPreset::Tavern => rendition!("tavern"),
        BackgroundPreset::Town => rendition!("town"),
        BackgroundPreset::Underworld => rendition!("underworld"),
        BackgroundPreset::Volcano => rendition!("volcano"),
        BackgroundPreset::Winter => rendition!("winter"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn card_cache_budget_follows_physical_size_with_bounded_memory() {
        for (width, height, bytes) in [
            (0, 0, CARD_ART_BYTES_MIN),
            (1000, 700, CARD_ART_BYTES_MIN),
            (3840, 2160, 3840 * 2160 * 8),
            (7680, 4320, 7680 * 4320 * 8),
            (u32::MAX, u32::MAX, CARD_ART_BYTES_MAX),
        ] {
            assert_eq!(
                card_cache_budget(PhysicalSize::new(width, height)).get(),
                bytes
            );
        }
    }

    fn card_key(picture: Option<Picture>) -> CardKey {
        CardKey {
            look: CardLook {
                picture,
                gradient: DEFAULT_GRADIENT,
                shade: Shade::None,
                radius: 0,
            },
            width: 12,
            height: 8,
        }
    }

    #[test]
    fn every_preset_decodes() {
        for preset in BackgroundPreset::ALL {
            let pixels = decode(&Picture::Preset(preset));

            assert!(pixels.is_some(), "{preset:?} doesn't decode");
            let pixels = pixels.unwrap();
            let long_side = pixels.width().max(pixels.height());
            assert!(
                long_side <= PICTURE_SIDE_MAX,
                "{preset:?} is {long_side} px"
            );
            assert!(
                pixels.width() > pixels.height(),
                "{preset:?} isn't landscape"
            );
        }
    }

    #[test]
    fn a_card_draws_its_picture_and_remembers_the_decoded_picture() {
        let mut pictures = byte_budget_cache(CARD_PICTURES_BYTES_MAX);
        let key = card_key(Some(Picture::Preset(BackgroundPreset::Tavern)));

        let drawn = draw_card(&mut pictures, &key);

        assert!(drawn.is_some_and(|pixels| (pixels.width(), pixels.height()) == (12, 8)));
        assert!(
            pictures
                .peek(&Picture::Preset(BackgroundPreset::Tavern))
                .is_some()
        );
    }

    #[test]
    fn a_card_whose_file_isnt_an_image_has_no_picture() {
        let directory = tempfile::tempdir().unwrap();
        let not_an_image = directory.path().join("tavern.png");
        std::fs::write(&not_an_image, b"not a png").unwrap();
        let mut pictures = byte_budget_cache(CARD_PICTURES_BYTES_MAX);

        let drawn = draw_card(&mut pictures, &card_key(Some(Picture::File(not_an_image))));

        assert!(drawn.is_none());
    }

    #[test]
    fn a_sliding_sidebar_reuses_a_picture_and_only_requests_its_settled_size() {
        let key = card_key(Some(Picture::Preset(BackgroundPreset::Tavern)));
        let image = Image::from_rgba8(SharedPixelBuffer::new(key.width, key.height));
        CARDS.with(|cards| {
            cards.borrow_mut().put_with_weight(key, image).unwrap();
        });
        let (sender, received) = mpsc::channel();
        JOBS.with(|jobs| *jobs.borrow_mut() = Some(sender));

        set_layout_moving(true);
        for width in [15, 17, 14] {
            let image = card(
                None,
                &Background::Preset(BackgroundPreset::Tavern),
                Shade::None,
                width,
                8,
                0,
            );
            assert_eq!((image.size().width, image.size().height), (12, 8));
        }
        assert!(
            received.try_recv().is_err(),
            "intermediate sizes reached the worker"
        );

        set_layout_moving(false);
        card(
            None,
            &Background::Preset(BackgroundPreset::Tavern),
            Shade::None,
            18,
            8,
            0,
        );
        assert!(matches!(
            received.try_recv().unwrap(),
            Job::DrawCard(CardKey {
                width: 18,
                height: 8,
                ..
            })
        ));
        assert!(received.try_recv().is_err());
    }

    #[test]
    fn a_sliding_sidebar_also_reuses_gradients_then_draws_the_settled_size() {
        let original = card(None, &Background::Gradient, Shade::None, 12, 8, 0);
        set_layout_moving(true);
        let moving = card(None, &Background::Gradient, Shade::None, 15, 8, 0);
        assert_eq!(moving.size(), original.size());
        set_layout_moving(false);
        let settled = card(None, &Background::Gradient, Shade::None, 18, 8, 0);
        assert_eq!((settled.size().width, settled.size().height), (18, 8));
    }
}
