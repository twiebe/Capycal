# Config file

Capycal reads one TOML file. The desktop app writes it for you; this page is about the file itself. The CLI looks at the platform default path unless you pass `-c <path>`; the desktop app uses the same default (see [Another config file](../app/system.md#another-config-file) for another file).

| OS | Default config path |
|---|---|
| Linux | `~/.config/capycal/capycal.toml` |
| macOS | `~/Library/Application Support/capycal/capycal.toml` |

A config has these parts:

- **Accounts** (`[accounts.<name>]`): a CalDAV server and the credentials for it. Every CalDAV calendar names its account.
- **Calendars** (`[calendars.<name>]`): where events live. Each is a CalDAV collection or a Calendar.app calendar. The `<name>` is your own key; syncs refer to it and it appears in markers and the `{{source_name}}` placeholder.
- **Syncs** (`[syncs.<name>]`): which calendar is copied to which, how often, and which iCal properties are kept.
- **Transforms** (the `transforms` list of a sync): what happens to each event on the way, in order.

```toml
[daemon]
log_level = "info"

[accounts.work]
url = "https://caldav.example.com/"
username = "alice"
password = { env = "CAPYCAL_WORK_PASSWORD" }

[calendars.work]
account = "work"
url = "https://caldav.example.com/calendars/alice/work/"

[calendars.blockers]
account = "work"
url = "https://caldav.example.com/calendars/alice/blockers/"

[syncs.work-to-blockers]
source = "work"
target = "blockers"
interval = "5m"

transforms = [
  { field = "SUMMARY", action = "set_field", value = "Busy ({{source_name}})" },
]
```

What each part means, with its place in the desktop app: [Calendars and accounts](../concepts/calendars.md), [Syncs](../concepts/syncs.md), [Transforms](../concepts/transforms.md). Every key: [Config options](../reference/options.md).

## Starter config

`capycal --generate-config` and the desktop app (on its first start) write a starter config. Every setting in it is commented out, so it loads as a valid config without syncs. It explains and shows, ready to uncomment:

- every `[daemon]` option with its default,
- a CalDAV calendar with the three credential forms and a Calendar.app calendar with the accepted naming forms,
- a complete sync, including what `fields` keeps and the Calendar.app query window,
- one example of every transform action.

Lines starting with `##` explain, lines starting with `# ` are settings: remove the `# ` to use one. The CLI refuses to run the starter config as it is, since it has no syncs.

## Required parts

- The `[daemon]` table is optional; without it every daemon setting has its default.
- Every sync needs a name (its table key), `source`, `target` and `interval`. Without `fields` it copies busy blockers.
- The CLI needs at least one `[syncs.<name>]` table to run (see [CLI reference](reference.md#running)). A config without syncs is still valid, so the desktop app can show a config that is not set up yet.

Unknown keys are errors, at every level (top level, `[daemon]`, calendars, credentials, syncs, transforms). The message names the key, its line and the keys allowed there:

```text
error: configuration error: failed to parse /etc/capycal/capycal.toml: TOML parse error at line 16, column 1
   |
16 | intervl = "5m"
   | ^^^^^^^
unknown field `intervl`, expected one of `source`, `target`, `interval`, `fields`, ...
```

## Validating

```bash
capycal --validate            # default config
capycal --validate -c my.toml
```

This checks the TOML, unknown keys, required options, calendar references, durations, regexes, days, times, placeholders and the [Calendar.app query window](../concepts/syncs.md#calendarapp-query-window). A config without syncs is reported as valid, but `--validate` exits with code 1, since the CLI could not run it. It does not resolve credentials or contact a server; use `capycal --once -v` for that.

## Daemon settings

The `[daemon]` table holds process-wide settings. The table and every option in it are optional.

```toml
[daemon]
state_dir = "/var/lib/capycal"
log_level = "info"
log_format = "pretty"
watch_config = false
```

### `state_dir`

Directory for the per-sync state files (`<sync name>.json`) and the config locks (`locks/`). Precedence: `state_dir` in the config, then the `CAPYCAL_STATE_DIR` environment variable (if not empty), then the OS default:

| OS | Default state directory |
|---|---|
| Linux | `$XDG_STATE_HOME/capycal`, usually `~/.local/state/capycal/` |
| macOS | `~/Library/Application Support/capycal/state/` |

Services that run without a home directory need one of the first two. The [systemd system unit](server.md#systemd) and the [Docker image](server.md#docker) set `CAPYCAL_STATE_DIR=/var/lib/capycal`, so their configs need no `state_dir`. A relative path is resolved against the working directory of the process, not the config file, so use an absolute path.

The state remembers which source event belongs to which target event. It is a cache: if it is lost, the next cycle is a [full sync](../concepts/full-sync.md) that rebuilds it from the markers Capycal puts on every event it creates, on CalDAV and Calendar.app targets alike, without creating events twice. Only Calendar.app events written by old versions, whose marker names no sync, are deleted and created again. See [Troubleshooting](../troubleshooting.md#lost-or-corrupt-state).

### `log_level`

Default `"info"`. One of `trace`, `debug`, `info`, `warn`, `error`, or a [tracing filter directive](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/filter/struct.EnvFilter.html#directives) such as `"info,capycal=debug"`.

Precedence: the `RUST_LOG` environment variable wins over everything, then `-v` / `-vv` on the command line (CLI only), then `log_level`.

A changed `log_level` takes effect on a config reload, in the CLI daemon and in the desktop app, and is logged as "log level changed". If `RUST_LOG` or `-v` set the level at start, a reload doesn't change it.

### `log_format`

`"pretty"` (default) or `"json"`. Applies to the CLI's output on stdout. JSON gives one object per line for log collectors. The desktop app always writes plain text to its log file.

### `watch_config`

Default `false`. When `true`, the CLI daemon reloads the config by itself when the file changes, the same way as on `SIGHUP`. See [Reloading the config](#reloading-the-config). The option is read at start and ignored for `--once`. The desktop app always watches its config, regardless of this option.

## Reloading the config

A running Capycal applies config changes without a restart:

| How | CLI daemon | Desktop app |
|---|---|---|
| `SIGHUP` (`kill -HUP <pid>`, `systemctl reload capycal`, `docker compose kill -s HUP capycal`) | yes | - |
| File change with `watch_config = true` in `[daemon]` | yes | always on |
| **Reload Config** (Problems window, or Settings > General while another process holds the config) | - | yes |

On reload Capycal parses and validates the new file first. If it is invalid, the error is logged (and shown in the app's menu) and the running syncs continue with the previous config. If it is valid, all syncs are stopped and started again with the new config; each starts with a cycle right away. State files are kept, so nothing is synced twice.

The file watcher notices saves, replacements by rename (how most editors save) and deletion followed by re-creation, about half a second after the last write. The CLI logs "config changed, reloading".

A changed `log_level` applies after a successful reload, unless `RUST_LOG` or `-v` fixed the level at start. Not affected by a reload: `log_format` (set at start) and `watch_config` itself. The config lock follows a reload: the new sync names are recorded, and a changed `state_dir` takes the lock in the new directory. A reload whose sync names another config already runs in the same state directory, or whose new state directory is locked by another process, is rejected like an invalid config.

A CLI daemon started with `--only` applies the same selection to the reloaded config; a reload that no longer contains a selected sync is rejected like an invalid config.

Removing a sync from the config stops it but leaves its events in the target and its state file in the state directory. Run `capycal --purge <sync>` before removing it if the events should go too.

## Config lock

One config runs in one process at a time. Two processes with the same config would write the same state files and create duplicates. The daemon, `--once`, `--full-sync`, `--purge`, `--purge-all` and the desktop app take an exclusive lock for the config file; a second process with the same config stops with an error naming the config and the pid that holds it. The app shows "Another Capycal runs this config (pid N)" instead and keeps checking on reload.

Different configs can run in parallel. If they share a state directory and both have a sync with the same name, both would write `<name>.json`, so the second one fails to start with an error naming the conflicting config.

The lock files are in `<state dir>/locks/`, one per config (named after a hash of the config's absolute path). They record the pid and program of the holder, the config path and its sync names; `capycal --status` shows the pid, and a second desktop app quits when another one holds the lock. The lock is released when the process exits, even after a crash; the files themselves stay and are harmless.

`--validate`, `--generate-config` and the discovery commands don't take the lock. To purge a config that a daemon or the desktop app runs, stop it first; otherwise the purge fails with the same "already in use" error.
