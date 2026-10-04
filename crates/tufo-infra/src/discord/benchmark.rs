//! Offline measurement of the production voice source and Songbird packet builder.

use std::{
    net::UdpSocket,
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context as _, ensure};
use kira::{
    Frame,
    sound::static_sound::{StaticSoundData, StaticSoundSettings},
};
use serde::{Deserialize, Serialize};
use songbird::{
    driver::{
        DummyMixer,
        bench_internals::{mixer, track_context},
    },
    tracks::Track,
};
use tufo_models::library::Volume;

use super::{DISCORD_SAMPLE_RATE, PacingTotals, voice_input};
use crate::audio::Mixer;

const RUNS: usize = 5;
const PACKETS: u32 = 200;
const TICK: Duration = Duration::from_millis(20);
const STARTUP_TICKS_MAX: u32 = 100;

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Configuration {
    schema: u32,
    workload: String,
    platform: String,
    profile: String,
    compiler: String,
    cpu_model: Option<String>,
    cpu_affinity: Option<String>,
    dependencies: String,
    runs: usize,
    packets: u32,
}

#[derive(Serialize, Deserialize)]
struct Report {
    configuration: Configuration,
    runs: Vec<Run>,
    peak_rss_kib: Option<u64>,
}

#[derive(Serialize, Deserialize)]
struct Run {
    startup_us: u64,
    startup_empty_ticks: u32,
    packet_build_us: Vec<u64>,
    loopback_us: Vec<u64>,
    packet_bytes: Vec<usize>,
    render_ticks: u64,
    lateness_max_us: u64,
}

struct Fixture {
    _mixer: Mixer,
    driver: DummyMixer,
    totals: Arc<PacingTotals>,
    sender: UdpSocket,
    receiver: UdpSocket,
    packet: [u8; 1460],
    received: [u8; 1460],
    input_prepare_us: u64,
}

impl Fixture {
    fn new(runtime: &tokio::runtime::Runtime) -> anyhow::Result<Self> {
        let (mut mixer, renderer) = Mixer::open_pulled(DISCORD_SAMPLE_RATE, Volume::default())?;
        for frequency in [440.0, 660.0, 880.0] {
            mixer.play_decoded_sound(tone(frequency))?;
        }
        let driver = mixer::Mixer::mock(runtime.handle().clone(), true);
        let totals = Arc::new(PacingTotals::default());
        let started = Instant::now();
        let input = voice_input(renderer, Arc::default(), Arc::clone(&totals))?;
        let input_prepare_us = micros(started.elapsed());
        let (_, track) = track_context(Track::from(input));
        let mut fixture = Self {
            _mixer: mixer,
            driver,
            totals,
            sender: UdpSocket::bind("127.0.0.1:0")?,
            receiver: UdpSocket::bind("127.0.0.1:0")?,
            packet: [0; 1460],
            received: [0; 1460],
            input_prepare_us,
        };
        fixture.sender.connect(fixture.receiver.local_addr()?)?;
        fixture
            .receiver
            .set_read_timeout(Some(Duration::from_secs(1)))?;
        fixture
            .sender
            .set_write_timeout(Some(Duration::from_secs(1)))?;
        fixture.packet[0] = 0x80;
        fixture.packet[1] = 0x78;
        fixture
            .driver
            .0
            .add_track(track)
            .map_err(|error| anyhow::anyhow!("adding the voice track: {error:?}"))?;
        Ok(fixture)
    }

    fn packet(&mut self, sequence: u32) -> anyhow::Result<(usize, u64)> {
        self.packet[2..4].copy_from_slice(&u16::try_from(sequence)?.to_be_bytes());
        self.packet[4..8].copy_from_slice(&(sequence * 960).to_be_bytes());
        let started = Instant::now();
        let length = self
            .driver
            .0
            .mix_and_build_packet(&mut self.packet)
            .map_err(|error| anyhow::anyhow!("building a voice packet: {error:?}"))?;
        Ok((length, micros(started.elapsed())))
    }

    fn receive(&mut self, length: usize) -> anyhow::Result<u64> {
        let started = Instant::now();
        ensure!(
            self.sender.send(&self.packet[..length])? == length,
            "short UDP send"
        );
        let received = self.receiver.recv(&mut self.received)?;
        let elapsed = micros(started.elapsed());
        ensure!(
            self.received[..received] == self.packet[..length],
            "packet changed in transit"
        );
        Ok(elapsed)
    }

