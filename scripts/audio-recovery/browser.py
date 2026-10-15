#!/usr/bin/env python3
"""Verify the real browser's audio attachment survives local mixer recovery."""

import argparse
from functools import partial
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import subprocess
import threading
import time

from check import Session, tone, wait_for


def verify_browser(session):
    display = session.start("xvfb", "Xvfb", "-displayfd", "1", "-screen", "0", "1000x700x24", stdout=subprocess.PIPE)
    session.env["DISPLAY"] = ":" + display.stdout.readline().strip()
    session.start_audio()
    session.start("server", session.binaries / "tufo-server")
    wait_for(lambda: (session.root / "tufo/tufo.sock").exists(), bool, "server socket")
    session.health("released")
    page = session.root / "page"
    tone(page / "tone.wav", 880, 60)
    (page / "index.html").write_text('''<!doctype html><title>Recovery fixture</title>
<button style="position:absolute;left:0;top:0;width:400px;height:300px" onclick="document.querySelector('audio').play()">Play synthetic tone</button>
<audio loop src="tone.wav"></audio>''')
    http = ThreadingHTTPServer(("127.0.0.1", 0), partial(SimpleHTTPRequestHandler, directory=str(page)))
    threading.Thread(target=http.serve_forever, daemon=True).start()
    try:
        session.cli("browser", "open")
        wait_for(session.state, lambda state: state["browser"]["status"] == "open", "browser ready", 35)
        grpcurl = subprocess.check_output(["mise", "which", "grpcurl"], text=True).strip()
        base = [grpcurl, "-plaintext", "-import-path", "crates/tufo-wire/proto", "-proto", "tufo/browser/v1/browser.proto"]
        endpoint = "unix://" + str(session.root / "tufo/tufo-browser.sock")
        session.run(*base, "-d", json.dumps({"address": f"http://127.0.0.1:{http.server_port}/index.html"}), endpoint, "tufo.browser.v1.BrowserService/OpenTab")
        view = session.start("browser-view", *base, "-d", "@", endpoint, "tufo.browser.v1.BrowserService/ViewBrowser", stdin=subprocess.PIPE, stdout=subprocess.DEVNULL)
        def send(value):
            view.stdin.write(json.dumps(value) + "\n")
            view.stdin.flush()
        send({"size": {"width": 800, "height": 600, "scaleFactor": 1}})
        time.sleep(1)
        send({"focus": {"focused": True}})
        for pressed in [True, False]:
            send({"pointerButton": {"x": 100, "y": 100, "button": "MOUSE_BUTTON_LEFT", "pressed": pressed, "clickCount": 1}})
        wait_for(session.state, lambda state: state["browser"]["audible"], "browser audio")
        session.cli("player", "settings", "update", '{"browser_volume":0.25}')
        time.sleep(2)
        before = session.capture("browser-before", expected_frequency=880)
        session.run("pw-cli", "destroy", "tufo-test-sink")
        failed = session.health("failed")
        assert failed["browser"]["status"] == "open" and failed["browser"]["audible"]
        session.cli("output", "recover")
        session.health("failed")
        session.sink()
        session.cli("output", "recover")
        restored = session.health("ready")
        assert restored["browser"] == failed["browser"]
        after = session.capture("browser-after", expected_frequency=880)
        assert abs(after["peak"] - before["peak"]) <= 2, (before, after)
        (session.artifacts / "results.json").write_text(json.dumps({"before": before, "after": after, "browser": restored["browser"]}, indent=2))
        session.cli("browser", "close")
        print("Browser audio reattached after local recovery with its gain and pitch preserved", flush=True)
    finally:
        http.shutdown()
        http.server_close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binaries", type=Path, default=Path("target/debug"))
    parser.add_argument("--artifacts", type=Path, default=Path(".artifacts/audio-recovery-browser"))
    args = parser.parse_args()
    session = Session(args.binaries.resolve(), args.artifacts.resolve())
    print(f"Isolated profile: {session.root}", flush=True)
    try:
        verify_browser(session)
    finally:
        session.close()


if __name__ == "__main__":
    main()
