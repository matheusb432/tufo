#!/usr/bin/env python3
"""Exercise local output loss against private PipeWire and synthetic audio only."""

import argparse
import array
import json
import math
import os
from pathlib import Path
import queue
import re
import signal
import statistics
import subprocess
import tempfile
import threading
import time
import wave


def wait_for(read, accept, description, seconds=12):
    deadline = time.monotonic() + seconds
    last = None
    while time.monotonic() < deadline:
        last = read()
        if accept(last):
            return last
        time.sleep(0.1)
    raise AssertionError(f"timed out: {description}; last value: {last}")


def tone(path, frequency, seconds, rate=44100):
    path.parent.mkdir(parents=True, exist_ok=True)
    samples = array.array("h", (int(7000 * math.sin(math.tau * frequency * i / rate)) for i in range(rate * seconds)))
    with wave.open(str(path), "wb") as out:
        out.setparams((1, 2, rate, 0, "NONE", "not compressed"))
        out.writeframes(samples.tobytes())


class Session:
    def __init__(self, binaries, artifacts):
        self.binaries = binaries
        self.artifacts = artifacts
        self.root = Path(tempfile.mkdtemp(prefix="tufo-output-"))
        self.children = []
        self.sequence = 0
        self.env = os.environ.copy()
        for name in ["TUFO_DISCORD_CREDENTIALS", "TUFO_DATABASE_PATH", "TUFO_RUNTIME_DIR", "PIPEWIRE_CONFIG_NAME", "PIPEWIRE_NODE", "WIREPLUMBER_CONFIG_DIR", "WAYLAND_DISPLAY"]:
            self.env.pop(name, None)
        self.env.update({
            "XDG_RUNTIME_DIR": str(self.root / "pw"),
            "PIPEWIRE_RUNTIME_DIR": str(self.root / "pw"),
            "PIPEWIRE_REMOTE": "tufo-test",
            "PIPEWIRE_NODE": "tufo-test-sink",
            "XDG_CONFIG_HOME": str(self.root / "config"),
            "XDG_DATA_HOME": str(self.root / "data"),
            "XDG_STATE_HOME": str(self.root / "state"),
            "TUFO_RUNTIME_DIR": str(self.root / "tufo"),
            "TUFO_DATABASE_PATH": str(self.root / "library.sqlite3"),
            "TUFO_DISCORD_CREDENTIALS": str(self.root / "no-credentials.toml"),
            "TUFO_DESKTOP_SETTINGS_PATH": str(self.root / "config.toml"),
            "GIO_USE_VFS": "local",
            "GTK_USE_PORTAL": "0",
        })
        (self.root / "pw").mkdir(mode=0o700)
        self.artifacts.mkdir(parents=True, exist_ok=True)
        (self.artifacts / "profile.json").write_text(json.dumps({"root": str(self.root), "environment": {k: v for k, v in self.env.items() if k.startswith(("TUFO_", "PIPEWIRE_", "XDG_"))}}, indent=2))
        config = self.root / "pipewire.conf"
        config.write_text('''context.properties = { core.daemon = true core.name = tufo-test default.clock.rate = 48000 default.clock.quantum = 1024 }
context.spa-libs = { audio.convert.* = audioconvert/libspa-audioconvert support.* = support/libspa-support }
context.modules = [
 { name = libpipewire-module-rt flags = [ ifexists nofail ] }
 { name = libpipewire-module-protocol-native }
 { name = libpipewire-module-profiler }
 { name = libpipewire-module-metadata }
 { name = libpipewire-module-spa-node-factory }
 { name = libpipewire-module-client-node }
 { name = libpipewire-module-client-device }
 { name = libpipewire-module-access }
 { name = libpipewire-module-adapter }
 { name = libpipewire-module-link-factory }
 { name = libpipewire-module-session-manager }
]
context.objects = [ { factory = spa-node-factory args = { factory.name = support.node.driver node.name = Dummy-Driver node.group = pipewire.dummy priority.driver = 20000 } } ]
''')
        self.pipewire_config = config
        monitors = self.root / "config/wireplumber/main.lua.d/80-disable-hardware.lua"
        monitors.parent.mkdir(parents=True)
        monitors.write_text("alsa_monitor.enabled = false\nv4l2_monitor.enabled = false\nlibcamera_monitor.enabled = false\n")

    def run(self, *args, check=True, timeout=15):
        result = subprocess.run([str(a) for a in args], env=self.env, text=True, capture_output=True, timeout=timeout)
        if check and result.returncode:
            raise AssertionError(f"{result.args}: {result.stderr}\n{result.stdout}")
        return result

    def start(self, name, *args, env=None, stdout=None, stdin=None):
        self.sequence += 1
        log = (self.artifacts / f"{self.sequence:02}-{name}.log").open("w")
        process = subprocess.Popen([str(a) for a in args], env=env or self.env, stdout=stdout or log, stderr=log, stdin=stdin, text=True)
        log.close()
        self.children.append(process)
        return process

    def stop(self, process):
        if process.poll() is None:
            process.send_signal(signal.SIGTERM)
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=3)

    def start_audio(self, rate=48000):
        config = self.pipewire_config.read_text()
        self.pipewire_config.write_text(re.sub(r"default.clock.rate = \d+", f"default.clock.rate = {rate}", config))
        self.audio_rate = rate
        self.daemon = self.start("pipewire", "pipewire", "-c", self.pipewire_config)
        wait_for(lambda: (self.root / "pw/tufo-test").exists(), bool, "private PipeWire socket")
        self.policy = [self.start("wireplumber-main", "wireplumber", "-c", "main.conf"), self.start("wireplumber-policy", "wireplumber", "-c", "policy.conf")]
        wait_for(lambda: self.run("pw-dump", check=False).returncode, lambda code: code == 0, "private PipeWire registry")
        self.sink()
        self.start("pw-top", "pw-top", "-b", "-n", "360")

    def sink(self, name="tufo-test-sink"):
        self.run("pw-cli", "create-node", "adapter", "{ factory.name=support.null-audio-sink node.name=" + name + f" audio.rate={self.audio_rate}" + " media.class=Audio/Sink object.linger=true audio.position=[FL FR] }")
        wait_for(self.graph, lambda graph: any(item.get("info", {}).get("props", {}).get("node.name") == name for item in graph), "null sink")

    def graph(self):
        return json.loads(self.run("pw-dump").stdout)

    def cli(self, *args, check=True):
        result = self.run(self.binaries / "tufo", "--json", *args, check=check)
        return json.loads(result.stdout) if result.stdout.strip() else None

    def state(self):
        return self.cli("player", "state")

    def health(self, status):
        return wait_for(self.state, lambda state: state["local_output"]["status"] == status, status)

    def capture(self, name, expected_frequency=440, target="tufo-test-sink"):
        path = self.artifacts / f"{name}.wav"
        (self.artifacts / f"{name}-graph.json").write_text(json.dumps(self.graph(), indent=2))
        env = dict(self.env)
        env.pop("PIPEWIRE_NODE", None)
        capture = self.start(name, "pw-record", "--target", target, "--properties", '{ stream.capture.sink = true }', "--rate", "48000", "--channels", "2", "--format", "s16", path, env=env)
        time.sleep(4)
        self.stop(capture)
        with wave.open(str(path)) as audio:
            samples = array.array("h", audio.readframes(audio.getnframes()))[::2]
            rate = audio.getframerate()
        # Attaching a monitor reconfigures the graph; retain its startup in the WAV, but
        # inspect two seconds of settled output after that graph change.
        samples = samples[2 * rate:]
        assert len(samples) >= rate // 2, f"{name}: captured fewer than half a second"
        peak = max(abs(value) for value in samples)
        crossings = [i - left / (right - left) for i, (left, right) in enumerate(zip(samples, samples[1:])) if left <= 0 < right]
        periods = [right - left for left, right in zip(crossings, crossings[1:])]
        period = statistics.median(periods) if periods else 0
        frequency = rate / period if period else 0
        mean_frequency = (len(crossings) - 1) * rate / (crossings[-1] - crossings[0]) if periods else 0
        irregular_cycles = sum(abs(value - period) > period * 0.05 for value in periods)
        silence = longest_silence = 0
        for sample in samples:
            silence = silence + 1 if sample == 0 else 0
            longest_silence = max(silence, longest_silence)
        if expected_frequency is None:
            assert peak == 0, (name, peak)
        else:
            assert peak > 100, (name, peak)
            assert abs(frequency - expected_frequency) < 0.05, (name, frequency)
            assert longest_silence < rate / 1000, (name, longest_silence)
        return {"peak": peak, "frequency_hz": frequency, "mean_frequency_hz": mean_frequency, "irregular_cycles": irregular_cycles, "frames": len(samples), "longest_silence_ms": longest_silence * 1000 / rate}

    def close(self):
        for process in reversed(self.children):
            self.stop(process)


