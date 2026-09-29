# Load the audio path under CPU pressure; not a routine check, see docs/agents/verification.md.
[group('performance')]
stress voice="" minutes="10" music="":
    #!/usr/bin/env bash
    set -euo pipefail
    command -v stress-ng >/dev/null || { echo "stress-ng is missing: sudo apt install stress-ng" >&2; exit 1; }
    command -v pw-cli >/dev/null || { echo "pw-cli is missing; the run plays into a PipeWire sink" >&2; exit 1; }
    cargo build --release -p tufo-server
    cargo build --release -p tufo-client --example stress_workload
    target="${CARGO_TARGET_DIR:-target}/release"
    run="$(mktemp -d)"
    sink="tufo-stress-$$"
    load="" server=""
    cleanup() {
        if [[ -n "$load" ]]; then kill "$load" 2>/dev/null || true; fi
        if [[ -n "$server" ]]; then kill -INT "$server" 2>/dev/null || true; fi
        wait
        pw-cli destroy "$sink" >/dev/null 2>&1 || true
        rm -rf "$run"
    }
    trap cleanup EXIT
    # A null sink paces the graph in real time like a device, so pw-top still shows Tufo's xruns.
    # It outlives pw-cli until cleanup destroys it.
    pw-cli create-node adapter "{ factory.name=support.null-audio-sink node.name=$sink node.description=\"Tufo stress run\" media.class=Audio/Sink object.linger=true audio.position=[FL FR] }" >/dev/null
    # PipeWire's ALSA plugin, which cpal's ALSA host opens, plays into the node PIPEWIRE_NODE names.
    export PIPEWIRE_NODE="$sink"
    echo "Local audio plays into the silent PipeWire sink $sink; watch Tufo's row under it in pw-top."
    export TUFO_RUNTIME_DIR="$run/runtime" TUFO_DATABASE_PATH="$run/tufo.sqlite3" XDG_STATE_HOME="$run/state"
    if [[ -z "{{ voice }}" ]]; then
        export TUFO_DISCORD_CREDENTIALS="$run/discord.toml"
    elif [[ -f secrets/discord.toml ]]; then
        export TUFO_DISCORD_CREDENTIALS="$PWD/secrets/discord.toml"
    fi
    taskset -c 2,3 "$target/tufo-server" & server=$!
    # The bot needs a few seconds to log in before it can join a channel.
    sleep 8
    stress-ng --taskset 2,3 --cpu 4 --vm 1 --vm-bytes 512M --hdd 1 --hdd-bytes 256M --timeout "{{ minutes }}m" --metrics-brief & load=$!
    voice=()
    if [[ -n "{{ voice }}" ]]; then voice=(--voice "{{ voice }}"); fi
    music="{{ music }}"
    "$target/examples/stress_workload" --music "${music/#\~/$HOME}" --minutes "{{ minutes }}" "${voice[@]}"
    echo "Server log: $run/state/tufo/tufo-server.log (deleted on exit)"
