# Option reference

Every option the config parser accepts. Durations use [humantime](https://docs.rs/humantime/latest/humantime/fn.parse_duration.html) syntax (`"30s"`, `"15m"`, `"1h 30m"`). Unknown keys are errors.

## Top level

| Option | Type | Default | Description |
|---|---|---|---|
| `version` | integer | `1` when missing | The format of the file. Capycal updates an older file when it loads it and refuses a newer one. Details: [Updates of the config file](../cli/config-file.md#updates-of-the-config-file) |

## `[daemon]`

The table and all its options are optional.

| Option | Type | Default | Description |
|---|---|---|---|
| `state_dir` | path | `$CAPYCAL_STATE_DIR`, else Linux `~/.local/state/capycal`, macOS `~/Library/Application Support/capycal/state` | State files and config locks. Wins over `CAPYCAL_STATE_DIR` |
| `log_level` | string | `"info"` | `trace`, `debug`, `info`, `warn`, `error` or a tracing filter directive. `RUST_LOG` and `-v` take precedence. Applied again on reload |
| `log_format` | `"pretty"` \| `"json"` | `"pretty"` | CLI log output format |
| `watch_config` | bool | `false` | CLI daemon: reload when the config file changes. The desktop app always does |

Details: [Daemon settings](../cli/config-file.md#daemon-settings).

## `[accounts.<name>]`

| Option | Type | Default | Description |
|---|---|---|---|
| `url` | string | required | Server address to discover the account's calendars on |
| `username` | string | required | HTTP Basic user name |
| `password` | credential | required | `"plain"`, `{ env = "VAR" }`, `{ file = "path" }`, `{ command = "..." }` or `{ keychain = "name" }`, see [Credentials](../concepts/calendars.md#passwords) |

Details: [Calendars](../concepts/calendars.md#accounts).

## `[calendars.<name>]`

| Option | Type | Default | Description |
|---|---|---|---|
| `account` | string | required for CalDAV | Name of the `[accounts.<name>]` whose credentials the calendar uses |
| `url` | string | required for CalDAV | CalDAV calendar collection URL |
| `eventkit_calendar` | string | - | macOS only. Calendar identifier, `"Account/Name"` or name. Makes this a Calendar.app calendar |
| `ics_url` | credential | - | Address of a read-only iCalendar feed (`https://`, `http://` or `webcal://`), as a string or any [credential](../concepts/calendars.md#passwords) form. Makes this a feed, usable only as a source; takes no `account` or `url`. Details: [Calendar feeds](../concepts/calendars.md#calendar-feeds-ical-address) |
| `concurrency` | integer | `2` | Maximum events written to this calendar at the same time as a target, shared by all syncs writing to it; also the parallel deletes of `--purge` / `--purge-all`. `0` counts as `1` |

Details: [Calendars](../concepts/calendars.md), [Credentials](../concepts/calendars.md#passwords).

## `[syncs.<name>]`

Each sync is a table named by its key, for example `[syncs.work-to-home]`. The name is non-empty, has no `/` or `\` and no leading `.`. Names with spaces, dots or other characters outside `A-Z a-z 0-9 _ -` need quotes: `[syncs."work to home"]`. Two syncs with the same name are a TOML error ("duplicate key"). Syncs run, log and show up in the desktop app in file order.

| Option | Type | Default | Description |
|---|---|---|---|
| `source` | string | required | Calendar key to read from |
| `target` | string | required | Calendar key to write to; must differ from `source` |
| `interval` | duration | required | Time between sync cycles |
| `fields` | list of strings | `[]` | Content properties to copy, such as `"SUMMARY"`, `"LOCATION"`, `"DESCRIPTION"`; `"VALARM"` keeps alarm components. `DTSTART`, `DTEND`, `DURATION`, `RRULE`, `RDATE`, `EXDATE`, `STATUS`, `TRANSP`, `UID`, `DTSTAMP` and `RECURRENCE-ID` are always kept. See [`fields`](../concepts/syncs.md#fields) |
| `enabled` | bool | `true` | `false` keeps the sync configured but stops it. See [Disabling a sync](../concepts/syncs.md#disabling-a-sync) |
| `full_sync_interval` | duration | `"7d"` | Time between automatic full syncs; must be greater than zero. See [Full sync](../concepts/full-sync.md) |
| `eventkit_days_past` | integer | `30` | Calendar.app source: days before now to query. Together with `eventkit_days_future` at most 1460 |
| `eventkit_days_future` | integer | `365` | Calendar.app source: days after now to query |
| `eventkit_all_events` | bool | `false` | Calendar.app source: query 20 years back and 4 years ahead in chunks |
| `transforms` | list of tables | `[]` | Transforms, run in order. See [Transforms](#transforms) below |

Details: [Syncs](../concepts/syncs.md).

## Transforms

A sync's transforms are the `transforms` list in its table, one inline table per transform:

```toml
[syncs.work-to-home]
# source, target, ...
transforms = [
  { field = "SUMMARY", action = "set_field", value = "Busy" },
  # comments are allowed between entries
  { action = "add_alarm", trigger = "10m" },
]
```

Multi-line lists of inline tables need TOML 1.1, which Capycal reads. Some editors and linters still only know TOML 1.0 and flag them. The TOML 1.0 form, one `[[syncs.<name>.transforms]]` table per transform, works too:

```toml
[[syncs.work-to-home.transforms]]
field = "SUMMARY"
action = "set_field"
value = "Busy"
```

A sync uses one form or the other; mixing both for the same sync is a TOML error.

| Option | Type | Used by | Description |
|---|---|---|---|
| `action` | string | all, required | `set_field`, `replace_field`, `replace_field_regex`, `remove_field`, `add_buffer`, `add_alarm`, `skip` |
| `field` | string | `set_field`, `replace_field`, `replace_field_regex`, `remove_field` (required) | iCal property name, e.g. `"SUMMARY"` |
| `value` | string | `set_field` (required) | New value, may contain placeholders |
| `pattern` | string | `replace_field_regex` (required) | Regular expression ([Rust regex syntax](https://docs.rs/regex/latest/regex/#syntax)) |
| `search` | string | `replace_field` (required) | Text to find, not empty |
| `replacement` | string | `replace_field`, `replace_field_regex` (required) | Replacement, may contain placeholders; for `replace_field_regex` also `$1` / `${name}` |
| `count` | integer | `replace_field` | Replace only the first `count` occurrences, default all |
| `pre_event` | duration | `add_buffer` | Move the start earlier |
| `post_event` | duration | `add_buffer` | Move the end later. At least one of the two is required |
| `trigger` | duration | `add_alarm` (required) | Alarm time before the start |
| `when` | list of conditions | all; `skip` (required) | The transform only changes events that match all of them; `{ any = [...] }` is one of them that matches when any of its conditions does |

Placeholders in `value` and `replacement`: `{{source_name}}`, `{{source_url}}`, `{{sync_name}}`. Details: [Transforms](../concepts/transforms.md).

### Conditions

`when` is a list of inline tables. In the table form, each condition is a `[[syncs.<name>.transforms.when]]` table after its transform. A condition has exactly one test: `contains`, `matches` or `empty` (with `field`), `days` (with optional `start_time` and `end_time`), or `all_day`. Or it is `{ any = [...] }`, at least two such conditions of which one has to match, with no other keys.

| Option | Type | Used by | Description |
|---|---|---|---|
| `any` | list of conditions | | At least two conditions, one of them has to match; nothing else in the same table, no `any` inside |
| `field` | string | `contains`, `matches`, `empty` (required) | iCal property name in upper case, e.g. `"LOCATION"` |
| `contains` | string | | The unescaped value contains this text, not empty |
| `matches` | string | | The unescaped value has a match of this regex, not empty |
| `empty` | boolean | | `true`: the property is missing or empty; `false`: it has a value |
| `days` | list of strings | | The event starts on one of `"mon"` ... `"sun"`, at least one |
| `start_time` | `"HH:MM"` | `days` | Window start, local time, default `"00:00"`; after `end_time` the window wraps around midnight |
| `end_time` | `"HH:MM"` | `days` | Window end (inclusive), local time, default `"23:59"` |
| `all_day` | boolean | | `true`: the event is all-day; `false`: it is not |
| `not` | boolean | `contains`, `matches`, `days` | Turn the test around, default `false` |

Details: [Conditions](../concepts/transforms.md#conditions).
