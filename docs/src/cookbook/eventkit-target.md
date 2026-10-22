# Calendar.app as target

Calendar.app can also receive events. This is the way to write blockers into a calendar that Capycal can't reach over CalDAV, such as an Exchange calendar your colleagues see. Here private appointments from Nextcloud appear as "Private" in the work calendar:

**In the app:** **Add Sync…** with the Nextcloud calendar as the source and the Exchange calendar under its account as the target, then set the title in the **Set field** step to "Private".

**In the config:**

```toml
[daemon]
log_level = "info"

[accounts.nextcloud]
url = "https://cloud.example.org/remote.php/dav/"
username = "alice"
password = { file = "nextcloud.secret" }

[calendars.personal]
account = "nextcloud"
url = "https://cloud.example.org/remote.php/dav/calendars/alice/personal/"

# Your Exchange calendar in Calendar.app
[calendars.work]
eventkit_calendar = "Exchange/Calendar"

[syncs.personal-to-work]
source = "personal"
target = "work"
interval = "10m"

transforms = [
  { field = "SUMMARY", action = "set_field", value = "Private" },
]
```

What to know about Calendar.app targets:

- Title, location, notes, start, end, all-day, availability, alarms and recurrence are written. A recurring CalDAV event becomes a series in Calendar.app, with its excluded and moved occurrences. Rules Calendar.app can't represent (hourly, `BYHOUR`, several rules) are written as their first occurrence only, with a warning in the log. See [What is written](../concepts/calendar-app.md#what-is-written).
- Capycal marks its events with a `[capycal:sync=...;source=personal;uid=...]` line at the end of the notes. Don't remove it; it prevents [loops](two-way.md#why-this-doesnt-loop) and lets a [full sync](../concepts/full-sync.md) and purge find the events.
- The target event's alarms are replaced with the alarms of the synced data on every update. With neither `"VALARM"` in `fields` nor `add_alarm`, blockers have no alarms.
- `TRANSP:TRANSPARENT` becomes availability "free"; everything else is "busy".
- `--purge <sync>` (or **Remove Synced Events** in the desktop app) deletes the events of a sync by this marker. `--purge-all` empties a dedicated calendar.
- Calendar.app sends the events to Exchange, Google or iCloud as it normally would. Your Mac has to be running for the blockers to update.

This config only validates on macOS.
