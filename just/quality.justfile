# Apply the Rust, protobuf, and Markdown formatters.
[group('quality')]
fmt:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo fmt --all
    just translations-fmt
    mise exec -- buf format --write
    markdown_files=()
    while IFS= read -r -d '' file; do [[ -f "$file" ]] && markdown_files+=("$file"); done < <(git ls-files --cached --others --exclude-standard -z -- '*.md')
    if ((${#markdown_files[@]})); then mise exec -- rumdl fmt --no-cache --deny-config-warnings -- "${markdown_files[@]}"; fi

# Check formatting without modifying files.
[group('quality')]
fmt-check:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo fmt --all --check
    just translations-fmt --check
    mise exec -- buf format --diff --exit-code
    markdown_files=()
    while IFS= read -r -d '' file; do [[ -f "$file" ]] && markdown_files+=("$file"); done < <(git ls-files --cached --others --exclude-standard -z -- '*.md')
    if ((${#markdown_files[@]})); then mise exec -- rumdl fmt --check --no-cache --deny-config-warnings -- "${markdown_files[@]}"; fi

# Normalize translation references to filenames without line numbers.
[group('quality')]
translations-fmt *args:
    cargo run --quiet -p xtask -- translations-fmt {{ args }}

# Run buf lint and clippy with warnings denied.
[group('quality')]
lint:
    mise exec -- buf lint
    cargo clippy --workspace --all-targets -- -D warnings

# Check the crate dependency graph against the layer policy.
[group('quality')]
architecture:
    cargo run --quiet -p xtask -- check-architecture

# Run the complete read-only formatting, lint, architecture, and ast-grep gate.
[group('quality')]
check:
    just fmt-check
    just lint
    just architecture
    mise exec -- ast-grep scan

# Run the workspace tests with cargo-nextest.
[group('quality')]
test *args:
    cargo nextest run {{ args }}

# Start, check, restart, and stop an isolated server with the executables in `bin_directory`.
[group('quality')]
[script('bash')]
smoke bin_directory:
    set -euo pipefail
    tufo={{ quote(bin_directory / "tufo") }}
    state="$(mktemp -d)"
    # Windows executables need a Windows path.
    if command -v cygpath >/dev/null; then state="$(cygpath -m "$state")"; fi
    export TUFO_RUNTIME_DIR="$state/runtime" TUFO_DATABASE_PATH="$state/tufo.sqlite3"
    export XDG_CONFIG_HOME="$state/config" XDG_DATA_HOME="$state/data" XDG_STATE_HOME="$state/state"
    unset TUFO_DISCORD_CREDENTIALS
    trap 'status=$?; [ "$status" -eq 0 ] || cat "$state/state/tufo/tufo-server.log" 2>/dev/null || true; "$tufo" server kill >/dev/null 2>&1 || true; rm -rf "$state"' EXIT
    "$tufo" --version
    "$tufo" server start
    "$tufo" doctor
    "$tufo" server restart
    "$tufo" server kill
    # `status` exits 1 once the server has stopped.
    if "$tufo" server status >/dev/null; then echo "the server still runs after kill" >&2; exit 1; fi
    echo "the isolated server started, restarted, and stopped"
