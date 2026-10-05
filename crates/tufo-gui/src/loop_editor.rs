//! The loop editor's draft, time text, and waveform view, apart from the window and the server.

use std::time::Duration;

use slint::{Rgba8Pixel, SharedPixelBuffer};
use tufo_models::library::song_loop::{
    LoopMarkers, LoopRegion, LoopRegionError, SourceFrames, SourceTiming, crossfade_max,
    nearest_frames,
};

const VIEW_FRAMES_MIN: u64 = 256;
const ZOOM_FACTOR: u64 = 4;
const MILLISECONDS_PER_SECOND: u64 = 1_000;
const FRACTION_DIGITS_MAX: usize = 9;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreviewKind {
    FromPlayhead,
    Seam,
}

/// What the editor shows. Positions are fractions of the visible waveform.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct LoopEditorFrame {
    pub(crate) song_title: String,
    pub(crate) loading: bool,
    pub(crate) notice: String,
    pub(crate) error: String,
    pub(crate) has_saved_loop: bool,
    pub(crate) can_save: bool,
    pub(crate) playback_start_text: String,
    pub(crate) loop_start_text: String,
    pub(crate) loop_end_text: String,
    pub(crate) crossfade_text: String,
    pub(crate) playhead_text: String,
    pub(crate) playback_start_x: f32,
    pub(crate) loop_start_x: f32,
    pub(crate) loop_end_x: f32,
    pub(crate) crossfade_start_x: f32,
    pub(crate) playhead_x: f32,
    pub(crate) view_start: f32,
    pub(crate) view_visible: f32,
    pub(crate) waveform_revision: i32,
    pub(crate) preview: Option<PreviewKind>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Marker {
    PlaybackStart,
    LoopStart,
    LoopEnd,
}

/// The marker and crossfade fields as typed, which may not have reached the draft yet.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct LoopFieldTexts {
    pub(crate) playback_start: String,
    pub(crate) loop_start: String,
    pub(crate) loop_end: String,
    pub(crate) crossfade: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LoopFieldError {
    UnreadableTime,
    UnreadableCrossfade,
}

/// Keeps its markers in order while they move, so it only fails to form a region for an empty
/// file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LoopDraft {
    timing: SourceTiming,
    markers: LoopMarkers,
}

impl LoopDraft {
    /// Markers that don't fit the file or each other stop at the nearest frame that does.
    pub(crate) fn new(timing: SourceTiming, markers: Option<LoopMarkers>) -> Self {
        let mut draft = Self {
            timing,
            markers: LoopMarkers::whole_file(timing.frame_count),
        };
        if let Some(markers) = markers {
            draft.move_marker(Marker::LoopEnd, markers.loop_end);
            draft.move_marker(Marker::LoopStart, markers.loop_start);
            draft.move_marker(Marker::PlaybackStart, markers.playback_start);
            draft.set_crossfade(markers.crossfade);
        }
        draft
    }

    pub(crate) fn timing(&self) -> SourceTiming {
        self.timing
    }

    pub(crate) fn markers(&self) -> LoopMarkers {
        self.markers
    }

    pub(crate) fn frame_of(&self, marker: Marker) -> SourceFrames {
        match marker {
            Marker::PlaybackStart => self.markers.playback_start,
            Marker::LoopStart => self.markers.loop_start,
            Marker::LoopEnd => self.markers.loop_end,
        }
    }

    /// Stops at the neighboring markers.
    pub(crate) fn move_marker(&mut self, marker: Marker, frame: SourceFrames) {
        let one = SourceFrames(1);
        let markers = &mut self.markers;
        match marker {
            Marker::PlaybackStart => markers.playback_start = frame.min(markers.loop_start),
            Marker::LoopStart => {
                markers.loop_start = frame
                    .min(markers.loop_end.saturating_sub(one))
                    .max(markers.playback_start);
            }
            Marker::LoopEnd => {
                markers.loop_end = frame
                    .max(markers.loop_start.saturating_add(one))
                    .min(self.timing.frame_count);
            }
        }
        self.set_crossfade(self.markers.crossfade);
    }

