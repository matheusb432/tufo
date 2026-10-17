#!/usr/bin/env bash
# Drops a folder from a GTK window onto the Playlists page in a headless GNOME Shell, with the GUI
# on native Wayland, and checks that the folder became a playlist. Run it through
# `just wayland-drop-check`, which builds the GUI and gives this script its own D-Bus session.
# It writes its steps and verdict to the report file it's given; TUFO_DROP_CHECK_KEEP=1 keeps the
# run folder with every process's log.
set -euo pipefail

target="$1" report="$2"
here="$(cd "$(dirname "$0")" && pwd)"
run="$(mktemp -d)"
mcp_port=9317
shell="" gui="" source=""
cleanup() {
    for process in "$source" "$gui"; do
        if [[ -n "$process" ]]; then kill "$process" 2>/dev/null || true; fi
    done
    # The GUI started a server that outlives it.
    TUFO_RUNTIME_DIR="$run/runtime" "$target/tufo" server kill >/dev/null 2>&1 || true
    if [[ -n "$shell" ]]; then kill "$shell" 2>/dev/null || true; fi
    wait 2>/dev/null || true
    if [[ -n "${TUFO_DROP_CHECK_KEEP:-}" ]]; then report "kept the logs in $run"; else rm -rf "$run"; fi
}
report() { echo "$*" >>"$report"; }
step() { report "check: $*"; }
trap cleanup EXIT

# The nested shell gets its own runtime directory and in-memory settings, so it touches neither
# the desktop session's services nor its configuration, and it has no session to register.
mkdir -m 700 "$run/xdg" "$run/runtime" "$run/home"
export XDG_RUNTIME_DIR="$run/xdg" GSETTINGS_BACKEND=memory GSK_RENDERER=cairo
unset DISPLAY WAYLAND_DISPLAY XDG_SESSION_ID XDG_SESSION_TYPE XDG_SEAT
# Its own home keeps the desktop's extensions out, and its settings keep a pressed window from
# rising over the GUI's, so the drag starts on the source and ends on the GUI.
mkdir -p "$run/home/.config/glib-2.0/settings"
printf '[org/gnome/desktop/wm/preferences]\nraise-on-click=false\n' \
    >"$run/home/.config/glib-2.0/settings/keyfile"
HOME="$run/home" XDG_CONFIG_HOME="$run/home/.config" GSETTINGS_BACKEND=keyfile \
    gnome-shell --headless --wayland --no-x11 --virtual-monitor 1400x900 \
    --wayland-display tufo-drop \
    >"$run/shell.log" 2>&1 & shell=$!
step "starting a headless GNOME Shell"
timeout 30 bash -c "until [[ -S $run/xdg/tufo-drop ]]; do sleep 0.2; done"
export WAYLAND_DISPLAY=tufo-drop

mkdir -p "$run/music/tavern"
ffmpeg -loglevel error -f lavfi -i "sine=frequency=440:duration=2" "$run/music/tavern/theme.wav"

step "opening the GUI, then the drag source"
TUFO_RUNTIME_DIR="$run/runtime" TUFO_DATABASE_PATH="$run/tufo.sqlite3" \
    XDG_CONFIG_HOME="$run/config" XDG_STATE_HOME="$run/state" SLINT_MCP_PORT=$mcp_port \
    RUST_LOG="${RUST_LOG:-info,tufo_gui=debug}" "$target/tufo-gui" >"$run/gui.log" 2>&1 & gui=$!
timeout 60 bash -c "until [[ -S $run/runtime/tufo.sock ]] && curl -s -o /dev/null -X POST -d '{}' http://127.0.0.1:$mcp_port/mcp; do sleep 0.3; done"
sleep 1
# Mutter keeps focus on the GUI's window and stacks the later, maximized drag source beneath it, so
# the source shows around the GUI's edges.
python3 "$here/drag_source.py" "$run/music/tavern" >"$run/source.log" 2>&1 & source=$!
sleep 2

step "dragging the folder onto the Playlists page"
python3 "$here/window.py" $mcp_port open-playlists
# From the drag source, right of the GUI's window, to the middle of the GUI's window.
python3 "$here/pointer.py" "$run/source.log" 1380 450 700 450
sleep 3

collections="$(timeout 5 grpcurl -plaintext -import-path "$here/../../crates/tufo-wire/proto" \
    -proto tufo/v1/library.proto -d '{}' "unix://$run/runtime/tufo.sock" \
    tufo.v1.LibraryService/WatchLibrary 2>/dev/null | grep -c '"title": "tavern"' || true)"
if [[ "$collections" -ge 1 ]]; then
    report "PASS: the dropped folder became a playlist"
else
    report "FAIL: no playlist came from the drop; the drag source's and the GUI's logs follow"
    grep -v "^pointer at" "$run/source.log" >>"$report" || true
    tail -20 "$run/gui.log" >>"$report"
    report "Rerun with TUFO_DROP_CHECK_KEEP=1 to keep every log in the run folder."
    exit 1
fi
