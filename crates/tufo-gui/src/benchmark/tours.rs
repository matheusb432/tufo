use std::{path::Path, time::Instant};

use slint::{ComponentHandle as _, Model as _};

use super::{
    BATCHES, animate, page,
    platform::Renderer,
    report::{self, Compatibility, Report, Sample},
};
use crate::{
    backgrounds, guided_tours,
    presentation::PageKind,
    ui::{AppWindow, Clock, GuidedTours, HomeTour, SettingsTour, Tour},
};

pub(super) const SCREENSHOTS: &[&str] = &[
    "home-first",
    "home-moving",
    "home-navigation",
    "home-last",
    "settings-first",
    "settings-missing",
    "settings-last",
    "closed",
];

pub(super) fn run(
    window: &AppWindow,
    renderer: &mut Renderer,
    root: &Path,
    compatibility: Compatibility,
    baseline: Option<&Report>,
    update: bool,
    verify_only: bool,
) -> anyhow::Result<()> {
    guided_tours::install_ephemeral(window);
    page(window, PageKind::Home);
    renderer.settle()?;
    let guides = window.global::<GuidedTours>();
    let home = window.global::<HomeTour>().get_guide();
    let settings = window.global::<SettingsTour>().get_guide();
    let mut samples = Vec::new();
    if !verify_only {
        exercise(window, renderer, &home, 0, &mut Vec::new())?;
    }
    let started = Instant::now();
    let counters = backgrounds::metrics::snapshot();
    for batch in 0..if verify_only { 0 } else { BATCHES } {
        exercise(window, renderer, &home, batch, &mut samples)?;
        window.invoke_open_settings();
        renderer.settle()?;
        exercise(window, renderer, &settings, batch, &mut samples)?;
        window.invoke_close_dialog();
        renderer.settle()?;
        super::fixture::playback(window, true);
        guides.invoke_start(home.clone());
        animate(renderer, "warmup", batch, &mut Vec::new())?;
        for _ in 1..home.steps.row_count() {
            guides.invoke_next();
            animate(renderer, "tour-live", batch, &mut samples)?;
        }
        guides.invoke_close();
        animate(renderer, "warmup", batch, &mut Vec::new())?;
        super::fixture::playback(window, false);
        renderer.settle()?;
    }
    let elapsed_ms = u64::try_from(started.elapsed().as_millis())?;
    let artwork = backgrounds::metrics::snapshot().since(counters);
    let rss_peak_kib = report::rss_peak_kib();
    capture(window, renderer, root, &home, &settings)?;
    if verify_only {
        println!(
            "Tour interaction-state checks passed; screenshots: {}",
            root.join("current").display()
        );
        return Ok(());
    }
    report::save(
        root,
        &Report {
            compatibility,
            revision: report::revision()?,
            elapsed_ms,
            rss_peak_kib,
            artwork,
            samples,
        },
        baseline,
        update,
    )
}

fn capture(
    window: &AppWindow,
    renderer: &mut Renderer,
    root: &Path,
    home: &Tour,
    settings: &Tour,
) -> anyhow::Result<()> {
    let guides = window.global::<GuidedTours>();
    window.global::<Clock>().set_seconds(0.75);
    guides.invoke_start(home.clone());
    renderer.settle()?;
    screenshot(renderer, root, "home-first")?;
    guides.invoke_next();
    for _ in 0..12 {
        renderer.frame()?;
    }
    screenshot(renderer, root, "home-moving")?;
    renderer.settle()?;
    guides.invoke_next();
    renderer.settle()?;
    screenshot(renderer, root, "home-navigation")?;
    for _ in 3..home.steps.row_count() {
        guides.invoke_next();
        renderer.settle()?;
    }
    screenshot(renderer, root, "home-last")?;
    guides.invoke_next();
    renderer.settle()?;
    anyhow::ensure!(!guides.get_active(), "Finish did not close the home guide");
    window.invoke_open_settings();
    renderer.settle()?;
    guides.invoke_start(settings.clone());
    renderer.settle()?;
    screenshot(renderer, root, "settings-first")?;
    guides.invoke_next();
    renderer.settle()?;
    anyhow::ensure!(
        !guides.get_layout().spotlight_visible,
        "unconfigured Discord target unexpectedly visible"
    );
    screenshot(renderer, root, "settings-missing")?;
    for _ in 2..settings.steps.row_count() {
        guides.invoke_next();
        renderer.settle()?;
    }
    screenshot(renderer, root, "settings-last")?;
    guides.invoke_next();
    renderer.settle()?;
    window.invoke_close_dialog();
    renderer.settle()?;
    screenshot(renderer, root, "closed")?;
    anyhow::ensure!(
        !guides.get_active(),
        "Finish did not close the settings guide"
    );
    Ok(())
}

fn exercise(
    window: &AppWindow,
    renderer: &mut Renderer,
    tour: &Tour,
    batch: u32,
    samples: &mut Vec<Sample>,
) -> anyhow::Result<()> {
    let guides = window.global::<GuidedTours>();
    guides.invoke_start(tour.clone());
    animate(renderer, "tour-open", batch, samples)?;
    for _ in 1..tour.steps.row_count() {
        guides.invoke_next();
        animate(renderer, "tour-step", batch, samples)?;
    }
    for _ in 1..tour.steps.row_count() {
        guides.invoke_previous();
        animate(renderer, "tour-reverse", batch, samples)?;
    }
    for _ in 0..3 {
        animate(renderer, "tour-idle", batch, samples)?;
    }
    guides.invoke_close();
    animate(renderer, "tour-close", batch, samples)?;
    renderer.settle()?;
    Ok(())
}

fn screenshot(renderer: &mut Renderer, root: &Path, name: &str) -> anyhow::Result<()> {
    renderer.screenshot(&root.join("current").join(format!("{name}.png")))
}
