#!/usr/bin/env python3
"""Capture isolated local playback under bounded load, retaining continuity and scheduling evidence."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import statistics
import subprocess
import sys
import threading
import time
import traceback

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "audio-recovery"))
from check import Session, wait_for  # noqa: E402
from waveform import RATE, SIGNAL_SECONDS, continuity_report, signal_samples, silence_report, write_stereo  # noqa: E402

SCHEMA_VERSION = 1
WORKLOAD_VERSION = 1
QUANTUM = 512
CONTROL_P99_MS_MAX = 250
CONTROL_MS_MAX = 1000


def write_json(path, value):
    temporary = path.with_name(f".{path.name}.{os.getpid()}.tmp")
    temporary.write_text(json.dumps(value, indent=2) + "\n")
    temporary.replace(path)


def digest(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def percentile(samples, fraction):
    return sorted(samples)[max(0, min(len(samples) - 1, round(fraction * (len(samples) - 1))))] if samples else None


def memory_and_faults(pid):
    result = {"threads": []}
    try:
        for line in Path(f"/proc/{pid}/status").read_text().splitlines():
            if line.startswith(("VmRSS:", "VmHWM:", "VmLck:")):
                field, value, _ = line.split()
                result[{"VmRSS:": "rss_kib", "VmHWM:": "peak_rss_kib", "VmLck:": "locked_kib"}[field]] = int(value)
        for path in Path(f"/proc/{pid}/task").iterdir():
            name = (path / "comm").read_text().strip()
            if name.startswith(("pw_out", "pw-data-loop")):
                fields = (path / "stat").read_text().rsplit(")", 1)[1].split()
                result["threads"].append({"tid": int(path.name), "name": name, "start_ticks": int(fields[19]),
                                          "minor_faults": int(fields[7]), "major_faults": int(fields[9])})
    except FileNotFoundError:
        result["process_exited"] = True
    return result


def fault_deltas(samples):
    threads = {}
    for sample in samples:
        for thread in sample["resources"]["threads"]:
            key = thread["tid"], thread["start_ticks"]
            entry = threads.setdefault(key, {"tid": thread["tid"], "name": thread["name"], "first": thread, "last": thread})
            entry["last"] = thread
    return [{"tid": value["tid"], "name": value["name"],
             "minor_faults": value["last"]["minor_faults"] - value["first"]["minor_faults"],
             "major_faults": value["last"]["major_faults"] - value["first"]["major_faults"]}
            for value in threads.values()]


def summarize_diagnostics(samples):
    callbacks = [sample["diagnostics"]["local_callbacks"] for sample in samples
                 if sample["diagnostics"].get("local_callbacks") is not None]
    memory = [sample["diagnostics"]["audio_memory"] for sample in samples
              if sample["diagnostics"].get("audio_memory") is not None]
    summary = {"local_callbacks": None, "audio_memory": None}
    if callbacks:
        first, last = callbacks[0], callbacks[-1]
        leads = [sample["playback_lead_ms"] for sample in callbacks if sample.get("playback_lead_ms") is not None]
        rate = last["sample_rate"]
        summary["local_callbacks"] = {
            "histogram_scope": "cumulative since server diagnostics session start; percentiles are bucket upper bounds",
            "session_started_at_unix_ms": last["session_started_at_unix_ms"],
            "sampled_at_ms_first": first["sampled_at_ms"], "sampled_at_ms_last": last["sampled_at_ms"],
            "callbacks_during_samples": last["callbacks"] - first["callbacks"],
            "late_callbacks_during_samples": last["late_callbacks"] - first["late_callbacks"],
            "render_over_budget_during_samples": last["render_over_budget"] - first["render_over_budget"],
            "streams_created_total": last["streams_created"], "latest_stream_id": last["latest_stream_id"],
            "sample_rate_last": rate, "callback_frames_last": last["callback_frames_last"],
            "callback_frames_min_total": last["callback_frames_min"], "callback_frames_max_total": last["callback_frames_max"],
            "callback_period_ms_last": last["callback_frames_last"] * 1000 / rate if rate else None,
            **{field: last[field] for field in ["callback_gap_p99_ms", "callback_gap_max_ms", "render_duration_p99_ms",
                                               "render_duration_max_ms", "first_anomaly_at_ms", "latest_anomaly_at_ms"]},
            "playback_lead_ms_min_observed": min(leads, default=None),
            "playback_lead_ms_max_observed": max(leads, default=None),
            "playback_lead_ms_last": last.get("playback_lead_ms"),
            "interpretation": "cadence/render observations, not xrun counts; playback lead is not decoded-source headroom",
        }
    if memory:
        first, last = memory[0], memory[-1]
        summary["audio_memory"] = {
            "bytes_used_peak_observed": max(sample["bytes_used"] for sample in memory),
            "bytes_used_last": last["bytes_used"], "bytes_max": last["bytes_max"],
            "budget_headroom_bytes_min_observed": min(sample["bytes_max"] - sample["bytes_used"] for sample in memory),
            "budget_headroom_bytes_last": last["bytes_max"] - last["bytes_used"],
            "locked_bytes_peak_observed": max(sample["locked_bytes"] for sample in memory),
            "locked_bytes_last": last["locked_bytes"], "locking_supported": last["locking_supported"],
            "lock_failures_total": last["lock_failures"],
            "lock_failures_during_samples": last["lock_failures"] - first["lock_failures"],
            "lock_failed_bytes_total": last["lock_failed_bytes"],
            "lock_failed_bytes_during_samples": last["lock_failed_bytes"] - first["lock_failed_bytes"],
        }
    return summary


class StabilitySession(Session):
    def __init__(self, binaries, artifacts):
        super().__init__(binaries, artifacts)
        for name in ["PIPEWIRE_PROPS", "PIPEWIRE_CONFIG_DIR", "PIPEWIRE_CONFIG_PREFIX"]:
            self.env.pop(name, None)
        # The baseline recovery helper retains its profile too; list only the isolated settings.
        write_json(artifacts / "profile.json", {"root": str(self.root), "environment": {
            name: self.env[name] for name in ["XDG_RUNTIME_DIR", "PIPEWIRE_RUNTIME_DIR", "PIPEWIRE_REMOTE",
                                              "PIPEWIRE_NODE", "TUFO_RUNTIME_DIR", "TUFO_DATABASE_PATH"]}})
        alsa = self.root / "alsa-null.conf"
        alsa.write_text("pcm.!default { type null }\n")
        self.env["ALSA_CONFIG_PATH"] = str(alsa)
        self.pipewire_config.write_text(self.pipewire_config.read_text().replace(
            "default.clock.quantum = 1024", "default.clock.quantum = 512 default.clock.min-quantum = 32 default.clock.max-quantum = 8192"))
        self.top_samples = []
        self.top_lock = threading.Lock()
        self.top_thread = None
        self.load_units = set()
        self.load_env = dict(self.env)
        self.load_env.update({"XDG_RUNTIME_DIR": f"/run/user/{os.getuid()}",
                              "DBUS_SESSION_BUS_ADDRESS": f"unix:path=/run/user/{os.getuid()}/bus"})

    def start_audio(self, rate=RATE):
        super().start_audio(rate)
        self.stop(self.children[-1])
        self.top = self.start("pw-top-timestamped", "stdbuf", "-oL", "pw-top", "-b", "-n", "100000", stdout=subprocess.PIPE)
        def read_top():
            with (self.artifacts / "pw-top.jsonl").open("w") as samples, (self.artifacts / "pw-top.txt").open("w") as raw:
                for line in self.top.stdout:
                    now = time.monotonic_ns()
                    raw.write(f"{now} {line}")
                    raw.flush()
                    fields = re.sub(r"\x1b\[[0-9;]*[A-Za-z]", "", line).split()
                    if len(fields) > 8 and len(fields[0]) == 1 and fields[1].isdigit() and fields[8].isdigit():
                        item = {"at_monotonic_ns": now, "node_id": int(fields[1]), "errors": int(fields[8]), "row": fields}
                        with self.top_lock:
                            self.top_samples.append(item)
                        samples.write(json.dumps(item) + "\n")
                        samples.flush()
        self.top_thread = threading.Thread(target=read_top, daemon=True)
        self.top_thread.start()

    def sink(self, name="tufo-test-sink"):
        self.run("pw-cli", "create-node", "adapter", "{ factory.name=support.null-audio-sink node.name=" + name
                 + f" audio.rate={self.audio_rate} node.force-quantum={QUANTUM}"
                 + " media.class=Audio/Sink object.linger=true audio.position=[FL FR] }")
        wait_for(self.graph, lambda graph: any(item.get("info", {}).get("props", {}).get("node.name") == name for item in graph), "null sink")

    def xrun_report(self, started, ended):
        with self.top_lock:
            prior = {}
            samples = []
            for sample in self.top_samples:
                if sample["at_monotonic_ns"] < started:
                    prior[sample["node_id"]] = sample
                elif sample["at_monotonic_ns"] <= ended:
                    samples.append(sample)
            samples = list(prior.values()) + samples
        nodes = {}
        for sample in samples:
            entry = nodes.setdefault(sample["node_id"], {"node_id": sample["node_id"], "first_errors": sample["errors"],
                                                        "last_errors": sample["errors"], "samples": 0, "row": sample["row"],
                                                        "quantums_seen": []})
            entry["last_errors"] = max(entry["last_errors"], sample["errors"])
            entry["samples"] += 1
            entry["row"] = sample["row"]
            quantum = int(sample["row"][2])
            if quantum and quantum not in entry["quantums_seen"]:
                entry["quantums_seen"].append(quantum)
        for entry in nodes.values():
            entry["additional_errors"] = entry["last_errors"] - entry["first_errors"]
        return list(nodes.values())

    def close(self):
        for unit in list(self.load_units):
            self.stop_load_unit(unit)
        super().close()
        if self.top_thread:
            self.top_thread.join(timeout=3)
        logs = self.root / "state/tufo"
        if logs.exists():
            shutil.copytree(logs, self.artifacts / "server-logs", dirs_exist_ok=True)

    def stop_load_unit(self, unit):
        subprocess.run(["systemctl", "--user", "stop", unit], env=self.load_env,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=10, check=False)
        self.load_units.discard(unit)


def prepare(session):
    reference = signal_samples()
    music = session.root / "synthetic/music"
    effects = session.root / "synthetic/effects"
    for name, frequency in [("first", 220), ("second", 330), ("third", 550)]:
        write_stereo(music / f"{name}.wav", 40, frequency=frequency)
    write_stereo(effects / "continuity.wav", SIGNAL_SECONDS, reference=reference)
    write_stereo(effects / "one-shot.wav", 21, frequency=880)
    for directory, kind in [(music, "playlist"), (effects, "soundboard")]:
        session.cli("library", "import", json.dumps({"kind": kind, "paths": [str(directory)]}))
    library = wait_for(lambda: session.cli("library", "snapshot"),
                       lambda snapshot: any(Path(entry["path"]).name == "Rain.ogg"
                                            for collection in snapshot["collections"] for entry in collection["entries"]),
                       "built-in Rain seeded")
    entries = {Path(entry["path"]).stem: entry for collection in library["collections"] for entry in collection["entries"]}
    playlist = next(collection for collection in library["collections"]
                    if any(entry["path"] == str(music / "first.wav") for entry in collection["entries"]))
    for name in ["Rain", "continuity"]:
        session.cli("entry", "update", entries[name]["id"], '{"sound":{"looping":true,"volume":1}}')
    for name in ["second", "third"]:
        entry = entries[name]
        waveform = session.cli("loop", "waveform", entry["audio_file_id"])
        session.cli("loop", "update", entry["audio_file_id"], json.dumps({"source_fingerprint": waveform["fingerprint"],
                    "playback_start_frame": RATE, "loop_start_frame": 2 * RATE, "loop_end_frame": 8 * RATE,
                    "crossfade_frames": RATE // 5}))
    session.cli("player", "settings", "update", '{"volume":0.4,"effects_volume":1,"repeat":"playlist","crossfade_ms":700}')
    write_json(session.artifacts / "fixtures.json", {"version": WORKLOAD_VERSION, "seed": "0x7475666f", "rate": RATE,
               "signal_seconds": SIGNAL_SECONDS, "files": [{"name": path.name, "sha256": digest(path)}
               for path in [Path(entries["Rain"]["path"]), *sorted((session.root / "synthetic").rglob("*.wav"))]]})
    return reference, entries, playlist


def start_load(session, args, seconds, phase):
    if args.no_load:
        return None, None
    unit = f"tufo-stability-{os.getpid()}-{phase}.scope"
    folder = session.root / f"load-{phase}"
    folder.mkdir()
    session.load_units.add(unit)
    process = session.start(f"load-{phase}", "systemd-run", "--user", "--scope", "--quiet", "--unit", unit,
                            "-p", "CPUQuota=200%", "-p", "MemoryMax=768M", "-p", "MemorySwapMax=0", "-p", "IOWeight=10",
                            "taskset", "-c", args.cpus, sys.executable, Path(__file__).with_name("load.py"),
                            "--seconds", str(seconds + 5), "--directory", folder, env=session.load_env)
    time.sleep(0.5)
    assert process.poll() is None, f"bounded load failed to start; inspect {phase} load log"
    return process, unit


def stop_load(session, process, unit):
    if unit:
        session.stop_load_unit(unit)
        session.stop(process)


def sample(session, server, capture, phase, samples, output):
    item = {"at_monotonic_ns": time.monotonic_ns(), "phase": phase,
            "resources": memory_and_faults(server.pid), "capture_resources": memory_and_faults(capture.pid),
            "pipewire_resources": memory_and_faults(session.daemon.pid), "diagnostics": session.cli("player", "diagnostics")}
    samples.append(item)
    output.write(json.dumps(item) + "\n")
    output.flush()


def run_phase(session, server, args, phase, seconds, entries, playlist):
    if phase == "rain":
        play = lambda: session.cli("effect", "play", entries["Rain"]["id"])
    else:
        session.cli("effect", "stop", entries["Rain"]["id"])
        session.cli("music", "play", "collection", playlist["id"])
        for name in ["second", "third"]:
            session.cli("layer", "add", entries[name]["id"])
            session.cli("layer", "update", entries[name]["id"], '{"looping":true,"volume":0.4}')
        play = lambda: session.cli("effect", "play", entries["continuity"]["id"])
    preparation_started = time.monotonic()
    play()
    preparation_ms = (time.monotonic() - preparation_started) * 1000
    session.health("ready")
    env = dict(session.env)
    env.pop("PIPEWIRE_NODE", None)
    env.pop("PIPEWIRE_PROPS", None)
    env["PIPEWIRE_CONFIG_NAME"] = "client-rt.conf"
    path = session.artifacts / f"{phase}.wav"
    capture_started = time.monotonic_ns()
    capture = session.start(f"capture-{phase}", "pw-record", "--target", "tufo-test-sink", "--properties",
                            "{ stream.capture.sink = true }", "--rate", str(RATE), "--channels", "2", "--format", "s16", path, env=env)
    graph = session.graph()
    write_json(session.artifacts / f"{phase}-graph.json", graph)
    assert not any(item["type"].endswith(":Device") for item in graph), "private graph discovered hardware"
    clients = {item["id"] for item in graph if str(item.get("info", {}).get("props", {}).get("application.process.id")) == str(server.pid)}
    nodes = [item for item in graph if item["id"] in clients or item.get("info", {}).get("props", {}).get("client.id") in clients]
    output_nodes = [item for item in nodes if item.get("info", {}).get("props", {}).get("media.class") == "Stream/Output/Audio"]
    assert output_nodes, "server has no native PipeWire output node"
    for node in output_nodes:
        actual = str(node["info"]["props"].get("node.async", "false")).lower()
        assert actual == args.async_mode, f"requested node.async={args.async_mode}, observed {actual}"
    # Attaching the monitor reconfigures the graph. Its warmup remains in the capture.
    time.sleep(2)
    scheduling = session.run("ps", "-L", "-p", f"{server.pid},{capture.pid},{session.daemon.pid}",
                             "-o", "pid,tid,comm,cls,rtprio,pri,ni").stdout
    (session.artifacts / f"{phase}-threads.txt").write_text(scheduling)
    realtime_threads = [{"pid": int(row[0]), "tid": int(row[1]), "name": row[2], "class": row[3], "priority": row[4]}
                        for line in scheduling.splitlines()[1:] if (row := line.split())]
    samples = []
    controls = []
    process = unit = None
    started = time.monotonic_ns()
    ended = started
    capture_stop_requested = capture_exited = None
    try:
        process, unit = start_load(session, args, seconds, phase)
        deadline = time.monotonic_ns() + seconds * 1_000_000_000
        next_effect = next_skip = next_control = started
        next_sample = started
        with (session.artifacts / f"{phase}-samples.jsonl").open("w") as output:
            while time.monotonic_ns() < deadline:
                now = time.monotonic_ns()
                assert server.poll() is None, "server exited during playback"
                assert capture.poll() is None, "audio capture exited early"
                assert process is None or process.poll() is None, "bounded load exited before the measured phase ended"
                if now >= next_sample:
                    sample(session, server, capture, phase, samples, output)
                    next_sample = now + 1_000_000_000
                if now >= next_control:
                    control_started = time.monotonic_ns()
                    session.cli("player", "settings", "update", json.dumps({"volume": 0.4 if len(controls) % 2 else 0.3}))
                    controls.append({"at_monotonic_ns": control_started,
                                     "command": "settings.volume",
                                     "elapsed_ms": (time.monotonic_ns() - control_started) / 1_000_000})
                    next_control = now + 2_000_000_000
                if phase == "mixed" and now >= next_effect:
                    session.cli("effect", "play", entries["one-shot"]["id"])
                    next_effect = now + 10_000_000_000
                if phase == "mixed" and now >= next_skip:
                    session.cli("music", "next")
                    next_skip = now + 45_000_000_000
                time.sleep(0.05)
            sample(session, server, capture, phase, samples, output)
            ended = time.monotonic_ns()
    finally:
        stop_load(session, process, unit)
        capture_stop_requested = time.monotonic_ns()
        session.stop(capture)
        capture_exited = time.monotonic_ns()
    diagnostics = samples[-1]["diagnostics"]
    keys = ["music_underrun_frames", "sound_underrun_frames", "local_stream_errors", "local_stream_errors_discarded"]
    diagnostic_deltas = {key: diagnostics[key] - samples[0]["diagnostics"][key] for key in keys}
    # Startup also matters: do not hide starvation that occurred before the measured load interval.
    diagnostic_totals = {key: diagnostics[key] for key in keys}
    xruns = session.xrun_report(started, ended)
    output_ids = {node["id"] for node in output_nodes}
    sink_ids = {item["id"] for item in graph if item.get("info", {}).get("props", {}).get("node.name") == "tufo-test-sink"}
    server_xruns_seen = any(node["node_id"] in output_ids and node["samples"] >= 2 for node in xruns)
    quantum_seen = any(node["node_id"] in sink_ids and node["quantums_seen"] == [QUANTUM] for node in xruns)
    realtime = all(any(thread["pid"] == pid and thread["name"].startswith(name)
                       and thread["class"] in {"RR", "FF"} for thread in realtime_threads)
                   for pid, name in [(server.pid, "pw_out"), (server.pid, "pw-data-loop"),
                                     (capture.pid, "pw-data-loop"), (session.daemon.pid, "pw-data-loop")])
    latencies = [item["elapsed_ms"] for item in controls]
    control = {"samples": controls, "p99_ms": percentile(latencies, .99), "max_ms": max(latencies, default=0)}
    rss = [item["resources"].get("rss_kib", 0) for item in samples]
    settled = rss[len(rss) // 3:]
    rss_growth = max(0, statistics.median(settled[-min(10, len(settled)):]) - statistics.median(settled[:min(10, len(settled))]))
    result = {"seconds": seconds, "at_monotonic_ns": started, "ended_at_monotonic_ns": ended,
              "capture": path.name, "capture_process_started_at_monotonic_ns": capture_started,
              "capture_stop_requested_at_monotonic_ns": capture_stop_requested,
              "capture_process_exited_at_monotonic_ns": capture_exited,
              "capture_analysis_skip_frames": 2 * RATE,
              "capture_window": "waveform includes load startup and teardown outside the diagnostics/xrun interval",
              "preparation_ms": preparation_ms, "diagnostic_deltas": diagnostic_deltas,
              "diagnostics": diagnostics, "diagnostics_summary": summarize_diagnostics(samples),
              "xruns": xruns, "server_xruns_seen": server_xruns_seen,
              "controls": control, "thread_fault_deltas": fault_deltas(samples), "threads": realtime_threads,
              "rss_peak_kib": max(item["resources"].get("peak_rss_kib", 0) for item in samples),
              "locked_peak_kib": max(item["resources"].get("locked_kib", 0) for item in samples),
              "rss_settled_growth_kib": rss_growth,
              "checks": {"diagnostics": not any(diagnostic_totals.values()),
                         "xruns": server_xruns_seen and not any(node["additional_errors"] for node in xruns),
                         "driver_quantum": quantum_seen, "realtime_threads": realtime,
                         "control_latency": bool(latencies) and control["p99_ms"] <= CONTROL_P99_MS_MAX and control["max_ms"] < CONTROL_MS_MAX,
                         "memory_plateau": rss_growth <= max(16 * 1024, statistics.median(settled) * .05)}}
    write_json(session.artifacts / f"{phase}-controls.json", control)
    return result


def compare(baseline, report):
    return {phase: {field: {"baseline": baseline["phases"][phase].get(field), "current": current.get(field)}
                    for field in ["preparation_ms", "rss_peak_kib", "locked_peak_kib", "rss_settled_growth_kib",
                                  "thread_fault_deltas", "waveform", "controls", "xruns", "diagnostics",
                                  "diagnostics_summary", "checks"]}
            for phase, current in report["phases"].items() if phase in baseline.get("phases", {})}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binaries", type=Path, default=Path("target/release"))
    parser.add_argument("--build-profile", choices=["release", "debug"], default="release")
    parser.add_argument("--artifacts", type=Path, default=Path(".artifacts/audio-stability/current"))
    parser.add_argument("--duration-seconds", type=int, default=600)
    parser.add_argument("--phase", choices=["all", "rain", "mixed"], default="all")
    parser.add_argument("--async-mode", choices=["false", "true"], default="false")
    parser.add_argument("--cpus", default="2,3")
    parser.add_argument("--no-load", action="store_true", help="check fixture/capture wiring without stress-ng")
    parser.add_argument("--baseline", type=Path, help="compare a compatible report, including failed baseline runs")
    parser.add_argument("--update", action="store_true", help="atomically replace --baseline only after a fully passing run")
    args = parser.parse_args()
    if args.update and args.baseline is None:
        parser.error("--update requires --baseline PATH")
    if args.baseline and not args.baseline.exists() and not args.update:
        parser.error(f"baseline does not exist: {args.baseline}; use --update to create it after a passing run")
    assert 10 <= args.duration_seconds <= 3600, "duration must be between ten seconds and one hour"
    cpus = {int(value) for value in args.cpus.split(",")}
    assert len(cpus) == 2 and cpus <= os.sched_getaffinity(0), "select exactly two available CPUs"
    binaries = args.binaries.resolve()
    artifacts = args.artifacts.resolve()
    assert not (artifacts / "report.json").exists(), "choose a fresh artifact directory"
    for command in ["pipewire", "wireplumber", "pw-cli", "pw-top", "pw-record", "taskset", "stdbuf", "systemd-run", "stress-ng"]:
        assert shutil.which(command), f"missing {command}"
    for binary in ["tufo", "tufo-server"]:
        assert (binaries / binary).is_file(), f"missing {binaries / binary}; build before measuring"
    configuration = {"rate": RATE, "quantum": QUANTUM, "duration_seconds": args.duration_seconds,
                     "build_profile": args.build_profile, "kernel": os.uname().release, "machine": os.uname().machine,
                     "python": sys.version.split()[0],
                     "phase": args.phase, "cpus": sorted(cpus), "load": not args.no_load,
                     "load_cpu_quota_percent": 200, "load_memory_max_mib": 768, "load_swap_max_mib": 0,
                     "load_vm_mib": 512, "load_io_file_mib": 64, "load_io_write_kib": 64,
                     "load_io_writes_per_second_max": 8, "load_io_fsync_every_writes": 8}
    baseline = None
    if args.baseline and (args.baseline.exists() or not args.update):
        baseline = json.loads(args.baseline.read_text())
        assert baseline["schema_version"] == SCHEMA_VERSION and baseline["workload_version"] == WORKLOAD_VERSION, "incompatible report schema/workload"
        assert baseline["configuration"] == configuration, "baseline uses a different workload/runner configuration"
    report = {"schema_version": SCHEMA_VERSION, "workload_version": WORKLOAD_VERSION, "configuration": configuration,
              "async_mode": args.async_mode, "started_at_unix_ms": time.time_ns() // 1_000_000,
              "started_at_monotonic_ns": time.monotonic_ns(), "binaries": {name: {"path": str(binaries / name), "sha256": digest(binaries / name)}
                                                                             for name in ["tufo", "tufo-server"]}, "phases": {}, "passed": False}
    session = StabilitySession(binaries, artifacts)
    print(f"Private profile: {session.root}\nArtifacts: {artifacts}", flush=True)
    try:
        session.start_audio()
        env = dict(session.env)
        env["PIPEWIRE_PROPS"] = "{ node.async = " + args.async_mode + " }"
        server = session.start("server", "taskset", "-c", args.cpus, binaries / "tufo-server", env=env)
        wait_for(lambda: (session.root / "tufo/tufo.sock").exists(), bool, "server socket")
        session.health("released")
        reference, entries, playlist = prepare(session)
        phases = ["rain", "mixed"] if args.phase == "all" else [args.phase]
        for phase in phases:
            print(f"Measuring {phase}, node.async={args.async_mode}, {args.duration_seconds // len(phases)} s", flush=True)
            result = run_phase(session, server, args, phase, args.duration_seconds // len(phases), entries, playlist)
            report["phases"][phase] = result
            write_json(artifacts / "report.json", report)
        session.cli("player", "standby")
        session.health("released")
        report["diagnostics_after_standby"] = session.cli("player", "diagnostics")
        # Analysis runs after load stops and playback ends, so it cannot manufacture xruns.
        for phase, result in report["phases"].items():
            path = artifacts / result["capture"]
            waveform = continuity_report(path, reference) if phase == "mixed" else silence_report(path)
            result["waveform"] = waveform
            result["checks"]["waveform"] = waveform["passed"]
        report["passed"] = all(all(result["checks"].values()) for result in report["phases"].values())
        if baseline:
            report["comparison"] = compare(baseline, report)
            report["baseline"] = str(args.baseline.resolve())
    except BaseException as error:
        report["passed"] = False
        report["failure"] = str(error)
        (artifacts / "failure.txt").write_text(traceback.format_exc())
        raise
    finally:
        session.close()
        write_json(artifacts / "report.json", report)
        print(json.dumps({"passed": report["passed"], "report": str(artifacts / "report.json"),
                          "checks": {phase: result["checks"] for phase, result in report["phases"].items()}}, indent=2), flush=True)
    if args.update and report["passed"]:
        report["baseline_updated"] = str(args.baseline.resolve())
        args.baseline.parent.mkdir(parents=True, exist_ok=True)
        write_json(args.baseline, report)
        write_json(artifacts / "report.json", report)
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
