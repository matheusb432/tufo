use std::{
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::Context as _;
use serde::{Deserialize, Serialize};

use super::{
    BATCHES, FRAME_STEP_NS,
    platform::{HEIGHT, WIDTH},
};
use crate::{backgrounds::metrics::Counters, files};

pub(super) const SCREENSHOTS: [&str; 4] = ["expanded", "collapsed", "settings", "playlist"];

#[derive(Deserialize, PartialEq, Serialize)]
pub(super) struct Compatibility {
    schema: u32,
    workload: u32,
    renderer: String,
    slint: String,
    profile: String,
    host: String,
    cpu: String,
    cpu_affinity: String,
    width: u32,
    height: u32,
    frame_step_ns: u64,
    batches: u32,
}

impl Compatibility {
    pub(super) fn current() -> Self {
        Self {
            schema: 1,
            workload: 1,
            renderer: "software/swapped-buffers/rgba8".to_owned(),
            slint: "1.18.1".to_owned(),
            profile: "release".to_owned(),
            host: format!("{}/{}", std::env::consts::OS, std::env::consts::ARCH),
            cpu: std::fs::read_to_string("/proc/cpuinfo")
                .ok()
                .and_then(|info| {
                    info.lines()
                        .find_map(|line| line.strip_prefix("model name").map(str::to_owned))
                })
                .unwrap_or_default(),
            cpu_affinity: std::fs::read_to_string("/proc/self/status")
                .ok()
                .and_then(|status| {
                    status.lines().find_map(|line| {
                        line.strip_prefix("Cpus_allowed_list:")
                            .map(|value| value.trim().to_owned())
                    })
                })
                .unwrap_or_default(),
            width: WIDTH,
            height: HEIGHT,
            frame_step_ns: FRAME_STEP_NS,
            batches: BATCHES,
        }
    }
}

#[derive(Deserialize, Serialize)]
pub(super) struct Sample {
    pub(super) scenario: String,
    pub(super) batch: u32,
    pub(super) update_ns: u64,
    pub(super) render_ns: u64,
    pub(super) dirty_pixels: u64,
}

#[derive(Deserialize, Serialize)]
pub(super) struct Report {
    pub(super) compatibility: Compatibility,
    pub(super) revision: String,
    pub(super) elapsed_ms: u64,
    pub(super) rss_peak_kib: Option<u64>,
    pub(super) artwork: Counters,
    pub(super) samples: Vec<Sample>,
}

pub(super) fn directory() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.artifacts/benchmarks/gui")
}

pub(super) fn baseline(
    root: &Path,
    compatibility: &Compatibility,
    update: bool,
) -> anyhow::Result<Option<Report>> {
    let path = root.join("baseline.json");
    if !path.exists() {
        anyhow::ensure!(
            update,
            "no GUI baseline; run `just gui-bench --update` first"
        );
        return Ok(None);
    }
    let report: Report = match serde_json::from_str(&std::fs::read_to_string(path)?) {
        Ok(report) => report,
        Err(_) if update => return Ok(None),
        Err(error) => return Err(error).context("invalid GUI baseline; capture it with --update"),
    };
    if report.compatibility != *compatibility {
        anyhow::ensure!(
            update,
            "incompatible GUI baseline; use --update to capture this configuration"
        );
        return Ok(None);
    }
    Ok(Some(report))
}

pub(super) fn revision() -> anyhow::Result<String> {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "cannot identify benchmark revision"
    );
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

pub(super) fn rss_peak_kib() -> Option<u64> {
    std::fs::read_to_string("/proc/self/status")
        .ok()?
        .lines()
        .find_map(|line| {
            line.strip_prefix("VmHWM:")?
                .split_whitespace()
                .next()?
                .parse()
                .ok()
        })
}

pub(super) fn save(
    root: &Path,
    report: &Report,
    baseline: Option<&Report>,
    update: bool,
) -> anyhow::Result<()> {
    if baseline.is_some() {
        compare_screenshots(root)?;
    }
    let json = serde_json::to_string_pretty(report)?;
    files::write_atomically(&root.join("current.json"), &json)?;
    for scenario in ["cold", "sidebar", "settings", "pages"] {
        print_scenario(scenario, report, baseline);
    }
    println!(
        "Artwork: {} requests, {} misses, {} completions, {} rasterized pixels; peak RSS: {:?} KiB; {} ms total",
        report.artwork.requests,
        report.artwork.misses,
        report.artwork.completions,
        report.artwork.pixels,
        report.rss_peak_kib,
        report.elapsed_ms
    );
    if update {
        std::fs::create_dir_all(root.join("baseline"))?;
        for name in SCREENSHOTS {
            std::fs::copy(
                root.join("current").join(format!("{name}.png")),
                root.join("baseline").join(format!("{name}.png")),
            )?;
        }
        files::write_atomically(&root.join("baseline.json"), &json)?;
        println!("Baseline updated: {}", root.join("baseline.json").display());
    }
    Ok(())
}

