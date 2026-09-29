mod fixture;
mod platform;
mod report;

use std::time::Instant;

use clap::Parser as _;
use platform::{Renderer, STEP};
use report::{Compatibility, Report, Sample};
use slint::{
    ComponentHandle as _, LogicalPosition, Model as _,
    platform::{Key, PointerEventButton, WindowEvent},
};

use crate::{
    backgrounds,
    presentation::{Navigation, PageKind},
    render,
    ui::{AppWindow, Clock, LibraryView, Session},
};

#[derive(clap::Parser)]
struct Arguments {
    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Subcommand)]
enum Command {
    /// Render the production GUI into memory and compare animation costs.
    Benchmark {
        /// Capture or replace the compatible performance baseline.
        #[arg(long)]
        update: bool,
    },
}

pub(super) fn run() -> anyhow::Result<()> {
    let Arguments {
        command: Command::Benchmark { update },
    } = Arguments::parse();
    anyhow::ensure!(
        !cfg!(debug_assertions),
        "GUI benchmarking requires --release"
    );
    anyhow::ensure!(
        std::env::var_os("SLINT_DEBUG_PERFORMANCE").is_none(),
        "unset SLINT_DEBUG_PERFORMANCE to preserve partial rendering"
    );
    let compatibility = Compatibility::current();
    let root = report::directory();
    let baseline = report::baseline(&root, &compatibility, update)?;
    std::fs::create_dir_all(root.join("current"))?;
    crate::i18n::initialize(crate::i18n::Language::English)?;
    let mut renderer = Renderer::new()?;
    let window = AppWindow::new()?;
    crate::i18n::select_window_language(crate::i18n::Language::English)?;
    crate::bind_art(&window);
    backgrounds::install(&window);
    render::sound_icons(&window);
    fixture::playback(&window, false);
    window.global::<Session>().set_drawer_pinned(true);
    page(&window, PageKind::Soundboards);
    Renderer::show(&window)?;
    let started = Instant::now();
    let counters = backgrounds::metrics::snapshot();
    let mut samples = Vec::new();
    animate(&mut renderer, "cold", 0, &mut samples)?;
    renderer.settle()?;
    for expanded in [false, true] {
        window.global::<Session>().set_drawer_pinned(expanded);
        animate(&mut renderer, "warmup", 0, &mut Vec::new())?;
        renderer.settle()?;
    }
    fixture::playback(&window, true);
    for batch in 0..5 {
        for expanded in [false, true, false, true, false, true] {
            window.global::<Session>().set_drawer_pinned(expanded);
            animate(&mut renderer, "sidebar", batch, &mut samples)?;
        }
        for _ in 0..2 {
            settings(&window, true);
            animate(&mut renderer, "settings", batch, &mut samples)?;
            settings(&window, false);
            animate(&mut renderer, "settings", batch, &mut samples)?;
        }
        for kind in [
            PageKind::Playlist,
            PageKind::Soundboard,
            PageKind::Soundboards,
        ] {
            page(&window, kind);
            animate(&mut renderer, "pages", batch, &mut samples)?;
        }
    }
    let elapsed_ms = u64::try_from(started.elapsed().as_millis())?;
    let artwork = backgrounds::metrics::snapshot().since(counters);
    let rss_peak_kib = report::rss_peak_kib();
    fixture::playback(&window, false);
    window.global::<Clock>().set_seconds(0.75);
    page(&window, PageKind::Soundboards);
    for (name, expanded) in [("expanded", true), ("collapsed", false)] {
        window.global::<Session>().set_drawer_pinned(expanded);
        renderer.settle()?;
        renderer.screenshot(&root.join("current").join(format!("{name}.png")))?;
    }
    window.global::<Session>().set_drawer_pinned(true);
    renderer.settle()?;
    settings(&window, true);
    renderer.settle()?;
    renderer.screenshot(&root.join("current/settings.png"))?;
    settings(&window, false);
    page(&window, PageKind::Playlist);
    renderer.settle()?;
    renderer.screenshot(&root.join("current/playlist.png"))?;
    anyhow::ensure!(
        window.global::<LibraryView>().get_tracks().row_count() == 13,
        "playlist fixture changed"
    );
    let report = Report {
        compatibility,
        revision: report::revision()?,
        elapsed_ms,
        rss_peak_kib,
        artwork,
        samples,
    };
    report::save(&root, &report, baseline.as_ref(), update)
}

fn animate(
    renderer: &mut Renderer,
    scenario: &str,
    batch: u32,
    samples: &mut Vec<Sample>,
) -> anyhow::Result<()> {
    let started = Instant::now();
    for index in 0..36 {
        let frame = renderer.frame()?;
        samples.push(Sample {
            scenario: scenario.to_owned(),
            batch,
            update_ns: frame.update_ns,
            render_ns: frame.render_ns,
            dirty_pixels: frame.dirty_pixels,
        });
        let deadline = started + STEP * (index + 1);
        std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
    }
    Ok(())
}

fn page(window: &AppWindow, page: PageKind) {
    render::library(
        &window.as_weak(),
        fixture::page(page),
        Navigation {
            page,
            collection_id: Some(tufo_models::library::CollectionId::from_uuid(
                uuid::Uuid::from_u128(5),
            )),
        },
    );
}

fn settings(window: &AppWindow, open: bool) {
    let window = window.window();
    if open {
        let position = LogicalPosition::new(28.0, 678.0);
        window.dispatch_event(WindowEvent::PointerPressed {
            position,
            button: PointerEventButton::Left,
        });
        window.dispatch_event(WindowEvent::PointerReleased {
            position,
            button: PointerEventButton::Left,
        });
    } else {
        window.dispatch_event(WindowEvent::KeyPressed {
            text: Key::Escape.into(),
        });
        window.dispatch_event(WindowEvent::KeyReleased {
            text: Key::Escape.into(),
        });
    }
}
