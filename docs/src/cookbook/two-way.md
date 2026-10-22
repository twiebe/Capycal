# Two-way blockers

Work and personal calendars block each other: private appointments appear as "Private" in the work calendar, work meetings as "Work" in the personal calendar. Both syncs write straight into the main calendars, so colleagues and family see the blockers without subscribing to anything.

**In the app:** **Add Sync…** twice, work to personal and personal to work, each with **Busy blocker**. Change the title in each sync's **Set field** step to "Work" and "Private", and add **Skip events** with **Event** set to **is all-day** with **+** to the second.

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
password = { env = "CAPYCAL_PERSONAL_PASSWORD" }

[calendars.work]
account = "work"
url = "https://caldav.work.example.com/calendars/alice/default/"

[calendars.personal]
account = "nextcloud"
url = "https://cloud.example.org/remote.php/dav/calendars/alice/personal/"

[syncs.work-to-personal]
source = "work"
target = "personal"
interval = "5m"

transforms = [
  { field = "SUMMARY", action = "set_field", value = "Work" },
]

[syncs.personal-to-work]
source = "personal"
target = "work"
interval = "5m"

transforms = [
  { field = "SUMMARY", action = "set_field", value = "Private" },
  { action = "skip", when = [{ all_day = true }] },
]
```

## Why this doesn't loop

Every event Capycal writes carries two extra properties:

```
X-CAPYCAL-SOURCE:work/<uid of the original event>
X-CAPYCAL-SYNC:work-to-personal
```

When `personal-to-work` reads the personal calendar, it skips every event with an `X-CAPYCAL-SOURCE` property. The "Work" blockers are therefore never copied back into the work calendar, and the same holds the other way round. In Calendar.app, which can't store custom properties, the marker is a `[capycal:sync=...;source=...;uid=...]` line at the end of the event notes.

Consequences:

- Edit or delete the original, not the blocker. A blocker is overwritten on the next change of its original. A blocker deleted or changed by hand in a CalDAV target is restored by the next cycle when the server supports sync-collection (see [One sync on several computers](../concepts/several-computers.md)), otherwise by the next full sync.
- Chains don't work: with A to B and B to C, the blockers from A in B are skipped and never reach C. Sync A to C directly.
- The CalDAV server must keep unknown `X-` properties. Common servers (Nextcloud, Radicale, Baikal, iCloud, Fastmail) do. If a server drops them, two-way syncs loop; use separate blocker calendars there, so no sync reads a calendar another sync writes to.
