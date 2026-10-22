# Desktop app (Linux)

`capycal-app` runs the syncs of one config in the background, with an icon in the panel's tray. Its windows are GTK 4 and libadwaita. It works like the [macOS app](menu.md): the same menu, status lines, Problems window, log window, Remove Synced Events and Start at Login. This page lists only what is different on Linux.

## Requirements

- GTK 4.14 and libadwaita 1.5 or later (Ubuntu 24.04, Debian 13, Fedora 40 and newer).
- A tray that shows StatusNotifierItem icons. KDE Plasma, Cinnamon, XFCE, MATE, Budgie and the Ubuntu desktop do. **GNOME** needs the extension [AppIndicator and KStatusNotifierItem Support](https://extensions.gnome.org/extension/615/appindicator-support/) (Fedora: `gnome-shell-extension-appindicator`). Without a tray, the app shows its menu as a window after ten seconds, with a note about the missing tray; it keeps syncing, and the icon appears as soon as a tray starts.

## Install and start

```bash
sudo apt install ./capycal-app_<version>-1_amd64.deb    # Debian, Ubuntu
sudo dnf install ./capycal-app-<version>-1.x86_64.rpm   # Fedora
yay -S capycal-app-bin                                  # Arch Linux, from the AUR
```

Up to 0.1.0 the app was called `capycal-gtk`. The `capycal-app` package replaces it, and the app points an enabled Start at Login to the new name on its first start.

The package adds **Capycal** to the application menu. Starting it again while it runs opens the menu as a window, with the same status lines and items as the tray menu (without a D-Bus session, the second start says "Capycal is already running" and quits). It uses the default config, `~/.config/capycal/capycal.toml`; for another one, start it with `capycal-app -c /path/to/capycal.toml`. On the first start it writes the commented [starter config](../cli/config-file.md#starter-config) and opens a small "Welcome to Capycal" window: after a short welcome it adds your CalDAV account (server URL, user name, password), then asks for the source and target calendar and what to copy.

Don't run the app and the [systemd user service](../cli/server.md#user-service) with the same config: they share a [config lock](../cli/config-file.md#config-lock), and the second one does not sync.

## Differences from the macOS app

| | Linux |
|---|---|
| Tray icon | A calendar symbol in the panel's own color, like the other tray icons: with a red dot when something needs attention, a dot while syncing, faded when inactive. A left click opens the menu |
| Settings… | The same [Settings](settings.md#the-window) window, in GTK. Without Calendar.app, the source and target lists only show the calendars of your accounts. A calendar you may not write to is marked "(read-only)" in the target list and cannot be chosen |
| General > Open in Editor… | Opens the config with the default application for its file type |
| General > Start Capycal at login | Writes `~/.config/autostart/app.capycal.desktop` (with the config path), which every desktop following the XDG autostart spec runs at login. **Show in Files** opens the config's folder in the file manager |
| Log file | `~/.local/share/capycal/capycal-app.log` |
| Sponsor dialog answers | `~/.config/capycal/sponsor`. `CAPYCAL_ASK_SPONSOR=1` asks at the next quiet moment, a line `sponsoring=1` shows the thank-you, deleting the file resets it |

A password typed in Settings is stored in your keyring through the Secret Service (GNOME Keyring, KWallet), as `{ keychain = "<account>" }` in the config. Passwords can also come from your keyring with a [command credential](../concepts/calendars.md#passwords), for example `{ command = "secret-tool lookup service capycal account work" }`.

In the Settings window, **Ctrl+W** closes it and **Ctrl+Q** quits the app (both ask first about unsaved changes); Ctrl+S, Ctrl+N, Ctrl+Delete and Ctrl+Z work as Cmd on macOS.
