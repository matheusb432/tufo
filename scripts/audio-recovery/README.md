# Local audio recovery checks

These Linux checks start private PipeWire and WirePlumber processes, create null sinks, and generate sine-wave WAV fixtures in temporary directories. They never use hardware devices or the user's Tufo profile. Each check stops its own processes and keeps its disposable profile path, logs, captures, and results for inspection.

Prerequisites are PipeWire tools, WirePlumber 0.4 with its `main.conf` and `policy.conf`, Python 3, and `dbus-run-session`. GUI and browser checks also need Xvfb; the GUI check uses xdotool and Slint MCP, and the browser check uses the mise-managed grpcurl and a repository-built browser host.

From the repository root:

```sh
cargo build -p tufo-server -p tufo-cli -p tufo-browser
SLINT_EMIT_DEBUG_INFO=1 cargo build -p tufo-gui --features slint/mcp
dbus-run-session -- python3 scripts/audio-recovery/check.py
dbus-run-session -- python3 scripts/audio-recovery/gui.py
dbus-run-session -- python3 scripts/audio-recovery/browser.py
```

Use `--binaries PATH` to select a build and `--artifacts PATH` to choose the capture directory. Defaults are `target/debug` and separate directories under `.artifacts/`.

`check.py` destroys and recreates the selected sink, stalls and restarts its private daemon at a different rate, and verifies streamed failure, retry while unavailable, lifetime counters, doctor diagnostics, preserved session settings, moving and paused music, and paused effects. Captured output must contain the expected tone, within 0.05 Hz by median cycle length, with no silence lasting a millisecond. Results retain mean frequency and irregular cycle counts separately for continuity analysis. Each WAV includes the monitor's graph startup; assertions use the settled output after two seconds. Paused effects must capture silence. Ordinary routing changes and intentional release must not report failure.

`gui.py` opens a fresh GUI while already failed, exercises the accessibility action and keyboard retry, and captures failure, failed retry, and recovery in English and pt-BR. It checks full and mini forms, including their minimum sizes. Suspending only its private PipeWire daemon holds a recovery open long enough to verify disabled controls, bounded failure, and responsive duplicate requests.

`browser.py` serves a synthetic tone from a local HTTP page, starts the real browser host, and verifies its audio is reattached with the same gain and pitch after recovery.

These recovery checks complement `just stress`, the ten-minute load and captured-waveform check.
