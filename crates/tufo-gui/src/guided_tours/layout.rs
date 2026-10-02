use crate::ui::{TourRect, TourScroll};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Rect {
    pub(super) x: f32,
    pub(super) y: f32,
    pub(super) width: f32,
    pub(super) height: f32,
}

impl Rect {
    pub(super) fn intersection(self, other: Self) -> Option<Self> {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = (self.x + self.width).min(other.x + other.width);
        let bottom = (self.y + self.height).min(other.y + other.height);
        (right > x && bottom > y).then_some(Self {
            x,
            y,
            width: right - x,
            height: bottom - y,
        })
    }

    fn padded(self, padding: f32) -> Self {
        Self {
            x: self.x - padding,
            y: self.y - padding,
            width: self.width + 2.0 * padding,
            height: self.height + 2.0 * padding,
        }
    }
}

impl From<TourRect> for Rect {
    fn from(rect: TourRect) -> Self {
        Self {
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: rect.height,
        }
    }
}

impl From<Rect> for TourRect {
    fn from(rect: Rect) -> Self {
        Self {
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: rect.height,
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct Target {
    pub(super) bounds: Rect,
    pub(super) scroll: TourScroll,
}

#[derive(Default)]
pub(super) struct Measurement {
    pub(super) targets: Vec<Target>,
    pub(super) viewports: Vec<(TourScroll, Rect)>,
}

impl Measurement {
    pub(super) fn record_target(&mut self, target: Target, content: Rect) {
        let in_content = target.bounds.intersection(content).is_some();
        let has_fallback = self
            .targets
            .iter()
            .any(|stored| stored.scroll == target.scroll);
        if self.targets.len() < 256 && (in_content || !has_fallback) {
            self.targets.push(target);
        }
    }

    pub(super) fn visible(&self, target: Target, content: Rect) -> Option<Rect> {
        let viewport = if target.scroll == TourScroll::None {
            content
        } else {
            self.viewports
                .iter()
                .find(|(scroll, _)| *scroll == target.scroll)?
                .1
        };
        target.bounds.intersection(viewport)?.intersection(content)
    }

    pub(super) fn first_target(&self, content: Rect) -> Option<Target> {
        let visible = self
            .targets
            .iter()
            .copied()
            .filter(|target| self.visible(*target, content).is_some())
            .min_by(|left, right| {
                left.bounds
                    .y
                    .total_cmp(&right.bounds.y)
                    .then(left.bounds.x.total_cmp(&right.bounds.x))
            });
        visible.or_else(|| self.first_scroll_target(content))
    }

    fn first_scroll_target(&self, content: Rect) -> Option<Target> {
        self.targets.iter().copied().find(|target| {
            target.scroll != TourScroll::None
                && target.bounds.width > 0.0
                && target.bounds.height > 0.0
                && self.viewports.iter().any(|(scroll, viewport)| {
                    *scroll == target.scroll && viewport.intersection(content).is_some()
                })
        })
    }
}

pub(super) fn place_card(target: Option<Rect>, content: Rect) -> crate::ui::TourLayout {
    let margin = 12.0_f32
        .min(content.width / 2.0)
        .min(content.height / 2.0)
        .max(0.0);
    let width = (content.width - 2.0 * margin).clamp(0.0, 360.0);
    let height = (content.height - 2.0 * margin).clamp(0.0, 250.0);
    let mut card = Rect {
        x: content.x + (content.width - width) / 2.0,
        y: content.y + (content.height - height) / 2.0,
        width,
        height,
    };
    let spotlight = target
        .filter(|target| target.width > 0.0 && target.height > 0.0)
        .and_then(|target| target.padded(6.0).intersection(content));
    if let Some(spotlight) = spotlight {
        let below = content.y + content.height - margin - spotlight.y - spotlight.height - 12.0;
        let above = spotlight.y - 12.0 - content.y - margin;
        card.x = (spotlight.x + spotlight.width / 2.0 - width / 2.0).clamp(
            content.x + margin,
            (content.x + content.width - margin - width).max(content.x + margin),
        );
        let available = below.max(above);
        if available >= 180.0 {
            card.height = card.height.min(available);
        }
        if below >= card.height {
            card.y = spotlight.y + spotlight.height + 12.0;
        } else if above >= card.height {
            card.y = spotlight.y - 12.0 - card.height;
        } else {
            card.y = content.y + content.height - margin - card.height;
        }
    }
    crate::ui::TourLayout {
        card: card.into(),
        spotlight: spotlight
            .unwrap_or(Rect {
                x: content.x,
                y: content.y,
                width: 0.0,
                height: 0.0,
            })
            .into(),
        spotlight_visible: spotlight.is_some(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f32, y: f32, width: f32, height: f32) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn offscreen_rows_do_not_crowd_out_the_visible_anchor() {
        let content = rect(0.0, 44.0, 1000.0, 656.0);
        let mut measurement = Measurement {
            viewports: vec![(TourScroll::Collection, content)],
            ..Measurement::default()
        };
        let offscreen = Target {
            bounds: rect(250.0, -500.0, 40.0, 40.0),
            scroll: TourScroll::Collection,
        };
        for _ in 0..1000 {
            measurement.record_target(offscreen, content);
        }
        let visible = Target {
            bounds: rect(250.0, 100.0, 40.0, 40.0),
            ..offscreen
        };
        measurement.record_target(visible, content);
        assert_eq!(measurement.targets.len(), 2);
        assert_eq!(
            measurement.first_target(content).unwrap().bounds,
            visible.bounds
        );
    }

    #[test]
    fn missing_and_hidden_targets_center_the_card() {
        let content = rect(0.0, 44.0, 1000.0, 656.0);
        for target in [
            None,
            Some(rect(100.0, 50.0, 0.0, 20.0)),
            Some(rect(1100.0, 50.0, 50.0, 50.0)),
        ] {
            let layout = place_card(target, content);
            assert!(!layout.spotlight_visible);
            assert_eq!(Rect::from(layout.card), rect(320.0, 247.0, 360.0, 250.0));
        }
    }

    #[test]
    fn card_and_spotlight_stay_in_full_and_mini_content() {
        for content in [
            rect(0.0, 44.0, 440.0, 336.0),
            rect(0.0, 0.0, 320.0, 168.0),
            rect(0.0, 0.0, 0.0, 0.0),
        ] {
            for target in [
                None,
                Some(rect(-20.0, -20.0, 1000.0, 1000.0)),
                Some(rect(5.0, 50.0, 20.0, 20.0)),
            ] {
                let layout = place_card(target, content);
                let card = Rect::from(layout.card);
                assert!(card.x >= content.x && card.y >= content.y);
                assert!(card.x + card.width <= content.x + content.width);
                assert!(card.y + card.height <= content.y + content.height);
                let spotlight = layout
                    .spotlight_visible
                    .then(|| Rect::from(layout.spotlight));
                assert_eq!(
                    spotlight.and_then(|spotlight| spotlight.intersection(content)),
                    spotlight
                );
            }
        }
    }

    #[test]
    fn short_windows_shrink_the_card_to_keep_the_target_visible() {
        let layout = place_card(
            Some(rect(40.0, 304.0, 350.0, 44.0)),
            rect(0.0, 44.0, 440.0, 336.0),
        );
        let card = Rect::from(layout.card);
        let spotlight = Rect::from(layout.spotlight);
        assert!(card.height >= 180.0 && card.height < 250.0);
        assert!(card.intersection(spotlight).is_none());
    }

    #[test]
    fn visible_match_precedes_scrolled_or_hidden_matches() {
        let content = rect(0.0, 44.0, 1000.0, 656.0);
        let first = Target {
            bounds: rect(250.0, -100.0, 40.0, 40.0),
            scroll: TourScroll::Collection,
        };
        let second = Target {
            bounds: rect(250.0, 100.0, 40.0, 40.0),
            scroll: TourScroll::Collection,
        };
        let measurement = Measurement {
            targets: vec![first, second],
            viewports: vec![(TourScroll::Collection, rect(232.0, 44.0, 768.0, 550.0))],
        };
        assert_eq!(
            measurement.first_target(content).unwrap().bounds,
            second.bounds
        );
        let measurement = Measurement {
            targets: vec![first],
            ..measurement
        };
        assert_eq!(
            measurement.first_target(content).unwrap().bounds,
            first.bounds
        );
        assert!(measurement.visible(first, content).is_none());
    }
}
