# Apply the Rust, protobuf, and Markdown formatters.
[group('quality')]
fmt:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo fmt --all
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
    mise exec -- buf format --diff --exit-code
    markdown_files=()
    while IFS= read -r -d '' file; do [[ -f "$file" ]] && markdown_files+=("$file"); done < <(git ls-files --cached --others --exclude-standard -z -- '*.md')
    if ((${#markdown_files[@]})); then mise exec -- rumdl fmt --check --no-cache --deny-config-warnings -- "${markdown_files[@]}"; fi

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