    fn measure(&mut self) -> anyhow::Result<Run> {
        let started = Instant::now();
        let startup_empty_ticks = self.startup(started)?;
        let startup_us = self.input_prepare_us + micros(started.elapsed());
        let mut result = Run {
            startup_us,
            startup_empty_ticks,
            packet_build_us: Vec::with_capacity(PACKETS as usize),
            loopback_us: Vec::with_capacity(PACKETS as usize),
            packet_bytes: Vec::with_capacity(PACKETS as usize),
            render_ticks: 0,
            lateness_max_us: 0,
        };
        let started = Instant::now();
        for sequence in 1..=PACKETS {
            wait_until(started + TICK * sequence);
            let (length, elapsed) = self.packet(sequence)?;
            ensure!(length > 0, "voice stopped producing packets");
            result.packet_build_us.push(elapsed);
            result.loopback_us.push(self.receive(length)?);
            result.packet_bytes.push(length);
        }
        let pacing = self.totals.summary();
        result.render_ticks = pacing.ticks;
        result.lateness_max_us = micros(pacing.lateness_max);
        ensure!(
            pacing.ticks == u64::from(PACKETS) + 1,
            "voice rendered ahead or lost ticks"
        );
        Ok(result)
    }

    fn startup(&mut self, started: Instant) -> anyhow::Result<u32> {
        for tick in 0..STARTUP_TICKS_MAX {
            wait_until(started + TICK * tick);
            let (length, _) = self.packet(0)?;
            if length > 0 {
                self.receive(length)?;
                return Ok(tick);
            }
        }
        anyhow::bail!("voice source did not become playable within two seconds")
    }
}

fn tone(frequency: f64) -> StaticSoundData {
    let frames = (0..DISCORD_SAMPLE_RATE)
        .map(|index| {
            let phase = std::f64::consts::TAU * frequency * f64::from(index)
                / f64::from(DISCORD_SAMPLE_RATE);
            #[expect(
                clippy::cast_possible_truncation,
                reason = "the synthetic tone is bounded to 0.08"
            )]
            let sample = (phase.sin() * 0.08) as f32;
            Frame::new(sample, -sample)
        })
        .collect::<Vec<_>>()
        .into();
    StaticSoundData {
        sample_rate: DISCORD_SAMPLE_RATE,
        frames,
        settings: StaticSoundSettings::default().loop_region(..),
        slice: None,
    }
}

fn wait_until(deadline: Instant) {
    thread::sleep(deadline.saturating_duration_since(Instant::now()));
}

fn micros(duration: Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

fn percentile(mut values: Vec<u64>, percent: usize) -> u64 {
    values.sort_unstable();
    values[(values.len() * percent).div_ceil(100).saturating_sub(1)]
}

fn print_report(label: &str, report: &Report) {
    let startup = report.runs.iter().map(|run| run.startup_us).collect();
    let builds: Vec<_> = report
        .runs
        .iter()
        .flat_map(|run| run.packet_build_us.iter().copied())
        .collect();
    let loopback = report
        .runs
        .iter()
        .flat_map(|run| run.loopback_us.iter().copied())
        .collect();
    println!(
        "{label}: startup p50={} us; packet build p50={} us p95={} us; loopback p50={} us; peak RSS={:?} KiB",
        percentile(startup, 50),
        percentile(builds.clone(), 50),
        percentile(builds, 95),
        percentile(loopback, 50),
        report.peak_rss_kib
    );
}

fn write_report(path: &Path, report: &Report) -> anyhow::Result<()> {
    let mut file = tempfile::NamedTempFile::new_in(path.parent().context("report has no parent")?)?;
    std::io::Write::write_all(&mut file, &serde_json::to_vec_pretty(report)?)?;
    file.persist(path)?;
    Ok(())
}

fn peak_rss_kib() -> Option<u64> {
    proc_field("/proc/self/status", "VmHWM:")?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

fn proc_field(path: &str, field: &str) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()?
        .lines()
        .find_map(|line| Some(line.strip_prefix(field)?.trim().to_owned()))
}

#[test]
fn voice_starts_on_the_first_packet_tick() -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let mut fixture = Fixture::new(&runtime)?;
    assert_eq!(fixture.totals.summary().ticks, 0);

    let (length, _) = fixture.packet(0)?;

    assert!(length > 0, "the first tick must carry the mixer audio");
    assert_eq!(fixture.totals.summary().ticks, 1);
    fixture.receive(length)?;
    Ok(())
}