def verify(session):
    session.start_audio()
    server = session.start("server", session.binaries / "tufo-server")
    wait_for(lambda: (session.root / "tufo/tufo.sock").exists(), bool, "server socket")
    assert session.health("released")["sounds"] == []
    for name, frequency in [("main", 220), ("layer", 330), ("queued", 550)]:
        tone(session.root / f"music/{name}.wav", frequency, 45)
    tone(session.root / "effects/loop.wav", 440, 31)
    for folder, kind in [("music", "playlist"), ("effects", "soundboard")]:
        session.cli("library", "import", json.dumps({"kind": kind, "paths": [str(session.root / folder)]}))
    library = session.cli("library", "snapshot")
    entries = {Path(entry["path"]).stem: entry["id"] for collection in library["collections"] for entry in collection["entries"]}
    session.cli("entry", "update", entries["loop"], json.dumps({"sound": {"looping": True, "volume": 0.6}}))
    session.cli("music", "play", "entry", entries["main"])
    session.cli("layer", "add", entries["layer"])
    session.cli("layer", "update", entries["layer"], json.dumps({"volume": 0.4, "looping": True}))
    session.cli("queue", "add", entries["queued"])
    session.cli("music", "seek", "3500")
    session.cli("music", "pause")
    session.cli("player", "settings", "update", json.dumps({"volume": 0.3, "effects_volume": 0.5, "muted": True, "browser_volume": 0.25, "browser_muted": True, "repeat": "playlist", "shuffle": True, "crossfade_ms": 700}))
    session.cli("effect", "play", entries["loop"])
    baseline = session.health("ready")
    results = {"initial": session.capture("initial-loop")}
    graph = session.graph()
    assert not any(item["type"].endswith(":Device") for item in graph), "private session discovered hardware"
    (session.artifacts / "initial-graph.json").write_text(json.dumps(graph, indent=2))

    watcher = session.start("watch", session.binaries / "tufo", "--json", "player", "watch", stdout=subprocess.PIPE)
    events = queue.Queue(maxsize=128)
    def read_events():
        for line in watcher.stdout:
            events.put(json.loads(line))
    threading.Thread(target=read_events, daemon=True).start()
    events.get(timeout=5)
    session.run("pw-cli", "destroy", "tufo-test-sink")
    streamed_failure = wait_for(lambda: events.get(timeout=5), lambda state: state["local_output"]["status"] == "failed", "failure without a command")
    failed = session.health("failed")
    assert failed["local_output"] == streamed_failure["local_output"]
    started = time.monotonic()
    session.cli("output", "recover")
    assert time.monotonic() - started < 1, "retry blocked the control API"
    unavailable = session.health("failed")
    assert unavailable["local_output"]["failure"]["reason"] == "target_missing"
    doctor = session.run(session.binaries / "tufo", "doctor", "--json", check=False)
    assert doctor.returncode == 1 and '"Local audio"' in doctor.stdout
    errors_before_recovery = session.cli("player", "diagnostics")["local_stream_errors"]

    for cycle in range(2):
        session.sink()
        session.cli("output", "recover")
        restored = session.health("ready")
        for field in ["queued_entry_ids", "volume", "effects_volume", "muted", "shuffle", "repeat", "crossfade_ms", "sounds_paused", "browser"]:
            assert restored[field] == baseline[field], (field, restored[field], baseline[field])
        assert restored["music"]["paused"]
        assert restored["music"]["position"]["position_ms"] == failed["music"]["position"]["position_ms"]
        assert len(restored["music_layers"]) == 1
        assert restored["music_layers"][0]["music"]["paused"]
        assert restored["music_layers"][0]["volume"] == baseline["music_layers"][0]["volume"]
        assert restored["music_layers"][0]["looping"] == baseline["music_layers"][0]["looping"]
        assert restored["music_layers"][0]["music"]["position"]["position_ms"] == failed["music_layers"][0]["music"]["position"]["position_ms"]
        assert restored["sounds"][0]["entry_id"] == baseline["sounds"][0]["entry_id"]
        results[f"recovered_{cycle}"] = session.capture(f"recovered-loop-{cycle}")
        assert session.cli("player", "diagnostics")["local_stream_errors"] >= errors_before_recovery
        (session.artifacts / f"recovered-graph-{cycle}.json").write_text(json.dumps(session.graph(), indent=2))
        if cycle == 0:
            session.run("pw-cli", "destroy", "tufo-test-sink")
            failed = session.health("failed")

    session.daemon.send_signal(signal.SIGSTOP)
    try:
        silent_loss = session.health("failed")
        assert silent_loss["local_output"]["failure"]["reason"] == "callback_stopped"
    finally:
        session.daemon.send_signal(signal.SIGCONT)
    session.cli("output", "recover")
    session.health("ready")
    results["silent_transport_loss"] = session.capture("silent-transport-recovery")

    session.cli("music", "resume")
    time.sleep(0.4)
    for process in session.policy:
        session.stop(process)
    session.stop(session.daemon)
    failed = session.health("failed")
    position = failed["music"]["position"]["position_ms"]
    assert not failed["music"]["paused"]
    assert position > baseline["music"]["position"]["position_ms"]
    session.cli("output", "recover")
    session.health("failed")
    session.start_audio(rate=44100)
    session.cli("output", "recover")
    restored = session.health("ready")
    assert restored["local_output"]["identity"]["sample_rate"] == 44100, restored["local_output"]
    assert not restored["music"]["paused"]
    assert 0 <= restored["music"]["position"]["position_ms"] - position < 1000
    session.cli("music", "pause")
    results["daemon_restart"] = session.capture("daemon-restart-loop")
    assert server.poll() is None
    session.sink("ordinary-device-change")
    session.run("pw-metadata", "-n", "default", "0", "default.audio.sink", '{"name":"ordinary-device-change"}', "Spa:String:JSON")
    time.sleep(2.5)
    session.health("ready")
    session.cli("effect", "pause")
    time.sleep(2.5)
    session.health("ready")
    session.run("pw-cli", "destroy", "tufo-test-sink")
    assert session.health("failed")["sounds_paused"]
    session.sink()
    session.cli("output", "recover")
    assert session.health("ready")["sounds_paused"]
    results["paused_recovery"] = session.capture("paused-recovery", expected_frequency=None)
    session.cli("effect", "resume")
    results["resumed_effect"] = session.capture("resumed-effect")
    session.cli("player", "standby")
    session.health("released")
    (session.artifacts / "results.json").write_text(json.dumps(results, indent=2))
    print(json.dumps(results, indent=2), flush=True)


