# Troubleshooting

Run `tufo doctor` first.

## No sound

Check the output in the sidebar and the volumes; muting music doesn't mute effects. For Discord,
make sure the bot can connect and speak in the channel. The server's log, `tufo-server.log`, is in
the state folder listed [below](#data-locations).

## Browser won't start

Check `tufo-browser.log` next to the server log. On Ubuntu 24.04 and later, AppArmor can block
Chromium's sandbox. The `.deb` sets this up for you; for a source build, save this as
`/etc/apparmor.d/tufo-browser`:

```text
abi <abi/4.0>,
include <tunables/global>

profile tufo-browser /home/*/.local/lib/tufo/browser/tufo-browser flags=(unconfined) {
  userns,
  include if exists <local/tufo-browser>
}
```

Then run `sudo apparmor_parser -r /etc/apparmor.d/tufo-browser`.

## Window and tray

Without a tray, closing the window quits. GNOME needs an AppIndicator extension for the tray icon.
The mini player's pin doesn't work on Wayland; use your desktop's **Always on Top** instead.

## Data locations

| Platform | Configuration | Data | State and logs |
| --- | --- | --- | --- |
| Linux | `$XDG_CONFIG_HOME/tufo` or `~/.config/tufo` | `$XDG_DATA_HOME/tufo` or `~/.local/share/tufo` | `$XDG_STATE_HOME/tufo` or `~/.local/state/tufo` |
| macOS | `~/Library/Application Support/tufo` | `~/Library/Application Support/tufo` | `~/Library/Application Support/tufo` |
| Windows | `%APPDATA%\tufo\config` | `%LOCALAPPDATA%\tufo\data` | `%LOCALAPPDATA%\tufo\data` |

Uninstalling keeps your library, settings, and token.
