# Load the audio path under CPU pressure; not a routine check, see wiki/development.md.
[group('performance')]
stress voice="" minutes="10" music="" sounds="":
    #!/usr/bin/env bash
    set -euo pipefail
    command -v stress-ng >/dev/null || { echo "stress-ng is missing: sudo apt install stress-ng" >&2; exit 1; }
    command -v pw-cli >/dev/null || { echo "pw-cli is missing; the run plays into a PipeWire sink" >&2; exit 1; }
    command -v systemd-run >/dev/null || { echo "systemd-run is missing; it bounds the load's CPU, memory, and swap" >&2; exit 1; }
    cargo build --release -p tufo-server
    cargo build --release -p tufo-client --example stress_workload
    target="${CARGO_TARGET_DIR:-target}/release"
    run="$(mktemp -d)"
    sink="tufo-stress-$$"
    load_unit="tufo-stress-$$.scope"
    load="" server=""
    cleanup() {
        if [[ -n "$load" ]]; then
            systemctl --user stop "$load_unit" >/dev/null 2>&1 || true
            kill "$load" 2>/dev/null || true
        fi
        if [[ -n "$server" ]]; then kill -INT "$server" 2>/dev/null || true; fi
        wait
        pw-cli destroy "$sink" >/dev/null 2>&1 || true
        rm -rf "$run"
    }
    trap cleanup EXIT
    # A null sink paces the graph in real time like a device, so pw-top still shows Tufo's xruns.
    # It outlives pw-cli until cleanup destroys it.
    pw-cli create-node adapter "{ factory.name=support.null-audio-sink node.name=$sink node.description=\"Tufo stress run\" media.class=Audio/Sink object.linger=true audio.position=[FL FR] }" >/dev/null
    export PIPEWIRE_NODE="$sink"
    echo "Local audio plays into the silent PipeWire sink $sink; watch Tufo's row under it in pw-top."
    export TUFO_RUNTIME_DIR="$run/runtime" TUFO_DATABASE_PATH="$run/tufo.sqlite3" XDG_STATE_HOME="$run/state"
    export XDG_CONFIG_HOME="$run/config" XDG_DATA_HOME="$run/data"
    if [[ -z "{{ voice }}" ]]; then
        export TUFO_DISCORD_CREDENTIALS="$run/discord.toml"
    elif [[ -f secrets/discord.toml ]]; then
        export TUFO_DISCORD_CREDENTIALS="$PWD/secrets/discord.toml"
    fi
    taskset -c 2,3 "$target/tufo-server" & server=$!
    # The bot needs a few seconds to log in before it can join a channel.
    sleep 8
    mkdir -p "$run/load"
    systemd-run --user --scope --quiet --unit "$load_unit" \
        -p CPUQuota=200% -p MemoryMax=768M -p MemorySwapMax=0 -p IOWeight=10 \
        stress-ng --taskset 2,3 --cpu 4 --vm 1 --vm-bytes 512M --hdd 1 --hdd-bytes 256M \
        --temp-path "$run/load" --timeout "{{ minutes }}m" --metrics-brief & load=$!
    voice=()
    if [[ -n "{{ voice }}" ]]; then voice=(--voice "{{ voice }}"); fi
    music="{{ music }}"
    sounds=()
    sounds_path="{{ sounds }}"
    if [[ -n "$sounds_path" ]]; then sounds=(--sounds "${sounds_path/#\~/$HOME}"); fi
    # Loop wraps must not grow the server's memory; compare a minute in with the last half minute.
    (sleep 60; ps -o rss= -p "$server" > "$run/rss-early"; sleep $(({{ minutes }} * 60 - 90)); ps -o rss= -p "$server" > "$run/rss-late") &
    "$target/examples/stress_workload" --music "${music/#\~/$HOME}" --minutes "{{ minutes }}" "${voice[@]}" "${sounds[@]}"
    echo "Server RSS: $(cat "$run/rss-early" 2>/dev/null || echo ?) KiB a minute in, $(cat "$run/rss-late" 2>/dev/null || echo ?) KiB 30 s before the end"
    echo "Server log: $run/state/tufo/tufo-server.log (deleted on exit)"
