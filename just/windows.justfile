# Install the Windows build tools into the selected winvm-rs state. See docs/agents/windows.md.
[group('quality')]
windows-provision:
    "${WINVM_RS:-winvm-rs}" script --elevated --timeout 5400 scripts/windows/provision-machine.cmd
    "${WINVM_RS:-winvm-rs}" script --timeout 3600 scripts/windows/provision-user.cmd

# Build Tufo on the winvm-rs Windows VM and run its portable tests there
[group('quality')]
windows-check:
    #!/usr/bin/env bash
    set -euo pipefail
    winvm="${WINVM_RS:-winvm-rs}"
    staging="$(mktemp -d)"
    trap 'rm -rf "$staging"' EXIT
    # The paintings' full-resolution originals take hundreds of megabytes, and the build never reads them.
    paintings_originals='assets/backgrounds/originals/'
    while IFS= read -r -d '' file; do
        if [[ -f "$file" && "$file" != "$paintings_originals"* ]]; then cp --parents -- "$file" "$staging"; fi
    done < <(
        git ls-files -z --cached --recurse-submodules
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
