# Work calendar as private blockers

Your work meetings appear as "Busy (work)" in a calendar your family can see. No titles, locations, attendees or descriptions leave the work calendar.

**In the app:** **Add Sync…**, the work calendar as the source, the calendar your family sees as the target, and **Busy blocker**. The copies are titled "Busy (Work)" after the source calendar.

**In the config:**

```toml
[daemon]
log_level = "info"

[accounts.work]
url = "https://caldav.work.example.com/"
username = "alice"
password = { env = "CAPYCAL_WORK_PASSWORD" }

[accounts.nextcloud]
url = "https://cloud.example.org/remote.php/dav/"
username = "alice"
password = { file = "nextcloud.secret" }

[calendars.work]
account = "work"
url = "https://caldav.work.example.com/calendars/alice/default/"

# An empty calendar on the family Nextcloud, shared with the family
[calendars.work_blockers]
account = "nextcloud"
url = "https://cloud.example.org/remote.php/dav/calendars/alice/work-blockers/"

[syncs.work-to-family]
source = "work"
target = "work_blockers"
interval = "5m"

transforms = [
  { field = "SUMMARY", action = "set_field", value = "Busy ({{source_name}})" },
]
```

What happens:

- Without `fields`, only the times, the recurrence rules, the status and the free/busy flag are copied. `SUMMARY` is not copied; the `set_field` transform adds a new one.
- Events you marked as "free" stay free, so they don't block the family calendar. Cancelled meetings are not copied.
- Recurring meetings stay a single series in the target.
- `nextcloud.secret` is read from the config's directory.

If the work calendar is in Calendar.app (Exchange, Google), use [Calendar.app to Nextcloud](calendar-app-to-nextcloud.md) instead.

Variations:

- Add `fields = ["LOCATION"]` to see where a meeting takes place.
- Add `{ action = "skip", when = [{ all_day = true }] }` (in the app **Skip events** with **Event** set to **is all-day**) if all-day events (holidays, out-of-office) shouldn't block a whole day; see [`skip`](../concepts/transforms.md#skip).
