# Local audio stability

`just stress` captures ten minutes of production playback in a private PipeWire session at 48 kHz with a fixed 512-frame quantum. It spends half the run playing the built-in Rain alone, then plays a crossfading synthetic playlist, two layers repeating saved regions, a cached synthetic loop, and a streamed one-shot every ten seconds. The private session disables hardware discovery and uses a null sink. It never opens a window or reads the developer's library or credentials.

The server and load use CPUs 2 and 3. The load's user scope limits aggregate CPU to 200%, memory to 768 MiB, and swap to zero. Its memory worker uses 512 MiB; the I/O worker writes at most 64 KiB every 125 ms into a 64 MiB rotating disposable file, with an fsync every eight writes. All load processes have deadlines and are stopped during cleanup. Choose two available CPUs with `TUFO_STRESS_CPUS=CPU,CPU`.

The harness reuses `scripts/audio-recovery/check.py` for isolated profiles, native output routing, and process cleanup. It needs that harness's PipeWire/WirePlumber 0.4 prerequisites, Python 3, `stress-ng`, `taskset`, and a running user systemd manager. No Python packages are needed. `dbus-run-session` isolates the audio session's bus; the bounded load scope uses the existing user manager's bus explicitly.

Artifacts remain under the printed `.artifacts/audio-stability/` directory, including WAV captures, graph snapshots, thread scheduling, server logs, timestamped `pw-top` rows, per-second diagnostic and memory samples, per-audio-thread page-fault counts, timed control commands, fixture hashes, executable hashes, and `report.json`. A failed check retains its evidence and exits nonzero. Temporary profiles and their generated fixtures also remain for inspection; remove the printed profile directory when finished.

When available, each phase summarizes `local_callbacks` and `audio_memory` alongside their complete raw samples. Callback summaries retain cumulative cadence/render histogram bounds, phase counter deltas, anomaly times, callback frame sizes, and observed CPAL playback lead. They are observations rather than xrun counts; playback lead does not measure decoded-source buffer headroom. Memory summaries show observed usage and locking peaks, remaining budget, lock-failure totals and phase deltas. These summaries do not add acceptance failures or change the workload. Older baseline executables can omit either diagnostic object.

Waveform event times count captured frames after the two-second analysis skip. The report records recorder start, stop request, and exit times for approximate correlation with diagnostics and `pw-top`; these are not exact first/last-frame graph timestamps. The waveform window includes load startup and teardown outside the diagnostics/xrun interval, and is deliberately retained in full after the startup exclusion. A captured discontinuity fails acceptance even when counters remain zero, but that alone cannot attribute omitted frames to the renderer, transport, graph, or capture process.

The Rain capture must contain audio and no exact-zero gap lasting one millisecond after monitor startup. This does not compare Rain's entire waveform to a reference. The mixed workload reserves the right channel for a deterministic PCM sequence and puts the other synthetic sources on the left. Matching that evolving sequence catches missing frames, repeated frames, silence, and changed playback speed without mistaking a sine's repeated cycles for continuity. The signal repeats every 31 seconds; whole-period displacement is outside this detector's distinction and is also covered by the output's xrun/timing evidence. Analysis happens after load and playback stop.

Every measured phase requires zero decoder underruns, zero reported/discarded CPAL errors, and zero additional PipeWire errors after its startup baseline. Server output nodes must actually appear in `pw-top`; absent evidence fails the check. The active driver must remain at 512 frames, and the server's render/transport threads plus the daemon and recorder transport threads must have real-time scheduling. Timed volume-control acknowledgements must have p99 at most 250 ms and maximum below one second. File preparation is measured separately. Settled RSS growth must stay below the larger of 16 MiB and 5% of settled RSS. This growth check complements the engine's explicit memory accounting; allocator high-water retention is permitted. A ten-minute pass establishes behavior for this workload and load envelope.

Build outside a measurement run, then run a short wiring check without load:

```sh
cargo build --release -j 2 -p tufo-server -p tufo-cli
dbus-run-session -- python3 scripts/audio-stability/check.py \
  --binaries target/release --duration-seconds 20 --phase mixed --no-load \
  --async-mode false --artifacts .artifacts/audio-stability/wiring
python3 -m unittest discover -s scripts/audio-stability -p 'test_*.py'
```

For a controlled comparison, retain baseline executables in a separate directory, finish all builds, and run each variant alone under the same host conditions:

```sh
dbus-run-session -- python3 scripts/audio-stability/check.py \
  --binaries .artifacts/audio-stability/baseline-bin --duration-seconds 600 \
  --async-mode false --artifacts .artifacts/audio-stability/baseline-false
dbus-run-session -- python3 scripts/audio-stability/check.py \
  --binaries target/release --duration-seconds 600 --async-mode false \
  --baseline .artifacts/audio-stability/baseline-false/report.json \
  --artifacts .artifacts/audio-stability/candidate-false
```

Repeat with `--async-mode true` to compare PipeWire scheduling separately from the code change. The setting applies only to the server through `PIPEWIRE_PROPS`; graph snapshots must confirm it. `--baseline` rejects incompatible report schemas, fixture semantics, CPU sets, durations, quantum, and load settings before starting measurement. Failed baseline reports are useful evidence and remain comparable. Inspect variation before interpreting performance changes.

`--update --baseline PATH` creates a baseline or compares a compatible existing one, then replaces it atomically only after every check passes. A failed run preserves the existing baseline and retains its own report. An incompatible existing baseline is rejected before playback. Ordinary `--baseline PATH` requires the report to exist; standalone stress acceptance does not require a baseline. Use a fresh `--artifacts` directory for each run.

`TUFO_STRESS_BINARIES=DIR` makes `just stress` use prepared executables without building. Other recipe overrides are `TUFO_STRESS_ASYNC=false|true`, `TUFO_STRESS_ARTIFACTS=DIR`, `TUFO_STRESS_BASELINE=REPORT`, and `TUFO_STRESS_PHASE=all|rain|mixed`. A duration passed directly to the Python harness counts total measured seconds across the chosen phases, excluding preparation and monitor startup.

`just stress 'GUILD_ID:CHANNEL_ID' 10 /abs/music /abs/sounds` retains the existing Discord workload. It requires explicitly supplied media and credentials and separate received-audio evidence.
