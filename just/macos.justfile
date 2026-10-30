# Lint the macOS code from Linux: clippy for Apple silicon, with zig compiling the C dependencies.
[group('quality')]
macos-lint:
    # Clippy never links, so pointing libopus_sys at a library folder skips building Opus.
    LIBOPUS_LIB_DIR="${CARGO_TARGET_DIR:-target}/macos-lint" CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-target}/macos-lint" \
        cargo-zigbuild clippy --locked --target aarch64-apple-darwin --workspace --all-targets -- -D warnings
