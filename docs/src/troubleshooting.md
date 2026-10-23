# Troubleshooting and FAQ

## Getting debug logs

- CLI: `capycal --once -v` (debug) or `-vv` (trace) runs one cycle with details per event. `RUST_LOG=capycal=debug` works too and overrides both `-v` and `log_level`.
- Daemon: set `log_level = "debug"` in `[daemon]` and reload (`SIGHUP`, or save with `watch_config = true`). This has no effect if `RUST_LOG` or `-v` set the level at start.
- Desktop app: a click on a red status line in the menu opens the Problems window, which shows the full error message and the recent warnings and errors. For more, set `log_level = "debug"` and save the config (the app reloads it), reproduce the problem, then **Show Log** (Settings > General) > **Export**.

Logs never contain passwords. They do contain calendar URLs and event UIDs, so check before sharing them.

## Sync

### Duplicate events

Capycal recognises its own events by the marker of the sync and the source event, so a [full sync](concepts/full-sync.md) removes duplicates of one sync: run `capycal --full-sync --once` or **Full Sync Now** on the sync's page in Settings (Advanced settings > Troubleshooting). Duplicates that come back keep a cause:

- **Copied config on a second machine**, or the same config in the desktop app and a LaunchAgent on different state directories. Both write the same events; run each sync in one place only.
- **Renamed sync**: the marker and the target UIDs depend on the sync name, so the old events are no longer this sync's. Purge before renaming (see [Syncs](concepts/syncs.md#name)).

