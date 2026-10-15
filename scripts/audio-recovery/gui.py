#!/usr/bin/env python3
"""Render and operate local output recovery in a private X11/PipeWire session."""

import argparse
import base64
import json
from pathlib import Path
import signal
import socket
import subprocess
import time
import urllib.error
import urllib.request

from check import Session, tone, wait_for


class Gui:
    def __init__(self, session, language):
        self.session = session
        self.language = language
        self.retry = "Retry audio" if language == "en" else "Tentar novamente"
        self.retrying = "Retrying…" if language == "en" else "Tentando…"
        with socket.socket() as reservation:
            reservation.bind(("127.0.0.1", 0))
            self.port = reservation.getsockname()[1]
        (session.root / "config.toml").write_text(f'language = "{language}"\ntours = "always-hide"\n[quit]\nkeep_playing = true\n')
        env = dict(session.env, SLINT_MCP_PORT=str(self.port), SLINT_BACKEND="winit-software")
        self.process = session.start("gui-" + language, session.binaries / "tufo-gui", env=env)
        def windows():
            try:
                return self.call("list_windows")["windowHandles"]
            except (urllib.error.URLError, ConnectionError):
                return []
        self.window = wait_for(windows, bool, "GUI MCP", 20)[0]
        self.root = self.call("get_window_properties", {"windowHandle": self.window})["rootElementHandle"]
        wait_for(lambda: self.find(self.retry), bool, "persistent failure on GUI reconnect")

    def call(self, name, arguments=None):
        request = urllib.request.Request(
            f"http://127.0.0.1:{self.port}/mcp",
            data=json.dumps({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": name, "arguments": arguments or {}}}).encode(),
            headers={"Content-Type": "application/json", "Accept": "application/json, text/event-stream"},
        )
        with urllib.request.urlopen(request, timeout=5) as response:
            raw = response.read().decode()
        message = json.loads(raw) if raw.startswith("{") else json.loads(next(line[6:] for line in raw.splitlines() if line.startswith("data: ")))
        assert "error" not in message, message
        result = message["result"]
        assert not result.get("isError"), result
        if name == "take_screenshot":
            return base64.b64decode(next(item["data"] for item in result["content"] if item["type"] == "image"))
        return json.loads(next(item["text"] for item in result["content"] if item["type"] == "text"))

    def tree(self):
        return self.call("get_element_tree", {"elementHandle": self.root, "maxElements": 1500})["elements"]

    def find(self, label):
        return next((element for element in self.tree() if element.get("accessibleLabel") == label and element.get("computedOpacity", 0) > 0), None)

    def key(self, text, event="PressAndRelease"):
        self.call("dispatch_key_event", {"windowHandle": self.window, "text": text, "eventType": event})

    def toggle_mini(self):
        self.key("\x11", "Press")
        self.key("\x10", "Press")
        self.key("m")
        self.key("\x10", "Release")
        self.key("\x11", "Release")
        time.sleep(0.35)

    def resize(self, width, height):
        windows = self.session.run("xdotool", "search", "--pid", str(self.process.pid), "--name", "Tufo").stdout.splitlines()
        self.session.run("xdotool", "windowsize", windows[-1], str(width), str(height))
        time.sleep(0.35)

    def screenshot(self, name, recovering=False):
        button = wait_for(lambda: self.find(self.retrying if recovering else self.retry), bool, "retry control")
        props = self.call("get_element_properties", {"elementHandle": button["handle"]})
        assert props.get("accessibleEnabled", False) != recovering, props
        prefix = self.session.artifacts / f"{self.language}-{name}"
        prefix.with_suffix(".png").write_bytes(self.call("take_screenshot", {"windowHandle": self.window}))
        prefix.with_suffix(".json").write_text(json.dumps(self.tree(), indent=2))

    def click_retry(self):
        button = wait_for(lambda: self.find(self.retry), bool, "retry control")
        self.call("invoke_accessibility_action", {"elementHandle": button["handle"], "action": "Default_"})


def verify_gui(session):
    display = session.start("xvfb", "Xvfb", "-displayfd", "1", "-screen", "0", "1400x900x24", stdout=subprocess.PIPE)
    session.env["DISPLAY"] = ":" + display.stdout.readline().strip()
    session.start_audio()
    session.start("server", session.binaries / "tufo-server")
    wait_for(lambda: (session.root / "tufo/tufo.sock").exists(), bool, "server socket")
    session.health("released")
    tone(session.root / "effects/loop.wav", 440, 31)
    session.cli("library", "import", json.dumps({"kind": "soundboard", "paths": [str(session.root / "effects")]}))
    library = session.cli("library", "snapshot")
    entry = next(entry for collection in library["collections"] for entry in collection["entries"] if entry["path"].endswith("loop.wav"))["id"]
    session.cli("entry", "update", entry, json.dumps({"sound": {"looping": True}}))
    session.cli("effect", "play", entry)
    session.run("pw-cli", "destroy", "tufo-test-sink")
    session.health("failed")
    for language in ["en", "pt-BR"]:
        gui = Gui(session, language)
        gui.screenshot("full-failed")
        gui.resize(440, 380)
        gui.screenshot("full-minimum-failed")
        gui.key("\x11", "Press")
        gui.key("b")
        gui.key("\x11", "Release")
        time.sleep(0.35)
        gui.screenshot("full-minimum-rail-failed")
        gui.resize(1000, 700)
        gui.toggle_mini()
        gui.resize(320, 252)
        gui.screenshot("mini-failed")
        gui.click_retry()
        session.health("failed")
        gui.screenshot("mini-retry-failed")
        session.daemon.send_signal(signal.SIGSTOP)
        try:
            gui.click_retry()
            recovering = session.health("recovering")
            for _ in range(3):
                started = time.monotonic()
                session.cli("output", "recover")
                assert time.monotonic() - started < 1, "a duplicate retry blocked the API"
                assert session.state()["local_output"] == recovering["local_output"]
            gui.screenshot("mini-recovering", recovering=True)
            gui.toggle_mini()
            gui.screenshot("full-recovering", recovering=True)
            session.health("failed")
        finally:
            session.daemon.send_signal(signal.SIGCONT)
        gui.screenshot("full-retry-failed")
        session.sink()
        gui.key("\x11", "Press")
        gui.key("\x10", "Press")
        gui.key("r")
        gui.key("\x10", "Release")
        gui.key("\x11", "Release")
        session.health("ready")
        wait_for(lambda: gui.find(gui.retry), lambda result: result is None, "error clears after callbacks")
        session.capture(language + "-gui-recovered")
        session.run("pw-cli", "destroy", "tufo-test-sink")
        session.health("failed")
        session.stop(gui.process)
    print("GUI reconnect, failure, retry, recovering and usable output verified in English and pt-BR", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binaries", type=Path, default=Path("target/debug"))
    parser.add_argument("--artifacts", type=Path, default=Path(".artifacts/audio-recovery-gui"))
    args = parser.parse_args()
    session = Session(args.binaries.resolve(), args.artifacts.resolve())
    print(f"Isolated profile: {session.root}", flush=True)
    try:
        verify_gui(session)
    finally:
        session.close()


if __name__ == "__main__":
    main()
