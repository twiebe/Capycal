# Quick start: CLI

This walks through a first sync from one CalDAV calendar to another. For Calendar.app calendars, see the [desktop app quick start](../app/quick-start.md) or [Calendar.app to Nextcloud](../cookbook/calendar-app-to-nextcloud.md).

## 1. Create a config

```bash
capycal --generate-config
```

This writes a commented starter config to the default location and refuses to overwrite an existing file:

| OS | Config | State |
|---|---|---|
| Linux | `~/.config/capycal/capycal.toml` | `~/.local/state/capycal/` |
| macOS | `~/Library/Application Support/capycal/capycal.toml` | `~/Library/Application Support/capycal/state/` |

Use `-c <path>` with any command for a different config file.

## 2. Find your calendars

```bash
capycal --discover https://caldav.example.com
```

Capycal asks for username and password, looks up the CalDAV root (RFC 6764, `/.well-known/caldav`) and prints an `[accounts.<name>]` snippet for the server and one `[calendars.<name>]` snippet per calendar:

```toml
[accounts.work]
url = "https://caldav.example.com/"
username = "alice"
password = { env = "CAPYCAL_WORK_PASSWORD" }

[calendars.work]
account = "work"
url = "https://caldav.example.com/calendars/alice/work/"
```

On macOS, `capycal --discover-eventkit` does the same for Calendar.app.

## 3. Wire a sync

Copy the source calendar and the calendar that should receive the blockers into the config, then edit the sync:

```toml
[daemon]
log_level = "info"

[accounts.work]
url = "https://caldav.example.com/"
username = "alice"
password = { env = "CAPYCAL_WORK_PASSWORD" }

[accounts.nextcloud]
url = "https://cloud.example.org/remote.php/dav/"
username = "alice"
password = { env = "CAPYCAL_HOME_PASSWORD" }

[calendars.work]
account = "work"
url = "https://caldav.example.com/calendars/alice/work/"

[calendars.blockers]
account = "nextcloud"
url = "https://cloud.example.org/remote.php/dav/calendars/alice/work-blockers/"

[syncs.work-to-blockers]
source = "work"
target = "blockers"
interval = "5m"

transforms = [
  { field = "SUMMARY", action = "set_field", value = "Busy ({{source_name}})" },
]
```

Use a dedicated, empty calendar as the target. Capycal only touches events it created, but a separate calendar is easier to hide, share or clean up.

## 4. Check and run once

```bash
export CAPYCAL_WORK_PASSWORD=... CAPYCAL_HOME_PASSWORD=...
capycal --validate
capycal --once -v
```

`--validate` parses the config and checks references, transforms and placeholders without contacting a server. `--once` runs every sync one time and exits with a non-zero code if any sync failed; `-v` adds debug output.

## 5. Run as a daemon

```bash
capycal
```

Without `--once`, Capycal keeps running and syncs every `interval` until it receives `SIGINT` or `SIGTERM`. For a permanent setup, see [Running on a server](server.md).

Next: [Config file](config-file.md) and the [Cookbook](../cookbook/index.md).