To remove everything a sync created, run `capycal --purge <sync>` (on the machine whose config has this sync), or use **Remove Synced Events…** (Settings, the sync's Advanced settings > Troubleshooting) in the desktop app. `--purge-all` (or **Remove All Events…** next to it in the app) empties a calendar used only by Capycal.

### A deleted copy comes back

A sync owns its copies: one deleted or changed by hand in a CalDAV target is restored by the next cycle, and by the next full sync on any target (see [One sync on several computers](concepts/several-computers.md)). To keep an event out of the target, leave it out with a [`skip`](concepts/transforms.md#skip) transform, switch the sync off, or remove its copies with **Remove Synced Events…** or `capycal --purge <sync>`.

### Lost or corrupt state

The state files are a cache. If one is missing (deleted state directory, new machine with the same target) the sync starts with a full sync that rebuilds it from the markers in the target, without creating events twice. A state file that cannot be parsed is renamed to `<sync>.json.corrupt-<timestamp>`, logged as a warning ("state file corrupt, moved aside"), and the sync rebuilds it the same way. The renamed file can be deleted.

### "most synced events are gone from the source, removing them in the next cycle"

When the source suddenly has less than half as many events as were synced, Capycal waits one cycle before removing their copies (see [Full sync](concepts/full-sync.md#when-events-disappear-from-the-source)). If you cleared the source on purpose, the copies go with the next cycle; **Now** in the desktop app or another `capycal --once` starts it right away. If the source events are still there, the source answered incompletely and the next cycle normally finds them again.

### Changes to fields or transforms do not show

Changing a sync's source, target, `fields` or transforms makes its next cycle a [full sync](concepts/full-sync.md) by itself: it applies the new rules to every event, updates the events in the target and removes those the new filters drop. The copies change at the next cycle (`interval`), or at once with **Sync Now**. If they still look old, check the log for errors of that cycle, or start a full sync yourself: `capycal --full-sync --once`, or **Full Sync Now** in the desktop app (Settings, the sync's Advanced settings > Troubleshooting).

### Changes in a calendar feed show up hours late

Capycal reads a [feed](concepts/calendars.md#calendar-feeds-ical-address) at every `interval`, but the service decides how fresh it is: Google refreshes the secret iCal address only every few hours. Nothing in Capycal makes it faster. For changes within minutes, read the calendar through Calendar.app on a Mac, or over CalDAV where the service offers it with a password.

### "the feed answered 404 Not Found" / "does not return an iCalendar file"

The feed's address is wrong or no longer valid, for example after resetting Google's secret address. Copy it again from the service and paste it into the sync's **Feed address** (or `ics_url`). "does not return an iCalendar file" means the address answers with a web page, often a sign-in page: use the address meant for calendar apps (Google: **Secret address in iCal format**, not the public or embed link).

### Blockers in the wrong place with a time condition

A condition on the start (`days`, `start_time`, `end_time`) uses the timezone of the machine running Capycal, which is often UTC on servers and in containers. See [Timezone](cookbook/time-filters.md#timezone).

## Desktop app

### macOS asks for the Keychain password after an update

macOS ties the permission to read a Keychain item to the exact build of the app, since Capycal is not signed with an Apple Developer ID. **Always Allow** keeps it until the next update; after an update macOS asks once more for each account. The app explains this before the prompt appears. To avoid the prompts, take the password from a file or an environment variable instead (see [Credentials](concepts/calendars.md#passwords)).

### The app won't open ("cannot be opened", "unidentified developer")

The app is not notarized. Right-click `Capycal.app` in Finder and choose **Open**, or run `xattr -dr com.apple.quarantine /Applications/Capycal.app`. For the CLI binary: `xattr -d com.apple.quarantine /usr/local/bin/capycal`. If the app opens but no menu bar icon appears, the menu bar may be full; quit other menu bar apps or check with `pgrep -l capycal-ui`.

### Calendar access denied / no Calendar.app events

- Check System Settings > Privacy & Security > Calendars. The permission belongs to "Capycal" for the app, to the terminal app for the CLI in a terminal, and to the `capycal` binary for a LaunchAgent (see [Permissions](concepts/calendar-app.md#permissions)).
- If you denied the prompt once, macOS doesn't ask again. Enable Capycal in the settings, or reset it with `tccutil reset Calendar app.capycal` (app) and start again.
- `calendar '...' not found in Calendar.app`: compare the `eventkit_calendar` value with the output of `capycal --discover-eventkit`.
- Events outside the [event window](concepts/syncs.md#event-window) are not synced.

### Linux desktop app

The Linux app is `capycal-app` (see [Desktop app (Linux)](app/linux.md)); `capycal-ui` is the macOS app and only prints a message on Linux. If the app says it has no tray icon, your desktop doesn't show StatusNotifierItem icons: on GNOME install the AppIndicator extension. The icon then appears without a restart.

## Config file and CLI

### "already in use by another Capycal process"

Another process holds the [config lock](cli/config-file.md#config-lock): a running daemon, a LaunchAgent or systemd service, or the desktop app. The message names the pid. Stop that process, or use it instead (send it `SIGHUP` to reload). The desktop app shows this as "Another Capycal runs this config (pid N)".

### "sync '...' is also run by config ... with the same state directory"

Two different configs share a state directory and have a sync with the same name; both would write the same state file. Rename one of the syncs, or give one config its own `state_dir`.

### "unknown field `...`"

The config has a key Capycal doesn't know at this place. The message shows the line and the keys allowed there. Check spelling and placement: `days_past` belongs to the `[syncs.<name>]` table, not to the calendar; `concurrency` belongs to the calendar; `[daemon]` options don't go into a sync.

### "syncs are named tables now: replace `[[syncs]]` ..."

The config uses the format of older Capycal versions, where each sync was a `[[syncs]]` entry with a `name` key and its transforms followed as `[[syncs.transforms]]`. Each sync is now a table named by its key:

```toml
# before
[[syncs]]
name = "work-to-home"
source = "work"

[[syncs.transforms]]
field = "SUMMARY"
action = "set_field"
value = "Busy"

# now
[syncs.work-to-home]
source = "work"
transforms = [
  { field = "SUMMARY", action = "set_field", value = "Busy" },
]
```

See [`[syncs.<name>]`](reference/options.md#syncsname) and [Transforms](reference/options.md#transforms).

### "no syncs defined in ... - add a `[syncs.<name>]` table"

The config is valid but has no `[syncs.<name>]` table, so the CLI has nothing to run. Add a sync (see [Syncs](concepts/syncs.md)). A reload to a config without syncs is rejected the same way, and the running syncs continue.

### Name resolution fails on Linux (`*.local`, LDAP hosts)

The static Linux binaries don't use NSS. Use the server's IP address or a DNS name, or [build from source](installation.md#from-source).
