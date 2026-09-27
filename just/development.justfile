# Open the GUI; it starts tufo-server when none is running.
[group('run')]
gui *args:
    #!/usr/bin/env bash
    set -euo pipefail
    # The GUI starts the tufo-server built next to it when none is running.
    cargo build --release -p tufo-server
    if [[ -f secrets/discord.toml ]]; then export TUFO_DISCORD_CREDENTIALS="$PWD/secrets/discord.toml"; fi
    cargo run --release -p tufo-gui -- "$@"

# Run tufo-server, streaming to Discord when `secrets/discord.toml` exists.
[group('run')]
server *args:
    #!/usr/bin/env bash
    set -euo pipefail
    credentials=()
    if [[ -f secrets/discord.toml ]]; then credentials=(--discord-credentials secrets/discord.toml); fi
    cargo run --release -p tufo-server -- "${credentials[@]}" "$@"

# Drop a folder onto Tufo on native Wayland in a headless GNOME Shell, and check it became a playlist.
[group('run')]
wayland-drop-check:
    #!/usr/bin/env bash
    set -euo pipefail
    for tool in gnome-shell dbus-run-session grpcurl ffmpeg python3; do
        command -v "$tool" >/dev/null || { echo "$tool is missing" >&2; exit 1; }
    done
    python3 -c 'import gi; gi.require_version("Gtk", "4.0")' || { echo "GTK 4's Python bindings are missing" >&2; exit 1; }
    SLINT_EMIT_DEBUG_INFO=1 cargo build -p tufo-gui --features slint/mcp
    cargo build -p tufo-server -p tufo-cli
    report="$(mktemp)" session_log="$(mktemp)"
    trap 'rm -f "$report"' EXIT
    # The nested GNOME session's services log a lot; the check reports on its own.
    status=0
    dbus-run-session -- scripts/wayland-drops/check.sh "${CARGO_TARGET_DIR:-target}/debug" "$report" >"$session_log" 2>&1 || status=$?
    cat "$report"
    if ((status)); then echo "The nested session's output is in $session_log." >&2; else rm -f "$session_log"; fi
    exit "$status"
