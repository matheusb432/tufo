mod fixture;
mod platform;
mod report;
mod tours;

use std::time::Instant;

use clap::Parser as _;
use platform::{Renderer, STEP};
use report::{Compatibility, Report, Sample};
use slint::{ComponentHandle as _, Model as _};

use crate::{
    backgrounds,
    presentation::{Navigation, PageKind},
    render,
    ui::{AppWindow, Clock, LibraryView, Session},
};

const BATCHES: u32 = 5;
const FRAME_STEP_NS: u64 = 8_333_333;

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
        #[command(flatten)]
        configuration: Configuration,
    },
}

#[derive(clap::Args)]
struct Configuration {
    /// Measure guided tour entry, steps, reversals, exit, and idle tracking.
    #[arg(long)]
    tours: bool,
    /// Capture tour screenshots and check interaction state without timing the workload.
    #[arg(long, requires = "tours", conflicts_with = "update")]
    verify_only: bool,
    /// Physical framebuffer width in pixels.
    #[arg(long, default_value_t = platform::WIDTH, value_parser = clap::value_parser!(u32).range(1..=16384))]
    width: u32,
    /// Physical framebuffer height in pixels.
    #[arg(long, default_value_t = platform::HEIGHT, value_parser = clap::value_parser!(u32).range(1..=16384))]
    height: u32,
    /// Display scale as a percentage; 125 represents 1.25 physical pixels per logical pixel.
    #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u32).range(50..=400))]
    scale_percent: u32,
}

impl Default for Configuration {
    fn default() -> Self {
        Self {
            tours: false,
            verify_only: false,
            width: platform::WIDTH,
            height: platform::HEIGHT,
            scale_percent: 100,
        }
    }
}

pub(super) fn run() -> anyhow::Result<()> {
    let Arguments {
        command: Command::Benchmark {
            update,
            configuration,
        },
    } = Arguments::parse();
    anyhow::ensure!(
        !cfg!(debug_assertions) || configuration.verify_only,
        "GUI benchmarking requires --release"
    );
    anyhow::ensure!(
        std::env::var_os("SLINT_DEBUG_PERFORMANCE").is_none(),
        "unset SLINT_DEBUG_PERFORMANCE to preserve partial rendering"
    );
    anyhow::ensure!(
        u64::from(configuration.width) * u64::from(configuration.height) <= 64_000_000,
        "GUI benchmark framebuffer exceeds 64 million pixels"
    );
    let compatibility = Compatibility::current(&configuration);
    let root = report::directory(&configuration);
    let baseline = if configuration.verify_only {
        None
    } else {
        report::baseline(&root, &compatibility, update)?
    };
    std::fs::create_dir_all(root.join("current"))?;
    let (window, mut renderer) = create_window(&configuration)?;
    if configuration.tours {
        return tours::run(
            &window,
            &mut renderer,
            &root,
            compatibility,
            baseline.as_ref(),
            update,
            configuration.verify_only,
        );
    }
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
    for batch in 0..BATCHES {
        for expanded in [false, true, false, true, false, true] {
            window.global::<Session>().set_drawer_pinned(expanded);
            animate(&mut renderer, "sidebar", batch, &mut samples)?;
        }
        for _ in 0..2 {
            window.invoke_open_settings();
            animate(&mut renderer, "settings", batch, &mut samples)?;
            window.invoke_close_dialog();
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
    capture(&window, &mut renderer, &root)?;
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

fn capture(
    window: &AppWindow,
    renderer: &mut Renderer,
    root: &std::path::Path,
) -> anyhow::Result<()> {
    fixture::playback(window, false);
    window.global::<Clock>().set_seconds(0.75);
    page(window, PageKind::Soundboards);
    for (name, expanded) in [("expanded", true), ("collapsed", false)] {
        window.global::<Session>().set_drawer_pinned(expanded);
        renderer.settle()?;
        renderer.screenshot(&root.join("current").join(format!("{name}.png")))?;
    }
    window.global::<Session>().set_drawer_pinned(true);
    renderer.settle()?;
    window.invoke_open_settings();
    renderer.settle()?;
    renderer.screenshot(&root.join("current/settings.png"))?;
    window.invoke_close_dialog();
    page(window, PageKind::Playlist);
    renderer.settle()?;
    renderer.screenshot(&root.join("current/playlist.png"))?;
    anyhow::ensure!(
        window.global::<LibraryView>().get_tracks().row_count() == 13,
        "playlist fixture changed"
    );
    Ok(())
}

fn create_window(configuration: &Configuration) -> anyhow::Result<(AppWindow, Renderer)> {
    crate::i18n::initialize(crate::i18n::Language::English)?;
    let renderer = Renderer::new(configuration)?;
    let window = AppWindow::new()?;
    crate::i18n::select_window_language(crate::i18n::Language::English)?;
    crate::bind_art(&window);
    backgrounds::install(&window);
    render::sound_icons(&window);
    fixture::playback(&window, false);
    window.global::<Session>().set_drawer_pinned(true);
    page(&window, PageKind::Soundboards);
    renderer.show(&window)?;
    Ok((window, renderer))
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