fn compare_screenshots(root: &Path) -> anyhow::Result<()> {
    for name in SCREENSHOTS {
        let previous = image::open(root.join("baseline").join(format!("{name}.png")))?.into_rgb8();
        let current = image::open(root.join("current").join(format!("{name}.png")))?.into_rgb8();
        anyhow::ensure!(
            previous.dimensions() == current.dimensions(),
            "{name} dimensions changed"
        );
        let delta_max = previous
            .as_raw()
            .iter()
            .zip(current.as_raw())
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .context("empty screenshot")?;
        anyhow::ensure!(
            delta_max <= 2,
            "{name} appearance changed (channel delta {delta_max}); inspect current and baseline screenshots"
        );
    }
    Ok(())
}

fn frame_times(report: &Report, scenario: &str, batch: Option<u32>) -> Vec<u64> {
    let mut times = report
        .samples
        .iter()
        .filter(|sample| {
            sample.scenario == scenario
                && sample.dirty_pixels > 0
                && batch.is_none_or(|batch| sample.batch == batch)
        })
        .map(|sample| sample.update_ns + sample.render_ns)
        .collect::<Vec<_>>();
    times.sort_unstable();
    times
}

fn percentile(times: &[u64], percent: usize) -> u64 {
    times
        .get((times.len().saturating_sub(1) * percent) / 100)
        .copied()
        .unwrap_or(0)
}

#[expect(
    clippy::cast_precision_loss,
    reason = "displaying nanosecond samples in milliseconds"
)]
fn print_scenario(scenario: &str, report: &Report, baseline: Option<&Report>) {
    let times = frame_times(report, scenario, None);
    let p50 = percentile(&times, 50) as f64 / 1_000_000.0;
    let p95 = percentile(&times, 95) as f64 / 1_000_000.0;
    let over_120 = times.iter().filter(|&&time| time > 8_333_333).count();
    let over_60 = times.iter().filter(|&&time| time > 16_666_667).count();
    let previous = baseline
        .map(|baseline| {
            let times = frame_times(baseline, scenario, None);
            format!(
                " (baseline p50 {:.3}, p95 {:.3} ms)",
                percentile(&times, 50) as f64 / 1_000_000.0,
                percentile(&times, 95) as f64 / 1_000_000.0
            )
        })
        .unwrap_or_default();
    let medians = (0..report.compatibility.batches)
        .map(|batch| {
            let times = frame_times(report, scenario, Some(batch));
            percentile(&times, 50) as f64 / 1_000_000.0
        })
        .collect::<Vec<_>>();
    let low = medians.iter().copied().reduce(f64::min).unwrap_or(0.0);
    let high = medians.iter().copied().reduce(f64::max).unwrap_or(0.0);
    println!(
        "{scenario}: {} rendered frames, p50 {p50:.3}, p95 {p95:.3} ms; >120 Hz {over_120}, >60 Hz {over_60}{previous}",
        times.len()
    );
    if scenario != "cold" {
        println!("  batch medians {low:.3}–{high:.3} ms");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_baseline_requires_explicit_update_before_measurement() {
        let directory = tempfile::tempdir().unwrap();
        let compatibility = Compatibility::current();
        assert!(baseline(directory.path(), &compatibility, false).is_err());
        assert!(
            baseline(directory.path(), &compatibility, true)
                .unwrap()
                .is_none()
        );
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }

    #[test]
    fn an_incompatible_runner_is_not_compared_or_overwritten() {
        let directory = tempfile::tempdir().unwrap();
        let mut compatibility = Compatibility::current();
        compatibility.width += 1;
        let report = Report {
            compatibility,
            revision: String::new(),
            elapsed_ms: 0,
            rss_peak_kib: None,
            artwork: Counters::default(),
            samples: Vec::new(),
        };
        let original = serde_json::to_string(&report).unwrap();
        let path = directory.path().join("baseline.json");
        std::fs::write(&path, &original).unwrap();
        assert!(baseline(directory.path(), &Compatibility::current(), false).is_err());
        assert!(
            baseline(directory.path(), &Compatibility::current(), true)
                .unwrap()
                .is_none()
        );
        assert_eq!(std::fs::read_to_string(path).unwrap(), original);
    }
}
