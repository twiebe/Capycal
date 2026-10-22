# CLI reference

```
capycal [OPTIONS]
```

| Option | Description |
|---|---|
| `-c`, `--config <PATH>` | Config file. Default: `~/.config/capycal/capycal.toml` (Linux), `~/Library/Application Support/capycal/capycal.toml` (macOS) |
| `-v`, `--verbose` | More log output: `-v` debug, `-vv` trace. Overrides `log_level`; `RUST_LOG` overrides both |
| `--once` | Run every enabled sync once and exit |
| `--only <NAMES>` | Run only these syncs (comma-separated), with the daemon, `--once` or `--full-sync` |
| `--full-sync` | Start every sync with a [full sync](../concepts/full-sync.md), then continue as daemon (or exit with `--once`) |
| `--validate` | Check the config and exit. Exits with `1` if it has no syncs |
| `--status` | Show whether a Capycal process runs the config and what each sync's state says, then exit |
| `--install-service` | Install, enable and start Capycal with this config as a systemd user service (Linux), see [User service](server.md#user-service) |
| `--uninstall-service` | Stop and remove that service (Linux) |
| `--generate-config` | Write a starter config to the config path and exit. Refuses to overwrite |
| `--discover <URL>` | List the calendars of a CalDAV account and print config snippets |
| `--discover-eventkit` | List Calendar.app calendars and print config snippets (macOS) |
| `--purge <SYNC>` | Delete the events a sync created from its target and forget its state |
| `--purge-all <CALENDAR>` | Delete **all** events from a calendar and forget the state of the syncs writing to it |
| `--yes` | With `--purge-all`: don't ask for confirmation |
| `--completions <SHELL>` | Print shell completions (`bash`, `zsh`, `fish`, `elvish`, `powershell`) |
| `--man` | Print the man page |
| `-V`, `--version` | Print the version |
| `-h`, `--help` | Show help |

Without `--once`, `--validate`, `--generate-config`, discovery or purge options, Capycal runs as a daemon: it syncs every `interval` until `SIGINT` (Ctrl-C) or `SIGTERM`, and reloads the config on `SIGHUP`.

## Exit codes

| Code | Meaning |
|---|---|
| `0` | Success. For `--once`: every sync completed |
| `1` | Any error: invalid config, a config without syncs (also for `--validate`), lock held by another process, a failed sync in `--once`, a purge or discovery that could not list the calendar. Single failed deletes during a purge are logged and counted but do not change the exit code |

Errors are printed to stderr or logged; with `log_format = "json"`, errors after config loading are JSON log lines.

## Running

```bash
capycal                         # daemon with the default config
capycal -c /etc/capycal/capycal.toml
capycal --once -v               # one cycle with debug output, e.g. from cron
capycal --once --only work-to-home
```

`--once` and the daemon take the [config lock](config-file.md#config-lock); a second process with the same config exits with code 1.

A daemon started with `--only` keeps the selection on every reload (`SIGHUP` or `watch_config`). If a selected sync is no longer in the reloaded config, the reload fails with `sync '<name>' selected with --only is no longer in the config` and the daemon continues with the previous config.

The daemon, `--once` and `--full-sync` need at least one `[syncs.<name>]` table. Without one they exit with code 1 and `no syncs defined in <path> - add a [syncs.<name>] table`. A reload to a config without syncs is rejected and the daemon keeps the previous config.

## Validate and generate

```bash
capycal --generate-config                 # writes the starter config to the default path
capycal --generate-config -c ./test.toml  # or elsewhere
capycal --validate                        # prints "configuration is valid" with the state directory
```

The [starter config](config-file.md#starter-config) is fully commented out: uncomment a calendar pair and a sync before running it.

`--validate` exits with code 0 only for a config the CLI can run. A config without syncs is valid (the desktop app accepts it), so it prints "configuration is valid" and then the "no syncs defined" error, and exits with code 1. This keeps `capycal --validate && systemctl restart capycal` from starting a daemon that would exit at once.

When no `-c` is given and the default config is missing or broken, the error includes the path that was looked at and a hint to run `--generate-config`.

## Status

```bash
capycal --status
```

```
config:  /home/alice/.config/capycal/capycal.toml
state:   /home/alice/.local/state/capycal
daemon:  running (pid 4242)

SYNC              FROM -> TO                 EVENTS  LAST CHANGE       LAST FULL SYNC
work-to-personal  work -> personal_blockers     128  2026-10-14 09:12  2026-10-01 08:00
personal-to-work  personal -> work_blockers       -  -                 -                 no state yet, starts with a full sync
```

`--status` reads the state files and the [config lock](config-file.md#config-lock) without changing anything or contacting a server, so it works while a daemon or the desktop app runs. Per sync it shows the number of synced events, when the state last changed (a cycle saves only when something changed), when the last [full sync](../concepts/full-sync.md) completed and notes such as an interrupted full sync or deletes waiting for a retry. Errors of the last cycles are in the log (`journalctl --user -u capycal` for the user service).

## Discover

```bash
capycal --discover https://cloud.example.org
capycal --discover-eventkit
```

`--discover` asks for username and password on the terminal, finds the account's calendars (via `/.well-known/caldav` and the principal's calendar home) and prints each calendar's name and URL to stderr, and to stdout an `[accounts.<name>]` snippet for the server (named after its host, with `password = { env = "CAPYCAL_<NAME>_PASSWORD" }`) plus a `[calendars.<name>]` snippet per calendar that refers to it. Task lists and other collections without events are left out. Redirect stdout to collect them: `capycal --discover https://... >> snippets.toml`.

`--discover-eventkit` lists every Calendar.app calendar with its identifier, grouped by account, and prints snippets in the same groups, named `<account>_<calendar>` and using the identifier, with the `Account/Name` form as a comment.

Neither needs a config file.

## Full sync

```bash
capycal --full-sync --once
capycal --full-sync --once --only work-to-home
```

Compares the whole source of each sync with its target: creates missing events, updates differing ones and removes Capycal's own duplicates, events whose source event is gone and events the current `fields` or transforms exclude. Nothing else in the target is touched, and the source never is. Use it after changing `fields` or transforms. Without `--once` the daemon starts with a full sync and then continues as usual.

The log line `full sync done` reports `took` (the duration), `created`, `updated`, `unchanged`, `deleted` and `duplicates` and `legacy_markers` (both included in `deleted`) per sync; with failed events it is a warning `full sync done with errors` that adds `errors`. Full syncs also run on their own, see [Full sync](../concepts/full-sync.md).

## Purge

```bash
capycal --purge work-to-home
capycal --purge-all blockers
```

`--purge <SYNC>` deletes the events this sync created from its target calendar, then removes the sync's state. Other events in the calendar are not touched:

- **CalDAV target**: Capycal fetches every event in the calendar and deletes those carrying the marker of exactly this sync (`X-CAPYCAL-SYNC:<sync>`).
- **Calendar.app target** (macOS): Capycal deletes the events whose notes marker names this sync. Events created by older Capycal versions carry only the source calendar (`[capycal:<source>]`); for these the source decides, so if two syncs read the same source and write to the same Calendar.app calendar, such old events of both are deleted. On other platforms the command fails with "EventKit calendars are only supported on macOS".

The source calendar is never touched. The sync must still be in the config, so purge before removing or renaming a sync. A daemon running the same config holds its lock, so stop it first; afterwards remove the sync from the config, or the next start creates the events again.

`--purge-all <CALENDAR>` deletes **every event** in the calendar, including ones Capycal didn't create, then removes the state of all syncs that write to it. It works for CalDAV and Calendar.app calendars. Only use it on calendars dedicated to Capycal. It asks you to type the calendar name first; in a script or without a terminal, add `--yes` instead (`capycal --purge-all blockers --yes`), otherwise it refuses.

Both use the calendar's `concurrency` for parallel deletes and report the number of deleted events and errors. Both take the [config lock](config-file.md#config-lock), so they exit with code 1 while a daemon or the desktop app runs the same config. In the desktop app, use **Remove Synced Events…** and **Remove All Events…** on the sync's page in Settings instead.