#[test]
fn voice_packets_preserve_samples_and_apply_controls_on_the_next_tick() -> anyhow::Result<()> {
    use songbird::input::{Input, core::audio::SampleBuffer};

    let sound = tone(440.0);
    let (mut mixer, renderer) = Mixer::open_pulled(DISCORD_SAMPLE_RATE, Volume::default())?;
    mixer.play_decoded_sound(sound.clone())?;
    let (mut reference, renderer_reference) =
        Mixer::open_pulled(DISCORD_SAMPLE_RATE, Volume::default())?;
    reference.play_decoded_sound(sound)?;
    let mut renderer_reference = renderer_reference.recv_timeout(Duration::from_secs(1))?;
    let totals = Arc::new(PacingTotals::default());
    let input = voice_input(renderer, Arc::default(), Arc::clone(&totals))?;
    assert_eq!(totals.summary().ticks, 0, "preparing must not render audio");
    let Input::Live(mut live, _) = input else {
        anyhow::bail!("the voice source must be live");
    };
    let parsed = live
        .parsed_mut()
        .context("the voice source must be ready")?;
    let mut expected = [0.0; 1920];
    for tick in 0..8 {
        for mixer in [&mut mixer, &mut reference] {
            match tick {
                1 | 5 => mixer.pause_sounds(Duration::ZERO),
                2 | 6 => mixer.resume_sounds(Duration::ZERO),
                3 => mixer.set_effects_volume(0.5),
                _ => {}
            }
        }
        renderer_reference.on_start_processing();
        renderer_reference.process(&mut expected, 2);
        let packet = parsed.format.next_packet()?;
        assert_eq!(packet.ts, tick * 960);
        assert_eq!(packet.dur, 960);
        let decoded = parsed.decoder.decode(&packet)?;
        let mut samples = SampleBuffer::<f32>::new(960, *decoded.spec());
        samples.copy_interleaved_ref(decoded);
        assert_eq!(
            samples.samples(),
            expected,
            "mixer samples changed at tick {tick}"
        );
        if matches!(tick, 1 | 5) {
            assert!(
                samples
                    .samples()
                    .iter()
                    .all(|sample| sample.abs() < f32::EPSILON)
            );
        } else {
            assert!(samples.samples().iter().any(|sample| sample.abs() > 0.001));
        }
        assert_eq!(
            totals.summary().ticks,
            tick + 1,
            "the source rendered ahead"
        );
    }
    Ok(())
}

#[test]
#[ignore = "paced release benchmark; run just discord-bench"]
fn run() -> anyhow::Result<()> {
    ensure!(
        !cfg!(debug_assertions),
        "run this benchmark in release mode"
    );
    let update = match std::env::var("TUFO_DISCORD_BENCH_MODE").as_deref() {
        Ok("--update") => true,
        Ok("") | Err(std::env::VarError::NotPresent) => false,
        _ => anyhow::bail!("expected --update or no benchmark argument"),
    };
    let configuration = Configuration {
        schema: 1,
        workload: "three synthetic looping stereo tones; production voice input; songbird-0.6.0 mock mixer, Opus and AES-GCM; loopback UDP; 20ms ticks".to_owned(),
        platform: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        profile: "release".to_owned(),
        compiler: String::from_utf8(std::process::Command::new("rustc").arg("--version").output()?.stdout)?,
        cpu_model: proc_field("/proc/cpuinfo", "model name\t:"),
        cpu_affinity: proc_field("/proc/self/status", "Cpus_allowed_list:"),
        dependencies: blake3::hash(include_bytes!("../../../../Cargo.lock")).to_hex().to_string(),
        runs: RUNS,
        packets: PACKETS,
    };
    let directory =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.artifacts/benchmarks/discord");
    let baseline_path = directory.join("baseline.json");
    let baseline: Option<Report> = match std::fs::read(&baseline_path) {
        Ok(bytes) => Some(serde_json::from_slice(&bytes)?),
        Err(error) if update && error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(error).context("capture a baseline with just discord-bench --update");
        }
    };
    if let Some(baseline) = &baseline {
        ensure!(
            baseline.configuration == configuration,
            "benchmark baseline is incompatible"
        );
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    // The source promotes its puller; keep report I/O off that real-time thread.
    let runs = thread::scope(|scope| {
        scope
            .spawn(|| {
                (0..RUNS)
                    .map(|_| Fixture::new(&runtime)?.measure())
                    .collect::<anyhow::Result<Vec<_>>>()
            })
            .join()
            .map_err(|_| anyhow::anyhow!("benchmark worker panicked"))
    })??;
    let report = Report {
        configuration,
        runs,
        peak_rss_kib: peak_rss_kib(),
    };
    if let Some(baseline) = &baseline {
        print_report("baseline", baseline);
    }
    print_report("current", &report);
    std::fs::create_dir_all(&directory)?;
    write_report(&directory.join("current.json"), &report)?;
    if update {
        write_report(&baseline_path, &report)?;
    }
    Ok(())
}
