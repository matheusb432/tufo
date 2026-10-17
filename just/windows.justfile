# Install the Windows build tools into the selected winvm-rs state. See wiki/development.md.
[group('quality')]
windows-provision:
    "${WINVM_RS:-winvm-rs}" script --elevated --timeout 5400 scripts/windows/provision-machine.cmd
    "${WINVM_RS:-winvm-rs}" script --timeout 3600 scripts/windows/provision-user.cmd

# Lint the Windows code from Linux: clippy for x86-64 Windows through cargo-xwin.
[group('quality')]
windows-lint:
    # Clippy never links, so pointing libopus_sys at a library folder skips building Opus. CL lets
    # an older clang-cl compile CEF's C++ wrapper against cargo-xwin's MSVC standard library.
    LIBOPUS_LIB_DIR="${CARGO_TARGET_DIR:-target}/windows-lint" CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-target}/windows-lint" \
        CL=/D_ALLOW_COMPILER_AND_STL_VERSION_MISMATCH \
        mise exec -- cargo xwin clippy --locked --target x86_64-pc-windows-msvc --workspace --all-targets -- -D warnings

# Build Tufo on the winvm-rs Windows VM and run its portable tests there
[group('quality')]
windows-check:
    #!/usr/bin/env bash
    set -euo pipefail
    winvm="${WINVM_RS:-winvm-rs}"
    staging="$(mktemp -d)"
    trap 'rm -rf "$staging"' EXIT
    while IFS= read -r -d '' file; do
        if [[ -f "$file" ]]; then cp --parents -- "$file" "$staging"; fi
    done < <(
        git ls-files -z --cached
        git ls-files -z --others --exclude-standard
    )
    "$winvm" send --force "$staging" '%USERPROFILE%\tufo'
    # The target directory sits outside the synced tree, so a sync keeps earlier build output.
    guest_cargo() {
        "$winvm" exec --timeout 7200 --cwd '%USERPROFILE%\tufo' --env 'CARGO_TARGET_DIR=..\tufo-target' -- cargo.exe "$@"
    }
    guest_cargo build --locked --workspace --all-targets
    guest_cargo test --locked -p tufo-cli -p tufo-local-transport -p tufo-models -p tufo-server -p tufo-wire
    guest_cargo test --locked -p tufo-infra --lib discord::credentials
