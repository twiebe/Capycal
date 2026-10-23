# Syncs

Each `[syncs.<name>]` table copies events from one calendar to another. The key after `syncs.` is the name of the sync.

> **In the app:** **Add Sync…** asks for the source, the target and what to copy, and writes the table. The sync's page in Settings shows each key below as a step or under Advanced settings.

```toml
[syncs.work-to-blockers]
source = "work"
target = "blockers"
interval = "5m"
```

| Key | Required | Description |
|---|---|---|
| name (table key) | yes | Unique name. Used for the state file, markers, `--only`, `--purge` and the `{{sync_name}}` placeholder |
| `source` | yes | Calendar key to read from |
| `target` | yes | Calendar key to write to, different from `source` |
| `interval` | yes | Time between cycles, e.g. `"30s"`, `"5m"`, `"1h"` |
| `fields` | no | Content properties to copy on top of time, recurrence and availability (allow-list), default none |
| `transforms` | no | List of [transforms](transforms.md), applied in order |
| `all_events` | no | See [Event window](#event-window) |
| `days_past` | no | See below |
| `days_future` | no | See below |

## Name

> **In the app:** click the name at the top of the sync's page to rename it. Settings removes the copies in the target first, after asking, and copies the source again.

The name is the key of the sync's table: `[syncs.work-to-blockers]` names the sync `work-to-blockers`. It must not be empty, contain `/` or `\`, or start with a dot. Names with spaces, dots or other characters outside letters, digits, `_` and `-` need quotes: `[syncs."work to blockers"]`. TOML rejects two syncs with the same name. Syncs keep their order from the file. The name is part of every target event's UID and marker, so **renaming a sync** makes Capycal create its events again while the old ones stay. Run `capycal --purge <old name>` (or **Remove Synced Events** in the desktop app) before renaming.

## `interval`

> **In the app:** **Check every** in the **From** step: 1, 5 or 15 minutes, or **Custom**.

A [humantime](https://docs.rs/humantime/latest/humantime/fn.parse_duration.html) duration such as `"90s"`, `"5m"`, `"1h 30m"`. A cycle is cheap when nothing changed: CalDAV sources use RFC 6578 sync tokens or a ctag check, Calendar.app sources compare a local query. `"5m"` is a good default; the desktop app's **Sync Now** and a reload start a cycle immediately. After a cycle that failed or could not write every event, Capycal tries again sooner: after 1, 2, 5 and then every 15 minutes, never later than `interval`, until a cycle succeeds.

## `fields`

> **In the app:** the **Keep** step: **Only the time** (no `fields`), **Title**, **Title, notes, location, reminders**, or **Custom** with a checkbox per field.

Every copied event keeps what defines when it takes place and how it blocks time: `DTSTART`, `DTEND`, `DURATION`, `RRULE`, `RDATE`, `EXDATE`, `STATUS` and `TRANSP`, plus `UID` (replaced by Capycal), `DTSTAMP`, `RECURRENCE-ID` and the `VTIMEZONE` definitions the kept properties refer to. Dropping any of them would put the copy at the wrong time or turn a series into a single event, so they are not configurable.

`fields` is an allow-list of the content properties to copy on top (uppercase, as in the iCal data). Everything else is dropped after the [transforms](transforms.md) have run, except properties a `set_field` transform sets. Without `fields`, a sync creates busy blockers: the times, recurrence, status and free/busy flag of the original and nothing else. `"VALARM"` keeps the event's alarm components as a whole. Listing a structural property such as `"DTSTART"` is allowed and changes nothing.

Typical choices:

| Goal | `fields` |
|---|---|
| Busy blocker | leave it out |
| Keep the source's reminders | `["VALARM"]` |
| Copy the visible details | `["SUMMARY", "LOCATION", "DESCRIPTION"]` |
| Keep categories and privacy class | add `"CATEGORIES", "CLASS"` |

Notes:

- `TRANSP` keeps events marked "free" as free in the target. To show every blocker as busy, remove it with a [`remove_field` transform](transforms.md#remove_field).
- Recurrence: a recurring CalDAV event is copied as one series. Modified single occurrences are stored as extra `VEVENT`s with their `RECURRENCE-ID`. Calendar.app sources deliver every occurrence as its own event.
- Only events are copied. Tasks and journal entries (`VTODO`, `VJOURNAL`), which some servers such as Nextcloud keep in the same calendar, are skipped.
- Cancelled events (`STATUS:CANCELLED` on the event or the whole series) are not copied, and a copy Capycal made earlier is removed. A cancelled single occurrence of a series is copied as cancelled, so the series stays intact.
- Privacy: you don't need `SUMMARY` in `fields` to set a title. A `set_field` transform inserts it. Leaving `SUMMARY`, `DESCRIPTION`, `LOCATION`, `ATTENDEE`, `ORGANIZER` and `URL` out is the safe default for blockers.
- Invitations: a CalDAV target that handles invitations (scheduling, as in Nextcloud or Google) may email the attendees of a copy that keeps `ATTENDEE` or `ORGANIZER`. Copy them only into a calendar you are sure about. In the app, **Keep** > **Custom** shows a warning while **Attendees** or **Organizer** is ticked.
- Calendar.app sources only provide `DTSTART`, `DTEND`, `SUMMARY`, `LOCATION`, `URL`, `DESCRIPTION`, `TRANSP` and `VALARM`. See [macOS and Calendar.app](calendar-app.md) for what Calendar.app targets write.

## How an event is processed

For each new or changed source event, every cycle:

1. Events that carry a Capycal marker are skipped, so blockers are never copied back. Cancelled events are skipped too, and an earlier copy is removed. Tasks and journal entries in the same calendar are never copied.
2. The transforms run in order on the whole source event, so they can look at properties the sync does not copy. A [`skip`](transforms.md#skip) can leave the event out here.
3. Time, recurrence, status and availability (`DTSTART`, `DTEND`, `DURATION`, `RRULE`, `RDATE`, `EXDATE`, `STATUS`, `TRANSP`), `UID`, `DTSTAMP`, `RECURRENCE-ID`, the properties in `fields` and those a `set_field` set in this event are kept. Timezone definitions referenced by the kept properties are carried over. Alarms of `add_alarm` are added after this step.
4. Capycal adds its markers, replaces the UID with one derived from the sync name and the source UID, and writes the event to the target.

When a source event is deleted or leaves the [event window](#event-window), its copy in the target is deleted too.

Changing `fields` or transforms only affects events that change afterwards, until the next automatic full sync. To apply the new rules to all events now, run a [full sync](full-sync.md).

## Event window

> **In the app:** **Events** in the From step: **All**, or **Range** with the days back and ahead.

A sync can copy only the source events around today:

| Key | Default | Description |
|---|---|---|
| `all_events` | `true`, `false` for a Calendar.app source | Copy every source event, ignoring the two options below |
| `days_past` | `30` | Days before now to include |
| `days_future` | `365` | Days after now to include |

An event is in the window when its time overlaps it. A recurring event is copied as a whole series if it might have an occurrence in the window: it starts before the window ends and its `RRULE` does not end (`UNTIL`) before the window starts. A series without an end, or with `COUNT` or `RDATE`, counts as running on, so a yearly event that started years ago is copied even when none of its occurrences falls into a short window. Times in a time zone count as UTC and the window is a day wider on each side, so no event at the edge is lost.

Events that leave the window count as removed and their copies are deleted from the target. Events that enter it, as the window moves on with today, are copied. A CalDAV source or a feed reports only events that changed, so both happen with the daily [full sync](full-sync.md); a Calendar.app source is read in full every cycle, so there they happen at once.

How a source applies the window:

- **Calendar.app** has no change log, so Capycal queries the window every cycle. That is why it has a window by default: Calendar.app lists a recurring event as single occurrences, which for all events would be thousands. `days_past + days_future` must be at most 1460 days: Calendar.app returns nothing for a single query over about four years, so a larger window is rejected as a config error. Use smaller values, or `all_events = true`, which reads 20 years back and 4 years ahead in several queries.
- **CalDAV calendars and feeds** are read in full and the events outside the window are left out. There is no limit on the window.

Versions before 0.3.0 had these options only for Calendar.app sources, as `eventkit_days_past`, `eventkit_days_future` and `eventkit_all_events`; the [config update](../cli/config-file.md#updates-of-the-config-file) renames them.

## Disabling a sync

> **In the app:** the switch next to the sync's name.

`enabled = false` keeps a sync in the config but stops it:

```toml
[syncs.work-to-home]
source = "work"
target = "home"
interval = "5m"
enabled = false
```

A disabled sync is still validated, so its calendars and transforms must be correct. The daemon starts no runner for it, `--once` skips it and `--status` shows it as disabled. `--purge` and "Remove Synced Events" in the app still remove its events. The first cycle after enabling it again is a full sync, so whatever changed in the meantime is picked up.

## Two syncs between the same calendars

Two syncs in opposite directions (A to B and B to A) are safe: events created by Capycal carry a marker and are never synced back. See [Two-way blockers](../cookbook/two-way.md).
