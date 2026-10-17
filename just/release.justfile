# Build the executables and the browser host, then package the Ubuntu 24.04+ x64 release in `dist/`.
[group('release')]
[linux]
package-linux: build
    cargo run --locked --quiet -p xtask -- package linux

# Build the executables, then package the Apple silicon release in `dist/`.
[group('release')]
[macos]
package-macos: build
    cargo run --locked --quiet -p xtask -- package macos
