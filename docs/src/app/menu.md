# Menu and status

`Capycal.app` runs the syncs of one config without a terminal or LaunchAgent. It needs macOS 14 or later on Apple silicon. It lives in the menu bar: everything is in the calendar icon there. While one of its windows is open (Settings, Log, Problems, About), it also shows in the Dock and in Cmd+Tab, with a menu bar of its own: **Settings…** (Cmd+,), **Quit Capycal** (Cmd+Q), Edit for the text fields and Window (Cmd+W, Cmd+M). Closing the last window takes it out of the Dock again; a click on its Dock icon with no window open shows Settings. Quitting with unsaved changes in Settings asks first.

## Menu bar icon

The icon shows the overall state; hover over it for a one-line summary.

| Icon | State |
|---|---|
| Calendar | All syncs ran without errors |
| Calendar with a dot | A sync is running, or events are being removed |
| Calendar with a red dot | A sync failed or had failed events, or the config has an error. The menu shows which |
| Dimmed calendar | Starting, not configured yet, or all syncs disabled |

## Menu

| Item | What it does |
|---|---|
| Status lines | One line per sync, or the reason nothing runs (see below) |
| **Sync Now** | Start a cycle of every sync without waiting for its `interval`. Enabled while the config is loaded and at least one of its syncs runs (is not disabled) |
| **Settings…** | Everything you can set: syncs, accounts and, under **General**, starting at login, the log level and the config file (see [Settings](settings.md#the-window)) |
| **About Capycal** | App icon, version and a random tagline, with buttons for the website, reporting an issue and sponsoring, and **Acknowledgements…**: Capycal's license, its contributors and the licenses of the software it is built from |
| **Quit Capycal** | Remove the menu bar icon and close the windows at once, stop the syncs (waiting up to 35 seconds for a running one to finish its current step) and quit |

Repairs live in Settings: **Full Sync Now**, **Remove Synced Events…** and **Remove All Events…** on a sync's page under Advanced settings > Troubleshooting, the config file and **Show Log** on General, which also shows what keeps the config from running. A sync is switched off and on in Settings, with the switch next to its name. While events are counted or removed, Sync Now is disabled.

### Status lines

| Line | Meaning |
|---|---|
| `work-to-home: synced 2 min ago` | The last cycle succeeded |
| `work-to-home: synced 2 min ago, 3 failed` | The last cycle ran, but 3 events could not be written; they are retried every cycle |
| `work-to-home: syncing` | A cycle is running |
| `work-to-home: error: ...` | The last cycle failed, for example because the server was unreachable |
| `work-to-home: waiting` | Started, no cycle finished yet |
| `work-to-home: disabled` | `enabled = false` in the config, for example after Remove Synced Events; switching it on in Settings starts it again |
| `Not set up yet: open Settings…` | The config is valid but has no syncs, for example the starter config |
| `Counting synced events`, `Removing the events of work-to-home` | Remove Synced Events is running |
| `No config: open Settings… to set one up` | The config file doesn't exist (it was deleted after the start, or could not be written) |
| `Config error: ...` | The config is invalid. If syncs were running, they continue with the previous config |
| `Another Capycal runs this config (pid N)` | Another Capycal process runs this config (see [config lock](../cli/config-file.md#config-lock)) |
| `Removing events failed: ...` | A target calendar could not be read (the sync keeps running), some events could not be deleted (the sync is disabled), or the config could not be written to disable the sync (it is stopped until Capycal restarts; restart it once the config is writable) |

Lines that report a problem (a sync error, failed events, a config error, the lock, a failed removal, enable or disable) can be clicked and open the [Problems window](#problems-window).

## Problems window

The Problems window shows every current problem with its full message, for example a config error with the line, column and the offending part of the file, followed by the most recent warnings and errors of the log (for failed events, these name the events). It updates while it is open and says "No problems" once everything runs again.

- **Edit Config** opens the config in your editor
- **Reload Config** loads it again
- **Show Log** opens the [log window](#log-window)

When loading the config fails, at the start or after you edited the file, Settings opens on General and shows the mistake, so a typo in the config shows up right away. Sync errors don't open it, since a server that is briefly unreachable would otherwise open it again and again; the red menu bar icon and the status line show them.

## Log window

The log window shows the recent log lines and new ones as they arrive, colored by level.

- **Search**: show only lines containing the text (case-insensitive).
- **Level**: show only lines of this level and more severe ones (Error, Warn, Info, Debug; Debug includes trace lines).
- **Follow**: keep the newest line in view. Scrolling up turns it off, scrolling back to the end turns it on again.
- **Clear View**: empty the window. The log itself is not affected.
- **Export**: save the complete log of the current app session as a text file: all levels, ignoring search and level filter.

The window keeps the last 5000 lines. Which lines are logged at all depends on `log_level` in `[daemon]`; set `log_level = "debug"` and save the config for more detail. The app applies the new level when it reloads the config. `RUST_LOG`, if set when the app starts, overrides `log_level` and is not changed by a reload.

### Log files

The app writes its log to `~/Library/Logs/capycal/capycal-ui.log` (also visible in Console.app):

- Every app start begins a new file; the previous one becomes `capycal-ui.log.1`.
- A file is also rotated when it would exceed 10 MB.
- Rotation shifts `.1` to `.2` and so on. The last 5 rotated files are kept, older ones are deleted.

Export covers only the current session. If a very long session was rotated more than 5 times, the export starts with a note that older parts are gone.
