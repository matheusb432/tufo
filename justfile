set minimum-version := "1.56.0"
set shell := ["bash", "-eu", "-o", "pipefail", "-c"]
set windows-shell := ["bash", "-eu", "-o", "pipefail", "-c"]
set positional-arguments

_default:
    @just --list --unsorted

# Install the pinned development tools with mise.
[group('build')]
bootstrap *args:
    mise bootstrap --yes {{ args }}

# Fetch Rust dependencies and check that the protobufs build.
[group('build')]
setup *args:
    cargo fetch {{ args }}
    mise exec -- buf build >/dev/null

# Build the GUI, server, tufo command, and browser host, which Windows loads as a DLL.
[group('build')]
build:
    cargo build --release -p tufo-gui -p tufo-server -p tufo-cli -p tufo-browser

# Install all binaries and the desktop launcher, then start the installed server.
[group('build')]
install: build
    cargo run --quiet -p xtask -- install

# Rebuild and replace the installed binaries, then start the replacement server.
[group('build')]
update: build
    cargo run --quiet -p xtask -- update

# Remove the installed binaries and launcher; keep the library, settings, and credentials.
[group('build')]
uninstall:
    cargo run --quiet -p xtask -- uninstall

# Cross-build the Windows executables and pack them into an unsigned installer in `dist/`.
[group('build')]
windows-installer:
    mise exec -- cargo run --quiet -p xtask -- windows-installer

# Render both Windows ICO files from the app, small, and tray SVG sources.
[group('build')]
app-icon:
    cargo run --quiet -p xtask -- app-icon

# Generate a migration from the Toasty models: `just migration-generate add_sound_settings`.
[group('build')]
migration-generate name:
    cargo run --quiet -p tufo-migrator -- generate "$1"

import 'just/development.justfile'
import 'just/quality.justfile'
import 'just/macos.justfile'
import 'just/windows.justfile'
import 'just/audio.justfile'
import 'just/release.justfile'
