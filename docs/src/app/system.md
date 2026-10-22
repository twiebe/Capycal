# Login, files and the CLI

## Start at Login

**Start Capycal at login** (Settings > General) registers the app as a login item; it appears in System Settings > General > Login Items. If the app does not run from an app bundle, or macOS refuses, it writes a LaunchAgent to `~/Library/LaunchAgents/app.capycal-ui.plist` instead. Turning it off removes the registration.

## Sponsoring

30 days after the first successful sync, the app asks whether you'd like to sponsor Capycal, and after that about once a year. It only asks at a quiet moment: no sync running, no problem shown. The dialog has a button per way to sponsor, **Sponsor on GitHub** ([GitHub Sponsors](https://github.com/sponsors/twiebe)) and **Support on Ko-fi** ([Ko-fi](https://ko-fi.com/thomaswiebe)). Choosing one opens its page.

- **Sponsor on GitHub**, **Support on Ko-fi** and **Already Sponsoring** ask again a year later, with a thank-you, in case the sponsorship was a one-time one or has ended.
- **Not Now** (also Return, Esc or closing the dialog) asks again half a year later the first time, then a year later.
- **I'd Rather Not** ends the asking for good.

The About dialog and **Support Capycal** at the end of Settings > General always list the same links; opening one there does not change when the app asks.

A line below the buttons says when the app asks again.

The app keeps this in its user defaults (`SponsorFirstSeen`, `SponsorNext`, `SponsorSponsoring`, `SponsorNever` in `app.capycal`). A `capycal-ui` binary started outside the app bundle, for example with `cargo run`, keeps them in the domain `capycal-ui` instead; use that name in the commands below. To see the dialog without waiting, start the app with `CAPYCAL_ASK_SPONSOR=1`; it then asks at the next quiet moment, whatever the defaults say:

```bash
CAPYCAL_ASK_SPONSOR=1 /Applications/Capycal.app/Contents/MacOS/capycal-ui
```

To see the thank-you a sponsor gets a year later, set `SponsorSponsoring` first:

```bash
defaults write app.capycal SponsorSponsoring -bool true
```

`defaults delete app.capycal` resets it all.

## Another config file

The app uses the default config path, `~/Library/Application Support/capycal/capycal.toml`. To run another config, start it with:

```bash
open -a Capycal --args -c /path/to/capycal.toml
```

## Running alongside the CLI

The app takes the same [config lock](../cli/config-file.md#config-lock) as the CLI. Don't run the same config in the app and in a `capycal` daemon (for example the LaunchAgent from `contrib/`) at the same time: the second one shows or logs the first one's pid and does not sync. Using the CLI for one-off commands such as `--validate` or `--discover-eventkit` while the app runs is fine.

A second copy of the app (for example `Capycal.app/Contents/MacOS/capycal-ui` started from a terminal while the app runs) says "Capycal is already running" and quits; Finder and `open` bring the running app forward anyway.

The CLI inside the bundle is at `/Applications/Capycal.app/Contents/MacOS/capycal`.

## Where files live

| What | Path |
|---|---|
| Config | `~/Library/Application Support/capycal/capycal.toml` |
| State | `~/Library/Application Support/capycal/state/` (or `state_dir`) |
| Config locks | `<state dir>/locks/` |
| Log files | `~/Library/Logs/capycal/capycal-ui.log*` |
| Login item fallback | `~/Library/LaunchAgents/app.capycal-ui.plist` |
| Sponsor dialog answers | user defaults `app.capycal` |

Calendar access is granted to "Capycal" in System Settings > Privacy & Security > Calendars.
