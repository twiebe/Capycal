# Weekends, nights and business hours

A [`skip`](../concepts/transforms.md#skip) with a [time condition](../concepts/transforms.md#time-conditions) leaves out events by weekday and start time in the local timezone of the machine running Capycal. These recipes block only the work hours of your private calendar in the work calendar: private appointments on weekends, at night or in the evening are nobody's business at work.

## Only business hours

Mon-Fri, 08:00-18:00. Everything that starts outside this window is left out:

**In the app:** add the sync with **Busy blocker**, then add the common steps **Skip weekends** and **Skip outside working hours** with **+**, and **Skip events** with **Event** set to **is all-day**.

**In the config:**

```toml
[daemon]
log_level = "info"

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
  # Weekends
  { action = "skip", when = [{ days = ["sat", "sun"] }] },
  # Weekdays before 08:00 and from 18:00: the window wraps around midnight
  { action = "skip", when = [{ days = ["mon", "tue", "wed", "thu", "fri"], start_time = "18:00", end_time = "07:59" }] },
  # Holidays, birthdays and other all-day entries
  { action = "skip", when = [{ all_day = true }] },
  { field = "SUMMARY", action = "set_field", value = "Private appointment" },
]
```

Notes:

- Only the start counts. A private appointment from 17:00 to 19:00 is copied and blocks until 19:00.
- `end_time` is inclusive, so `"07:59"` keeps an 08:00 start.
- A time condition with times never matches all-day events, not even with `not = true`, so holidays need their own `skip` with `all_day = true`.
- A recurring CalDAV series is checked by its first start: the whole series is kept or left out together. Calendar.app sources deliver every occurrence as its own event, so each occurrence is checked on its own.

## Nights

A window whose start is after its end wraps around midnight, so one condition covers the night:

**In the app:** one **Skip events** step with **Start**, every day ticked, **From** 22:00 and **Until** 05:59.

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

[calendars.family]
account = "nextcloud"
url = "https://cloud.example.org/remote.php/dav/calendars/alice/work-blockers/"

[syncs.work-to-family]
source = "work"
target = "family"
interval = "5m"

transforms = [
  # 22:00 - 05:59
  { action = "skip", when = [{ days = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"], start_time = "22:00", end_time = "05:59" }] },
  { field = "SUMMARY", action = "set_field", value = "Busy" },
]
```

## Timezone

The times are compared in the system timezone of the machine running Capycal. On a server in UTC, `start_time = "18:00"` means 18:00 UTC. Set the server's timezone, or for Docker mount the host's zone into the container (`/etc/localtime:/etc/localtime:ro`).
