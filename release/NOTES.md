# Tufo 0.1.1

[![Made with Slint](https://github.com/matheusb432/tufo/raw/0.1.1/release/MadeWithSlint-logo-whitebg.png)](https://slint.dev)

Tufo provides a native Slint desktop app for organizing local music, playlists,
soundboards, and built-in ambience. Its local Tonic server communicates with the
GUI and CLI over a private socket or named pipe. Discord output needs a configured
bot and supports one voice channel at a time.

Download one installer and its matching `.sha256` file:

| System | Installer |
| --- | --- |
| Ubuntu 24.04+ x64 | `tufo-0.1.1-linux-x86_64.deb` |
| Windows 11 x64 | `tufo-0.1.1-windows-x86_64-setup.exe` |
| macOS 15+ Apple Silicon | `tufo-0.1.1-macos-arm64.pkg` |

[Installation, update, removal, checksums, and platform limits](https://github.com/matheusb432/tufo/blob/0.1.1/release/INSTALL.md)
are documented for this tag. The Windows installer and macOS package are unsigned;
the macOS app bundle has an ad hoc signature and is not notarized. The package
smoke tests do not establish audible local or Discord playback.
