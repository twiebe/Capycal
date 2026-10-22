# Travel buffers and reminders

## Travel buffers

On-site appointments in the personal calendar need travel time. `add_buffer` blocks 30 minutes before and 15 minutes after each event in the work calendar:

**In the app:** **+** > **Add buffer** with 30 minutes before and 15 after; **Travel time** among the common steps fills in 30 minutes on both sides. **Skip events** with **Event** set to **is all-day** goes before it.

**In the config:**

```toml
[daemon]

[accounts.nextcloud]
url = "https://cloud.example.org/remote.php/dav/"
username = "alice"
password = { env = "CAPYCAL_PERSONAL_PASSWORD" }

[accounts.work]
url = "https://caldav.work.example.com/"
username = "alice"
password = { env = "CAPYCAL_WORK_PASSWORD" }

[calendars.personal]
account = "nextcloud"
url = "https://cloud.example.org/remote.php/dav/calendars/alice/personal/"

[calendars.work]
account = "work"
url = "https://caldav.work.example.com/calendars/alice/default/"

[syncs.personal-to-work]
source = "personal"
target = "work"
interval = "10m"

transforms = [
  { action = "skip", when = [{ all_day = true }] },
  { action = "add_buffer", pre_event = "30m", post_event = "15m" },
  { field = "SUMMARY", action = "set_field", value = "Private (incl. travel)" },
]
```

- All-day events are never buffered. They are dropped here, because a blocked whole day in the work calendar is rarely wanted.
- `post_event` extends `DTEND`. Events stored with `DURATION` instead of `DTEND` get a longer `DURATION`. Recurring events stay consistent: excluded and moved occurrences move with the series.
- Put a `skip` with a time condition before `add_buffer`, so it looks at the original times.

### Only events with a place

Travel time only makes sense for appointments somewhere. With conditions, the buffer goes only to events that have a location which is not a video call link; the others are still copied, without a buffer:

**In the app:** **Travel time for events at a place** among the common steps adds the buffer with **Location** **is not empty**. Open it, press **Add Condition**, choose **Location**, **does not contain** and type `https://`.

**In the config**, instead of the `add_buffer` above:

```toml
transforms = [
  { action = "skip", when = [{ all_day = true }] },
  { action = "add_buffer", pre_event = "30m", post_event = "15m", when = [
      { field = "LOCATION", empty = false },
      { field = "LOCATION", contains = "https://", not = true },
  ] },
  { field = "SUMMARY", action = "set_field", value = "Private (incl. travel)" },
]
```

An event without a location counts as empty. To leave out events without a place altogether, use `action = "skip"` with `{ field = "LOCATION", empty = true }` instead.

## Reminders on blockers

There are two ways to get alarms on the blockers:

**Keep the source's alarms**: add `"VALARM"` to `fields`. Every alarm of the original is copied, so the blocker reminds you exactly like the original.

**Add your own**: `add_alarm` adds a display alarm a fixed time before the (buffered) start. Repeat it for several alarms.

**In the app:** to keep the source's alarms, tick **Reminders of the source event** under **Keep** > **Custom** (**Title, notes, location, reminders** keeps them too). For your own, **+** > **Add alarm**, or **Reminder 10 min before** among the common steps.

**In the config:**

```toml
[daemon]

[accounts.work]
url = "https://caldav.work.example.com/"
username = "alice"
password = { env = "CAPYCAL_WORK_PASSWORD" }

[accounts.nextcloud]
url = "https://cloud.example.org/remote.php/dav/"
username = "alice"
password = { env = "CAPYCAL_PERSONAL_PASSWORD" }

[calendars.work]
account = "work"
url = "https://caldav.work.example.com/calendars/alice/default/"

[calendars.phone]
account = "nextcloud"
url = "https://cloud.example.org/remote.php/dav/calendars/alice/work-blockers/"

[syncs.work-to-phone]
source = "work"
target = "phone"
interval = "5m"

transforms = [
  { field = "SUMMARY", action = "set_field", value = "Work meeting" },
  { action = "add_alarm", trigger = "10m" },
  { action = "add_alarm", trigger = "1m" },
]
```

With both `"VALARM"` in `fields` and `add_alarm`, the blocker gets the source's alarms plus yours. Without either, blockers have no alarms, which is usually what you want for blockers in a calendar other people see.

For Calendar.app targets, the target event's alarms are replaced by the alarms of the synced data on every update.
