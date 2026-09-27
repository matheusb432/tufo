@echo off
rem Installs the per-user tools Tufo's Windows build needs: Rust through rustup, and buf, which
rem tufo-wire's build script runs. Run as the standard user through `winvm-rs script`; rerunning
rem skips installed tools.
setlocal
set "DOWNLOADS=%TEMP%\tufo-provision"
set "CARGO_BIN=%USERPROFILE%\.cargo\bin"
if not exist "%DOWNLOADS%" mkdir "%DOWNLOADS%"

if exist "%CARGO_BIN%\rustup.exe" goto rust_ready
echo Installing Rust...
curl.exe -fsSL -o "%DOWNLOADS%\rustup-init.exe" https://static.rust-lang.org/rustup/dist/x86_64-pc-windows-msvc/rustup-init.exe || exit /b 1
"%DOWNLOADS%\rustup-init.exe" -y --default-toolchain stable --profile minimal || exit /b 1
:rust_ready
"%CARGO_BIN%\rustup.exe" toolchain install stable --profile minimal --component clippy --component rustfmt --component llvm-tools-preview || exit /b 1

if exist "%CARGO_BIN%\buf.exe" goto buf_ready
echo Installing buf 1.72.0...
curl.exe -fsSL -o "%CARGO_BIN%\buf.exe" https://github.com/bufbuild/buf/releases/download/v1.72.0/buf-Windows-x86_64.exe || exit /b 1
:buf_ready

"%CARGO_BIN%\cargo.exe" --version || exit /b 1
"%CARGO_BIN%\buf.exe" --version || exit /b 1