    pub(crate) fn type_marker(&mut self, marker: Marker, text: &str) -> Result<(), LoopFieldError> {
        let frame = self.typed_frame(marker, text)?;
        self.move_marker(marker, frame);
        Ok(())
    }

    pub(crate) fn type_crossfade(&mut self, text: &str) -> Result<(), LoopFieldError> {
        let frames = self.typed_crossfade(text)?;
        self.set_crossfade(frames);
        Ok(())
    }

    /// Moves every marker at once, so none stops at a neighbor that is also moving; changes
    /// nothing unless every field reads.
    pub(crate) fn type_fields(&mut self, fields: &LoopFieldTexts) -> Result<(), LoopFieldError> {
        let typed = LoopMarkers {
            playback_start: self.typed_frame(Marker::PlaybackStart, &fields.playback_start)?,
            loop_start: self.typed_frame(Marker::LoopStart, &fields.loop_start)?,
            loop_end: self.typed_frame(Marker::LoopEnd, &fields.loop_end)?,
            crossfade: self.typed_crossfade(&fields.crossfade)?,
        };
        *self = Self::new(self.timing, Some(typed));
        Ok(())
    }

    /// Text that still reads as the shown time keeps the marker's exact frame.
    fn typed_frame(&self, marker: Marker, text: &str) -> Result<SourceFrames, LoopFieldError> {
        let frame = self.frame_of(marker);
        if text.trim() == format_time(frame, self.timing) {
            return Ok(frame);
        }
        parse_time(text, self.timing).ok_or(LoopFieldError::UnreadableTime)
    }

    fn typed_crossfade(&self, text: &str) -> Result<SourceFrames, LoopFieldError> {
        let frames = self.markers.crossfade;
        if text.trim() == format_milliseconds(frames, self.timing) {
            return Ok(frames);
        }
        parse_milliseconds(text, self.timing).ok_or(LoopFieldError::UnreadableCrossfade)
    }

    fn set_crossfade(&mut self, frames: SourceFrames) {
        self.markers.crossfade = frames.min(crossfade_max(
            self.timing,
            self.markers.loop_start,
            self.markers.loop_end,
        ));
    }

    pub(crate) fn region(&self) -> Result<LoopRegion, LoopRegionError> {
        LoopRegion::try_new(self.timing, self.markers)
    }
}

/// A preview from `start` wraps at the loop end, except one starting past it.
pub(crate) fn preview_frame(
    region: LoopRegion,
    start: SourceFrames,
    elapsed: Duration,
) -> SourceFrames {
    let timing = region.timing();
    let played = timing.frames_in(elapsed);
    if start < region.markers().loop_end {
        region.source_frame_after(start, played)
    } else {
        start.saturating_add(played).min(timing.frame_count)
    }
}

/// The side before the loop end plays, then the side after the loop start, then silence; each
/// side lasts at most one loop, as the server plays it.
pub(crate) fn seam_preview_frame(
    region: LoopRegion,
    side: SourceFrames,
    cycle: Duration,
    elapsed: Duration,
) -> SourceFrames {
    let timing = region.timing();
    let markers = region.markers();
    let side = side.min(region.loop_length());
    let cycle_nanos = cycle.as_nanos().max(1);
    let into_cycle = timing.frames_in(Duration::from_nanos(
        u64::try_from(elapsed.as_nanos() % cycle_nanos).unwrap_or(0),
    ));
    if into_cycle < side {
        return markers
            .loop_end
            .saturating_sub(side)
            .saturating_add(into_cycle);
    }
    markers
        .loop_start
        .saturating_add(into_cycle.saturating_sub(side).min(side))
}

