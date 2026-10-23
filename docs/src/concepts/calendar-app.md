# macOS and Calendar.app

On macOS, Capycal reads and writes Calendar.app calendars through Apple's EventKit framework. Every account Calendar.app knows works: iCloud, Exchange, Google, CalDAV and local calendars. Capycal does not talk to Google or Exchange itself; Calendar.app does the syncing with the provider.

## Permissions

macOS asks for calendar access the first time Capycal reads Calendar.app. Who gets the permission depends on how Capycal runs:

| Started from | Permission belongs to |
|---|---|
| Desktop app | "Capycal" |
| Terminal, iTerm, ... | the terminal app |
| LaunchAgent | the `capycal` binary |

Change it in System Settings > Privacy & Security > Calendars. Capycal needs full access, since it writes events too. If the prompt is not answered within two minutes, the sync fails and is retried on the next cycle.

## What is read

A Calendar.app source provides these properties per event:

| iCal | From Calendar.app |
|---|---|
| `DTSTART`, `DTEND` | start and end, with the event's timezone (`TZID`); all-day events as dates |
| `SUMMARY` | title |
| `LOCATION` | location |
| `URL` | URL |
| `DESCRIPTION` | notes (without Capycal's marker line) |
| `TRANSP` | availability: "free" becomes `TRANSPARENT`, busy, tentative and unavailable become `OPAQUE` |
| `VALARM` | alarms, relative or absolute |

Attendees, organizer, status and recurrence rules are not available. Cancelled events are skipped.

### Recurring events

A Calendar.app source delivers recurring events as individual occurrences, without `RRULE`. Each occurrence becomes its own event in the target. The reason: when you delete a single occurrence of a series, EventKit simply stops returning it. A copied `RRULE` would bring it back in the target; single occurrences mirror exactly what Calendar.app shows.

### Query window

There is no change log in EventKit, so every cycle queries a time window around now: by default 30 days back and 365 days ahead (`days_past`, `days_future` on the sync). Events leaving the window are removed from the target.

EventKit returns nothing for a single query spanning more than about four years. Keep the sum of both options at or below 1460 days. `all_events = true` instead queries 20 years back and 4 years ahead in four-year chunks. See [Event window](syncs.md#event-window).

### Timezones and all-day events

Timed events keep their timezone (`DTSTART;TZID=Europe/Berlin:...`), so local times survive daylight saving changes. All-day events use local dates; their iCal end is the day after the last day, which Capycal converts from and to Calendar.app, where an all-day event ends at the last second of its last day. Time conditions of transforms compare in the Mac's timezone.

Events Capycal writes to Calendar.app carry the timezone of their source event (UTC for UTC times), so they stay at the right moment when you travel; all-day events and times without a timezone stay floating, like in the source. A full sync adds the timezone to timed events written by earlier versions.

When writing to Calendar.app, Capycal translates timezone names macOS doesn't know: Windows names such as `W. Europe Standard Time` (from Outlook and Exchange) become their IANA zone, and prefixed names such as `/freeassociation.sourceforge.net/Europe/Berlin` lose the prefix. Another unknown name is converted to UTC with the `VTIMEZONE` the event carries. Times without a timezone (floating) are local times; a name that still can't be resolved falls back to local time with a warning in the log.

## What is written

A Calendar.app target sets title, location, URL, notes, start, end, all-day flag, availability, alarms and recurrence from the synced event:

- `DTEND`, or `DURATION` if there is no `DTEND`, sets the end.
- `TRANSP:TRANSPARENT` sets availability "free", anything else "busy" (if the calendar supports availability).
- Alarms mirror the synced `VALARM`s: alarms of the target event are replaced on every update. Leave `VALARM` out of `fields` and don't use `add_alarm` for blockers without reminders.
- `STATUS` can't be set; EventKit treats it as read-only. Cancelled events are not copied anyway, and cancelled single occurrences of a series are removed from it.
- A recurring event (`RRULE`, from a CalDAV source) becomes a series in Calendar.app with the same rule. `EXDATE`s remove single occurrences, moved or changed occurrences (`RECURRENCE-ID`) are changed in the series. When the series changes, Capycal deletes it and writes it again as a whole, and a full sync always rewrites it.
- Calendar.app can't represent every `RRULE`: hourly or more frequent rules, `BYHOUR`/`BYMINUTE`/`BYSECOND` and several rules on one event are logged as a warning, and only the first occurrence is written. Extra dates (`RDATE`) are left out with a warning.
- Removing a series (deleted in the source, `--purge`, **Remove Synced Events**) removes all its occurrences.

### Marker

EventKit doesn't keep custom iCal properties, so Capycal marks the events it creates with a line `[capycal:sync=<sync>;source=<source calendar>;uid=<source event UID>]` at the end of the notes (values containing `%`, `;`, `=`, `]` or line breaks are percent-encoded). The marker prevents sync loops when the calendar is also a source, and lets a [full sync](full-sync.md) rebuild the state and purge find Capycal's events. Keep it when you edit such an event. Events written by older versions carry `[capycal:<source calendar>]`; Capycal still reads it and replaces it with the full marker the next time it writes the event.

## Calendar.app as source and target

Every combination works: Calendar.app to CalDAV, CalDAV to Calendar.app and Calendar.app to Calendar.app, for example from an Exchange calendar to an iCloud calendar in the same Calendar.app. See the [desktop app quick start](../app/quick-start.md) and the [Cookbook](../cookbook/index.md).
