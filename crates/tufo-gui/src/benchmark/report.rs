use std::{
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use tufo_client::desktop::files;

use super::{BATCHES, Configuration, FRAME_STEP_NS};
use crate::backgrounds::metrics::Counters;

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
    #[serde(default = "scale_default")]
    scale_percent: u32,
    frame_step_ns: u64,
    batches: u32,
}

impl Compatibility {
    pub(super) fn current(configuration: &Configuration) -> Self {
        Self {
            schema: 1,
            workload: if configuration.tours { 2 } else { 1 },
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
            width: configuration.width,
            height: configuration.height,
            scale_percent: configuration.scale_percent,
            frame_step_ns: FRAME_STEP_NS,
            batches: BATCHES,
        }
    }

    fn screenshots(&self) -> &[&str] {
        if self.workload == 2 {
            super::tours::SCREENSHOTS
        } else {
            &SCREENSHOTS
        }
    }
}

fn scale_default() -> u32 {
    100
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

pub(super) fn directory(configuration: &Configuration) -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.artifacts/benchmarks/gui");
    if configuration.tours {
        root.join(format!(
            "tours/{}x{}-{}pct",
            configuration.width, configuration.height, configuration.scale_percent
        ))
    } else if configuration.width != super::platform::WIDTH
        || configuration.height != super::platform::HEIGHT
        || configuration.scale_percent != 100
    {
        root.join(format!(
            "{}x{}-{}pct",
            configuration.width, configuration.height, configuration.scale_percent
        ))
    } else {
        root
    }
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
            "no GUI baseline; rerun this command with --update first"
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
        compare_screenshots(
            root,
            report.compatibility.screenshots(),
            report.compatibility.workload == 2,
        )?;
    }
    let json = serde_json::to_string_pretty(report)?;
    files::write_atomically(&root.join("current.json"), &json)?;
    let scenarios: &[&str] = if report.compatibility.workload == 2 {
        &[
            "tour-open",
            "tour-step",
            "tour-reverse",
            "tour-close",
            "tour-idle",
            "tour-live",
        ]
    } else {
        &["cold", "sidebar", "settings", "pages"]
    };
    for scenario in scenarios {
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
        for name in report.compatibility.screenshots() {
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

fn compare_screenshots(root: &Path, screenshots: &[&str], tours: bool) -> anyhow::Result<()> {
    for name in screenshots {
        let previous = image::open(root.join("baseline").join(format!("{name}.png")))?.into_rgb8();
        let current = image::open(root.join("current").join(format!("{name}.png")))?.into_rgb8();
        anyhow::ensure!(
            previous.dimensions() == current.dimensions(),
            "{name} dimensions changed"
        );
        let reference = if tours {
            let image =
                image::open(root.join("current").join(format!("{name}.full.png")))?.into_rgb8();
            anyhow::ensure!(
                image.dimensions() == current.dimensions(),
                "{name} full redraw dimensions changed"
            );
            Some(image)
        } else {
            None
        };
        // Timer rounding can shift an animated pose by a frame; check its redraw at the same pose.
        let moving = tours && *name == "home-moving";
        let comparison = if moving {
            reference
                .as_ref()
                .context("missing moving-frame reference")?
        } else {
            &previous
        };
        let delta_max = comparison
            .as_raw()
            .iter()
            .zip(current.as_raw())
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .context("empty screenshot")?;
        let mut outliers = 0;
        let mut corrected = 0;
        let mut corrected_row = None;
        let mut corrected_rows_match = true;
        for (index, (a, b)) in comparison.pixels().zip(current.pixels()).enumerate() {
            if a.0.iter().zip(b.0).all(|(a, b)| a.abs_diff(b) <= 2) {
                continue;
            }
            if reference.as_ref().is_some_and(|image| {
                b.0.iter()
                    .zip(&image.as_raw()[index * 3..index * 3 + 3])
                    .all(|(a, b)| a.abs_diff(*b) <= 2)
            }) {
                let row = index / current.width() as usize;
                corrected_rows_match &= corrected_row.is_none_or(|previous| previous == row);
                corrected_row = Some(row);
                corrected += 1;
            } else {
                outliers += 1;
            }
        }
        // Fractional rounded borders can leave individual pixels in software partial redraws.
        anyhow::ensure!(
            outliers <= if tours && !moving { 3 } else { 0 }
                && corrected_rows_match
                && corrected <= current.width() as usize,
            "{name} appearance changed ({outliers} pixels, channel delta {delta_max}); inspect current and baseline screenshots"
        );
        if moving {
            println!("{name}: checked against the full redraw at the current animation pose");
        }
        println!(
            "{name}: {outliers} pixel exceptions, {corrected} corrected trail pixels; maximum delta {delta_max}"
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
    fn tour_comparisons_accept_corrected_trails_and_reject_new_or_wider_changes() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        std::fs::create_dir(root.join("baseline")).unwrap();
        std::fs::create_dir(root.join("current")).unwrap();
        let clean = image::RgbImage::from_pixel(8, 4, image::Rgb([0, 0, 0]));
        let mut trail = clean.clone();
        for x in 0..8 {
            trail.put_pixel(x, 1, image::Rgb([20, 0, 0]));
        }
        trail.save(root.join("baseline/tour.png")).unwrap();
        clean.save(root.join("current/tour.png")).unwrap();
        clean.save(root.join("current/tour.full.png")).unwrap();
        assert!(compare_screenshots(root, &["tour"], true).is_ok());
        assert!(compare_screenshots(root, &["tour"], false).is_err());

        clean.save(root.join("baseline/tour.png")).unwrap();
        trail.save(root.join("current/tour.png")).unwrap();
        assert!(compare_screenshots(root, &["tour"], true).is_err());

        trail.save(root.join("baseline/home-moving.png")).unwrap();
        clean.save(root.join("current/home-moving.png")).unwrap();
        clean
            .save(root.join("current/home-moving.full.png"))
            .unwrap();
        assert!(compare_screenshots(root, &["home-moving"], true).is_ok());
        trail.save(root.join("current/home-moving.png")).unwrap();
        assert!(compare_screenshots(root, &["home-moving"], true).is_err());

        for x in 0..8 {
            trail.put_pixel(x, 2, image::Rgb([20, 0, 0]));
        }
        trail.save(root.join("baseline/tour.png")).unwrap();
        clean.save(root.join("current/tour.png")).unwrap();
        assert!(compare_screenshots(root, &["tour"], true).is_err());
    }

    #[test]
    fn a_missing_baseline_requires_explicit_update_before_measurement() {
        let directory = tempfile::tempdir().unwrap();
        let compatibility = Compatibility::current(&Configuration::default());
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
        let mut compatibility = Compatibility::current(&Configuration::default());
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
        assert!(
            baseline(
                directory.path(),
                &Compatibility::current(&Configuration::default()),
                false
            )
            .is_err()
        );
        assert!(
            baseline(
                directory.path(),
                &Compatibility::current(&Configuration::default()),
                true
            )
            .unwrap()
            .is_none()
        );
        assert_eq!(std::fs::read_to_string(path).unwrap(), original);
    }
}