/// `m:ss.mmm`, or `h:mm:ss.mmm` from an hour on.
pub(crate) fn format_time(frames: SourceFrames, timing: SourceTiming) -> String {
    let milliseconds = nearest_frames(timing.duration_of(frames), 1_000);
    let (seconds, milliseconds) = (
        milliseconds / MILLISECONDS_PER_SECOND,
        milliseconds % MILLISECONDS_PER_SECOND,
    );
    let (minutes, seconds) = (seconds / 60, seconds % 60);
    if minutes >= 60 {
        format!(
            "{}:{:02}:{seconds:02}.{milliseconds:03}",
            minutes / 60,
            minutes % 60
        )
    } else {
        format!("{minutes}:{seconds:02}.{milliseconds:03}")
    }
}

/// Accepts `[[h:]m:]s[.fraction]`, with a comma as the decimal separator too.
fn parse_time(text: &str, timing: SourceTiming) -> Option<SourceFrames> {
    let text = text.trim().replace(',', ".");
    let (whole, fraction) = text.split_once('.').unwrap_or((&text, ""));
    let mut seconds = 0_u64;
    for (index, part) in whole.split(':').enumerate() {
        if index > 2 || part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        seconds = seconds.checked_mul(60)?.checked_add(part.parse().ok()?)?;
    }
    let duration = Duration::from_secs(seconds) + parse_fraction(fraction)?;
    Some(timing.frames_in(duration))
}

/// Accepts whole or fractional milliseconds.
fn parse_milliseconds(text: &str, timing: SourceTiming) -> Option<SourceFrames> {
    let text = text.trim().replace(',', ".");
    let (whole, fraction) = text.split_once('.').unwrap_or((&text, ""));
    if whole.is_empty() || !whole.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let microseconds = parse_fraction(fraction)?.as_micros() / 1_000;
    let duration = Duration::from_millis(whole.parse().ok()?)
        + Duration::from_micros(u64::try_from(microseconds).ok()?);
    Some(timing.frames_in(duration))
}

pub(crate) fn format_milliseconds(frames: SourceFrames, timing: SourceTiming) -> String {
    nearest_frames(timing.duration_of(frames), 1_000).to_string()
}

