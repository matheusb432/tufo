# Install Tufo

Download the installer and its matching `.sha256` file from the same release.
Keep the `tufo` CLI, `tufo-server`, and `tufo-gui` together. The GUI starts its sibling
server over a user-private Unix socket or Windows named pipe; it needs no web server.
Tufo points at your music files and never copies them into the package.

## Check the download

On Linux, run `sha256sum --check tufo-*-linux-x86_64.deb.sha256`. On macOS, run
`shasum -a 256 -c tufo-*-macos-arm64.pkg.sha256`. On Windows, compare the hash
printed by `Get-FileHash .\tufo-*-windows-x86_64-setup.exe -Algorithm SHA256` in
PowerShell with the first field of the matching `.sha256` file.

## Ubuntu 24.04 or newer, x64

Install the `.deb` with your package manager:

```sh
sudo apt install ./tufo-*-linux-x86_64.deb
```

Open Tufo from the application menu, or run `tufo-gui`. `tufo`, `tufo-server`,
and `tufo-gui` are in `/usr/bin`. The package installs the linked ALSA, PipeWire,
D-Bus, font, X11, and Wayland libraries; use a graphical session and a working
audio service for local playback.

Before updating or removing Tufo, exit the GUI using **Close** in its tray menu,
then run `tufo server kill`. The server can remain
alive after the window closes. Install a newer `.deb` to update. To remove it,
first disable **Start at login** in Settings, exit the GUI and stop the server, then run:

```sh
tufo server kill
rm -f "${XDG_CONFIG_HOME:-$HOME/.config}/autostart/dev.tufo.Tufo.desktop"
sudo apt remove tufo
```

## Windows 11, x64

Run `tufo-*-windows-x86_64-setup.exe`, then open Tufo from the Start menu.
Installation is per user in `%LOCALAPPDATA%\Programs\Tufo` and needs no administrator
access. The bundled MSVC runtime needs no separate Visual C++ Redistributable.
The CLI is `%LOCALAPPDATA%\Programs\Tufo\tufo.exe`; add that folder to your user
`PATH` if you want to type `tufo` from a terminal. Run the newer setup program
to update, or remove Tufo through Windows Settings → Apps.

The installer is unsigned, so Windows may ask you to confirm the publisher.

## macOS 15 or newer, Apple Silicon

Install `tufo-*-macos-arm64.pkg`, then open `/Applications/Tufo.app`.
The CLI is linked at `/usr/local/bin/tufo`. The app bundle is ad hoc signed, but
the package is unsigned and neither is notarized. If macOS blocks installation or
first launch, allow Tufo in System Settings → Privacy & Security.

Before updating, exit the GUI using **Close** in its menu bar icon, run `tufo server kill`, then
install the newer `.pkg`. The installer always replaces `/Applications/Tufo.app`;
if you previously built Tufo into `~/Applications`, remove that older app copy
after stopping it to avoid opening the wrong version.

To remove Tufo, first disable **Start at login** in Settings and exit the GUI
using **Close** in its menu bar icon, then run:

```sh
tufo server kill
rm -f "$HOME/Library/LaunchAgents/dev.tufo.Tufo.plist"
sudo rm /usr/local/bin/tufo
sudo rm -r /Applications/Tufo.app
sudo pkgutil --forget dev.tufo.Tufo
```

## Data

Updates and removal leave your library, desktop settings, and Discord credentials
in your user profile. `tufo doctor` reports gRPC health, version match, and Discord
configuration. See [Discord setup](../wiki/discord.md) and
[troubleshooting](../wiki/troubleshooting.md).
