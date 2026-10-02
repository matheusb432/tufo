mod layout;
mod seen;

use std::{cell::RefCell, rc::Rc, time::Duration};

use layout::{Measurement, Rect, Target, place_card};
use seen::{SeenFile, SeenTours};
use slint::{ComponentHandle as _, Model as _, Timer, TimerMode};

use crate::ui::{AppWindow, GuidedTours, Session, Theme, Tour, TourScroll, TourStep};

struct ActiveTour {
    steps: Vec<TourStep>,
    index: usize,
    reveal: bool,
    content: Rect,
}

struct TourRuntime {
    window: slint::Weak<AppWindow>,
    active: RefCell<Option<ActiveTour>>,
    seen: RefCell<SeenTours>,
    file: Option<SeenFile>,
    measurement: RefCell<Measurement>,
    timer: Timer,
}

pub(crate) fn install(window: &AppWindow) {
    let file = SeenFile::from_environment();
    let seen = file
        .as_ref()
        .and_then(|file| {
            file.load()
                .inspect_err(|error| {
                    tracing::warn!(%error, "cannot read guided tours; guides remain available");
                })
                .ok()
        })
        .unwrap_or_default();
    let runtime = Rc::new(TourRuntime {
        window: window.as_weak(),
        active: RefCell::new(None),
        seen: RefCell::new(seen),
        file,
        measurement: RefCell::new(Measurement::default()),
        timer: Timer::default(),
    });
    let guides = window.global::<GuidedTours>();
    guides.on_seen({
        let runtime = Rc::clone(&runtime);
        move |tour| runtime.seen.borrow().contains(tour)
    });
    guides.on_start({
        let runtime = Rc::clone(&runtime);
        move |tour| runtime.start(&tour)
    });
    guides.on_previous({
        let runtime = Rc::clone(&runtime);
        move || runtime.previous()
    });
    guides.on_next({
        let runtime = Rc::clone(&runtime);
        move || runtime.next()
    });
    guides.on_close({
        let runtime = Rc::clone(&runtime);
        move || runtime.close()
    });
    guides.on_report_target({
        let runtime = Rc::clone(&runtime);
        move |scroll, bounds| {
            let Some(content) = runtime
                .active
                .borrow()
                .as_ref()
                .map(|active| active.content)
            else {
                return;
            };
            runtime.measurement.borrow_mut().record_target(
                Target {
                    bounds: bounds.into(),
                    scroll,
                },
                content,
            );
        }
    });
    guides.on_report_viewport(move |scroll, bounds| {
        let mut measurement = runtime.measurement.borrow_mut();
        if measurement.viewports.len() < 16 {
            measurement.viewports.push((scroll, bounds.into()));
        }
    });
    guides.set_ready(true);
}

impl TourRuntime {
    fn start(self: &Rc<Self>, tour: &Tour) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        if self.active.borrow().is_some() {
            return;
        }
        let steps: Vec<_> = tour.steps.iter().collect();
        if steps.is_empty() {
            return;
        }
        let guides = window.global::<GuidedTours>();
        let content = content(&window);
        *self.active.borrow_mut() = Some(ActiveTour {
            steps,
            index: 0,
            reveal: true,
            content,
        });
        self.seen.borrow_mut().insert(tour.id);
        if let Some(file) = &self.file
            && let Err(error) = file.save(&self.seen.borrow())
        {
            tracing::warn!(%error, "cannot save guided tours; guides remain replayable");
        }
        guides.set_tour(tour.id);
        guides.set_layout(place_card(None, content));
        guides.set_active(true);
        self.present_step(&window);
        let runtime = Rc::downgrade(self);
        self.timer
            .start(TimerMode::Repeated, Duration::from_millis(50), move || {
                if let Some(runtime) = runtime.upgrade() {
                    runtime.measure();
                }
            });
    }

    fn previous(&self) {
        if let Some(active) = &mut *self.active.borrow_mut() {
            active.index = active.index.saturating_sub(1);
            active.reveal = true;
        }
        if let Some(window) = self.window.upgrade() {
            self.present_step(&window);
        }
    }

    fn next(&self) {
        let finished = {
            let mut active = self.active.borrow_mut();
            let Some(active) = active.as_mut() else {
                return;
            };
            if active.index + 1 == active.steps.len() {
                true
            } else {
                active.index += 1;
                active.reveal = true;
                false
            }
        };
        if finished {
            self.close();
        } else if let Some(window) = self.window.upgrade() {
            self.present_step(&window);
        }
    }

    fn close(&self) {
        self.timer.stop();
        self.active.borrow_mut().take();
        *self.measurement.borrow_mut() = Measurement::default();
        if let Some(window) = self.window.upgrade() {
            window.global::<GuidedTours>().set_active(false);
        }
    }

    fn present_step(&self, window: &AppWindow) {
        let active = self.active.borrow();
        let Some(active) = active.as_ref() else {
            return;
        };
        let step = &active.steps[active.index];
        let guides = window.global::<GuidedTours>();
        guides.set_anchor(step.anchor.clone());
        guides.set_title(step.title.clone());
        guides.set_body(step.body.clone());
        guides.set_step(i32::try_from(active.index).unwrap_or(0));
        guides.set_count(i32::try_from(active.steps.len()).unwrap_or(0));
        *self.measurement.borrow_mut() = Measurement::default();
        guides.set_measurement(guides.get_measurement().wrapping_add(1));
    }

    fn measure(&self) {
        let Some(window) = self.window.upgrade() else {
            self.timer.stop();
            return;
        };
        let guides = window.global::<GuidedTours>();
        let content = content(&window);
        let measurement = std::mem::take(&mut *self.measurement.borrow_mut());
        let resized = self
            .active
            .borrow()
            .as_ref()
            .is_some_and(|active| active.content != content);
        // The prior sample belongs to the old layout; reveal from the next native sample.
        let target = (!resized)
            .then(|| measurement.first_target(content))
            .flatten();
        let visible = target.and_then(|target| measurement.visible(target, content));
        self.reveal_target(&window, target, content, visible.is_none());
        let mut layout = place_card(visible, content);
        if !layout.spotlight_visible {
            // Fade the old highlight in place when the next target is unavailable.
            layout.spotlight = guides.get_layout().spotlight;
        }
        guides.set_layout(layout);
        guides.set_measurement(guides.get_measurement().wrapping_add(1));
    }

    fn reveal_target(
        &self,
        window: &AppWindow,
        target: Option<Target>,
        content: Rect,
        offscreen: bool,
    ) {
        let mut active = self.active.borrow_mut();
        let Some(active) = active.as_mut() else {
            return;
        };
        active.reveal |= active.content != content || offscreen;
        active.content = content;
        let Some(target) = target.filter(|_| active.reveal) else {
            return;
        };
        active.reveal = false;
        if target.scroll != TourScroll::None {
            let guides = window.global::<GuidedTours>();
            guides.set_reveal_scroll(target.scroll);
            guides.set_reveal_target(target.bounds.into());
            guides.set_reveal_revision(guides.get_reveal_revision().wrapping_add(1));
        }
    }
}

fn content(window: &AppWindow) -> Rect {
    let size = window
        .window()
        .size()
        .to_logical(window.window().scale_factor());
    let top = if window.global::<Session>().get_mini_player() {
        0.0
    } else {
        window.global::<Theme>().get_title_row_height()
    };
    Rect {
        x: 0.0,
        y: top,
        width: size.width,
        height: (size.height - top).max(0.0),
    }
}