fn parse_fraction(fraction: &str) -> Option<Duration> {
    if fraction.len() > FRACTION_DIGITS_MAX || !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    if fraction.is_empty() {
        return Some(Duration::ZERO);
    }
    let digits = u32::try_from(fraction.len()).ok()?;
    let nanoseconds = fraction.parse::<u32>().ok()? * 10_u32.pow(9 - digits);
    Some(Duration::from_nanos(u64::from(nanoseconds)))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WaveformView {
    start: SourceFrames,
    length: SourceFrames,
    frame_count: SourceFrames,
}

impl WaveformView {
    pub(crate) fn whole(frame_count: SourceFrames) -> Self {
        Self {
            start: SourceFrames::ZERO,
            length: frame_count.max(SourceFrames(1)),
            frame_count,
        }
    }

    pub(crate) fn zoom(&mut self, zoom_in: bool, center: SourceFrames) {
        let length = if zoom_in {
            self.length.0 / ZOOM_FACTOR
        } else {
            self.length.0.saturating_mul(ZOOM_FACTOR)
        };
        self.length = SourceFrames(
            length
                .max(VIEW_FRAMES_MIN.min(self.frame_count.0))
                .min(self.frame_count.0)
                .max(1),
        );
        self.center_on(center);
    }

    pub(crate) fn show_around(&mut self, center: SourceFrames, length: SourceFrames) {
        self.length = length.min(self.frame_count).max(SourceFrames(1));
        self.center_on(center);
    }

    pub(crate) fn contains(&self, frame: SourceFrames) -> bool {
        (0.0..=1.0).contains(&self.fraction_of(frame))
    }

    pub(crate) fn center_on(&mut self, frame: SourceFrames) {
        self.place_start(frame.saturating_sub(SourceFrames(self.length.0 / 2)));
    }

    /// `fraction` is the share of the file before the view.
    pub(crate) fn scroll_to(&mut self, fraction: f32) {
        self.place_start(self.frame_of_file_fraction(fraction));
    }

    fn place_start(&mut self, start: SourceFrames) {
        self.start = start.min(self.frame_count.saturating_sub(self.length));
    }

    /// Outside 0 to 1 for frames outside the view.
    pub(crate) fn fraction_of(&self, frame: SourceFrames) -> f32 {
        ratio(
            i128::from(frame.0) - i128::from(self.start.0),
            i128::from(self.length.0),
        )
    }

    pub(crate) fn frame_at(&self, fraction: f32) -> SourceFrames {
        let offset = scale(self.length.0, fraction.clamp(0.0, 1.0));
        self.start
            .saturating_add(SourceFrames(offset))
            .min(self.frame_count)
    }

    pub(crate) fn start_fraction(&self) -> f32 {
        ratio(i128::from(self.start.0), i128::from(self.frame_count.0))
    }

    pub(crate) fn visible_fraction(&self) -> f32 {
        ratio(i128::from(self.length.0), i128::from(self.frame_count.0)).min(1.0)
    }

    fn frame_of_file_fraction(&self, fraction: f32) -> SourceFrames {
        SourceFrames(scale(self.frame_count.0, fraction.clamp(0.0, 1.0)))
    }

    fn frames_per_pixel(&self, width: u32) -> f64 {
        to_f64(self.length.0) / f64::from(width.max(1))
    }
}

/// Draws the view's peaks as bars mirrored around the middle, on a transparent background.
pub(crate) fn render_waveform(
    peaks: &[u8],
    bucket_frames: u64,
    view: WaveformView,
    size: (u32, u32),
    color: Rgba8Pixel,
) -> SharedPixelBuffer<Rgba8Pixel> {
    let (width, height) = size;
    let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(width, height);
    let frames_per_pixel = view.frames_per_pixel(width);
    let bucket_frames = to_f64(bucket_frames.max(1));
    let half_height = f64::from(height) / 2.0;
    let pixels = buffer.make_mut_slice();
    for column in 0..width {
        let first_frame = to_f64(view.start.0) + f64::from(column) * frames_per_pixel;
        let first_bucket = to_index(first_frame / bucket_frames);
        let last_bucket = to_index(((first_frame + frames_per_pixel) / bucket_frames).ceil())
            .saturating_sub(1)
            .max(first_bucket);
        let Some(level) = peaks
            .get(first_bucket..=last_bucket.min(peaks.len().saturating_sub(1)))
            .and_then(|buckets| buckets.iter().copied().max())
        else {
            continue;
        };
        let bar_half = (f64::from(level) / 255.0 * (half_height - 1.0)).max(0.5);
        let top = to_row(half_height - bar_half);
        let bottom = to_row(half_height + bar_half).min(height);
        for row in top..bottom {
            let index = usize::try_from(row * width + column).unwrap_or(usize::MAX);
            if let Some(pixel) = pixels.get_mut(index) {
                *pixel = color;
            }
        }
    }
    buffer
}

#[expect(
    clippy::cast_precision_loss,
    reason = "frame counts past 2^52 are centuries of audio"
)]
fn to_f64(frames: u64) -> f64 {
    frames as f64
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "callers pass non-negative values within the file's frame count"
)]
fn to_index(value: f64) -> usize {
    value.max(0.0) as usize
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "rows lie within the image's height"
)]
fn to_row(value: f64) -> u32 {
    value.round().max(0.0) as u32
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "a view's fractions only position pixels"
)]
fn ratio(numerator: i128, denominator: i128) -> f32 {
    if denominator == 0 {
        return 0.0;
    }
    (numerator as f64 / denominator as f64) as f32
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the clamped fraction keeps the product within `frames`"
)]
fn scale(frames: u64, fraction: f32) -> u64 {
    (to_f64(frames) * f64::from(fraction)).round() as u64
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use super::*;

    fn timing(frame_count: u64) -> SourceTiming {
        SourceTiming {
            sample_rate: NonZeroU32::new(48_000).unwrap(),
            frame_count: SourceFrames(frame_count),
        }
    }

    #[test]
    fn a_new_draft_loops_the_whole_file() {
        let draft = LoopDraft::new(timing(480_000), None);

        assert_eq!(
            draft.markers(),
            LoopMarkers::whole_file(SourceFrames(480_000))
        );
        assert!(draft.region().is_ok());
    }

    #[test]
    fn markers_stop_at_their_neighbors_and_the_crossfade_follows() {
        let mut draft = LoopDraft::new(timing(480_000), None);
        draft.move_marker(Marker::LoopStart, SourceFrames(96_000));
        draft.move_marker(Marker::PlaybackStart, SourceFrames(200_000));
        draft.set_crossfade(SourceFrames(48_000));
        draft.move_marker(Marker::LoopEnd, SourceFrames(10));

        let markers = draft.markers();
        assert_eq!(markers.playback_start, SourceFrames(96_000));
        assert_eq!(markers.loop_end, SourceFrames(96_001));
        assert_eq!(markers.crossfade, SourceFrames(1));
        assert!(draft.region().is_ok());
    }

    #[test]
    fn saved_markers_past_a_shorter_file_stop_at_its_end() {
        let saved = LoopMarkers {
            playback_start: SourceFrames(1_000),
            loop_start: SourceFrames(400_000),
            loop_end: SourceFrames(900_000),
            crossfade: SourceFrames(4_800),
        };

        let draft = LoopDraft::new(timing(480_000), Some(saved));

        assert_eq!(draft.markers().loop_end, SourceFrames(480_000));
        assert_eq!(draft.markers().loop_start, SourceFrames(400_000));
        assert!(draft.region().is_ok());
    }

    #[test]
    fn fields_reading_as_shown_keep_frames_between_milliseconds() {
        let timing = timing(48_000 * 600);
        let mut draft = LoopDraft::new(timing, None);
        draft.move_marker(Marker::LoopStart, SourceFrames(6_762_993));
        draft.set_crossfade(SourceFrames(601));
        let shown = draft.markers();
        let fields = LoopFieldTexts {
            playback_start: format_time(shown.playback_start, timing),
            loop_start: format_time(shown.loop_start, timing),
            loop_end: format_time(shown.loop_end, timing),
            crossfade: format_milliseconds(shown.crossfade, timing),
        };

        assert_eq!(draft.type_fields(&fields), Ok(()));
        assert_eq!(draft.markers(), shown);

        let fields = LoopFieldTexts {
            loop_end: "5:00".to_owned(),
            ..fields
        };
        assert_eq!(draft.type_fields(&fields), Ok(()));
        assert_eq!(draft.markers().loop_end, SourceFrames(48_000 * 300));
        assert_eq!(draft.markers().loop_start, SourceFrames(6_762_993));
    }

    #[test]
    fn markers_typed_together_pass_their_old_neighbors() {
        let timing = timing(48_000 * 10);
        let mut draft = LoopDraft::new(timing, None);
        draft.move_marker(Marker::LoopEnd, SourceFrames(96_000));
        draft.move_marker(Marker::LoopStart, SourceFrames(48_000));
        let fields = LoopFieldTexts {
            playback_start: "0:00.000".to_owned(),
            loop_start: "0:03".to_owned(),
            loop_end: "0:04".to_owned(),
            crossfade: "0".to_owned(),
        };

        assert_eq!(draft.type_fields(&fields), Ok(()));
        assert_eq!(draft.markers().loop_start, SourceFrames(144_000));
        assert_eq!(draft.markers().loop_end, SourceFrames(192_000));
    }

    #[test]
    fn an_unreadable_field_leaves_the_whole_draft_alone() {
        let timing = timing(48_000 * 600);
        let mut draft = LoopDraft::new(timing, None);
        let before = draft.markers();
        let fields = LoopFieldTexts {
            playback_start: "0:01".to_owned(),
            loop_start: "0:02".to_owned(),
            loop_end: "soon".to_owned(),
            crossfade: "0".to_owned(),
        };

        assert_eq!(
            draft.type_fields(&fields),
            Err(LoopFieldError::UnreadableTime)
        );
        assert_eq!(draft.markers(), before);
        assert_eq!(
            draft.type_crossfade("long"),
            Err(LoopFieldError::UnreadableCrossfade)
        );
    }

    #[test]
    fn times_read_back_as_they_print() {
        let timing = timing(48_000 * 4_000);

        assert_eq!(format_time(SourceFrames(6_762_993), timing), "2:20.896");
        assert_eq!(
            format_time(SourceFrames(48_000 * 3_725), timing),
            "1:02:05.000"
        );
        assert_eq!(
            parse_time("2:20.896", timing),
            Some(SourceFrames(6_763_008))
        );
        assert_eq!(
            parse_time("1:02:05", timing),
            Some(SourceFrames(48_000 * 3_725))
        );
        assert_eq!(parse_time("14,5", timing), Some(SourceFrames(696_000)));
        assert_eq!(parse_time("1::5", timing), None);
        assert_eq!(parse_time("-1", timing), None);
        assert_eq!(parse_milliseconds("12.5", timing), Some(SourceFrames(600)));
        assert_eq!(format_milliseconds(SourceFrames(600), timing), "13");
    }

    #[test]
    fn previews_follow_the_loop_and_the_seam() {
        let region = LoopRegion::try_new(
            timing(480_000),
            LoopMarkers {
                playback_start: SourceFrames::ZERO,
                loop_start: SourceFrames(96_000),
                loop_end: SourceFrames(192_000),
                crossfade: SourceFrames::ZERO,
            },
        )
        .unwrap();
        let cycle = Duration::from_millis(2_500);
        let seam = |milliseconds| {
            seam_preview_frame(
                region,
                SourceFrames(48_000),
                cycle,
                Duration::from_millis(milliseconds),
            )
        };

        assert_eq!(
            preview_frame(region, SourceFrames(144_000), Duration::from_secs(2)),
            SourceFrames(144_000)
        );
        assert_eq!(
            preview_frame(region, SourceFrames(300_000), Duration::from_secs(1)),
            SourceFrames(348_000)
        );
        assert_eq!(seam(0), SourceFrames(144_000));
        assert_eq!(seam(1_500), SourceFrames(120_000));
        assert_eq!(seam(2_400), SourceFrames(144_000));
        assert_eq!(seam(2_500), SourceFrames(144_000));
    }

    #[test]
    fn zooming_keeps_the_view_inside_the_file_around_its_center() {
        let mut view = WaveformView::whole(SourceFrames(1_000_000));

        view.zoom(true, SourceFrames(990_000));
        assert_eq!(view.frame_at(1.0), SourceFrames(1_000_000));
        assert!((view.visible_fraction() - 0.25).abs() < 1e-6);
        assert!((view.fraction_of(SourceFrames(750_000))).abs() < 1e-6);

        view.zoom(false, SourceFrames(0));
        assert_eq!(view, WaveformView::whole(SourceFrames(1_000_000)));
    }

    #[test]
    fn the_waveform_draws_each_columns_loudest_bucket() {
        let view = WaveformView::whole(SourceFrames(4));
        let peaks = [255, 0, 0, 0];
        let lit = Rgba8Pixel::new(255, 255, 255, 255);

        let image = render_waveform(&peaks, 1, view, (4, 10), lit);

        let column = |x: u32| {
            (0..10)
                .filter(|row| image.as_slice()[usize::try_from(row * 4 + x).unwrap()] == lit)
                .count()
        };
        assert_eq!(column(0), 8);
        assert_eq!(column(3), 1);
    }
}