def verify_default_routing(session):
    session.start_audio()
    session.sink("next-default")
    # Even an unexpected native-host failure must not fall through to a hardware ALSA device.
    alsa = session.root / "alsa-null.conf"
    alsa.write_text("pcm.!default { type null }\n")
    session.env["ALSA_CONFIG_PATH"] = str(alsa)
    session.env.pop("PIPEWIRE_NODE")
    nodes = {item.get("info", {}).get("props", {}).get("node.name"): item["id"] for item in session.graph()}
    session.run("wpctl", "set-default", nodes["tufo-test-sink"])
    session.start("server", session.binaries / "tufo-server")
    wait_for(lambda: (session.root / "tufo/tufo.sock").exists(), bool, "server socket")
    session.health("released")
    tone(session.root / "effects/loop.wav", 440, 31)
    session.cli("library", "import", json.dumps({"kind": "soundboard", "paths": [str(session.root / "effects")]}))
    library = session.cli("library", "snapshot")
    entry = next(entry for collection in library["collections"] for entry in collection["entries"] if entry["path"].endswith("loop.wav"))["id"]
    session.cli("entry", "update", entry, '{"sound":{"looping":true}}')
    session.cli("effect", "play", entry)
    assert session.health("ready")["local_output"]["identity"]["backend"] == "PipeWire"
    watcher = session.start("watch-routing", session.binaries / "tufo", "--json", "player", "watch")
    log = session.artifacts / f"{session.sequence:02}-watch-routing.log"
    before = session.capture("default-before")
    session.run("wpctl", "set-default", nodes["next-default"])
    time.sleep(2.5)
    session.health("ready")
    after = session.capture("default-after", target="next-default")
    session.stop(watcher)
    states = [json.loads(line) for line in log.read_text().splitlines()]
    assert states and all(state["local_output"]["status"] == "ready" for state in states)
    (session.artifacts / "results.json").write_text(json.dumps({"before": before, "after": after}, indent=2))
    print("Default device changed with captured audio on the new sink and no false failure", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binaries", type=Path, default=Path("target/debug"))
    parser.add_argument("--artifacts", type=Path, default=Path(".artifacts/audio-recovery"))
    args = parser.parse_args()
    session = Session(args.binaries.resolve(), args.artifacts.resolve())
    print(f"Isolated profile: {session.root}", flush=True)
    try:
        verify(session)
    finally:
        session.close()
    routing = Session(args.binaries.resolve(), args.artifacts.resolve() / "default-routing")
    try:
        verify_default_routing(routing)
    finally:
        routing.close()


if __name__ == "__main__":
    main()
