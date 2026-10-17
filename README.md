# Tufo

Music and sfx player for ttrpg sessions, running on your computer or in one Discord voice channel.

The native desktop app uses Slint and a local Rust server. It supports Linux, macOS, and Windows,
with English and Brazilian Portuguese interfaces.

## Get started

Application downloads belong on the [Releases page](https://github.com/matheusb432/tufo/releases);
see [installation and updates](release/INSTALL.md) for platform requirements and checksums.
Individual workspace crates are not published. For a source build, follow [Build from source](release/INSTALL.md#build-from-source).

[![Made with Slint](release/MadeWithSlint-logo-whitebg.png)](https://slint.dev)

1. Open Tufo and drop a music folder on Playlists or an effects folder on Soundboards.
2. Play a song or trigger a sound.
3. Choose your output in the sidebar. For voice channels, first [connect a Discord bot](wiki/discord.md).

The **?** button starts contextual guides for the current page.
Closing hides the window when a tray is available; quitting stops playback unless
**Keep playing after quitting** is enabled.

## Guides

- [Library organization](wiki/library.md): followed folders, manual collections, search, and missing files.
- [Playback and soundboards](wiki/playback.md): queues, music layers, effects, and volume.
- [Browser](wiki/browser.md): music and ambience from websites, mixed with the rest.
- [Outputs and Discord setup](wiki/discord.md): local audio, bot permissions, and voice channels.
- [Import from Kenku FM](wiki/import.md): migration, conflict choices, and retries.
- [Command-line use](wiki/usage.md): resource commands, JSON input and output, and watching state.
- [Troubleshooting](wiki/troubleshooting.md): diagnostics, tray behavior, and stored data.

Tufo is [MIT licensed](LICENSE). Paintings, recordings, fonts, icons, and vendored libraries
retain the licenses listed in [Third-party material](THIRD-PARTY.md).
