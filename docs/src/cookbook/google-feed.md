# Google Calendar as blockers

Your Google Calendar appears as "Busy (google)" in your work calendar, without a Google password or OAuth: Capycal reads the calendar's secret iCal address. The same works with Outlook.com and any other service that publishes a calendar as an `.ics` address. See [Calendar feeds](../concepts/calendars.md#calendar-feeds-ical-address) for where to find the address.

**In the app:** in Google Calendar, copy **Settings for my calendars**, the calendar, **Integrate calendar**, **Secret address in iCal format**. Then in Capycal **Add Sync…**, **Add Calendar Feed…** below the calendars, paste the address into **Feed address** and **Continue**; pick the work calendar as the target and **Busy blocker**. In the sync's **From**, set **Check every** to **Custom** and `30m`.

**In the config:**

```toml
[daemon]
log_level = "info"

[accounts.work]
url = "https://caldav.work.example.com/"
username = "alice"
password = { env = "CAPYCAL_WORK_PASSWORD" }

# The secret address, kept out of the config file
[calendars.google]
ics_url = { env = "CAPYCAL_GOOGLE_FEED" }

[calendars.work]
account = "work"
url = "https://caldav.work.example.com/calendars/alice/default/"

[syncs.google-to-work]
source = "google"
target = "work"
# Google refreshes the feed only every few hours
interval = "30m"

transforms = [
  { field = "SUMMARY", action = "set_field", value = "Busy ({{source_name}})" },
]
```

What happens:

- Every 30 minutes Capycal downloads the feed and copies new, changed and removed events. Google itself updates the feed with a delay of several hours, so a new private appointment can take that long to block your work calendar. For changes within minutes, read Google through Calendar.app on a Mac instead ([Calendar.app to Nextcloud](calendar-app-to-nextcloud.md)).
- Without `fields`, only times, recurrence, status and free/busy are copied; an event you marked as "free" in Google stays free.
- The feed is read-only: Capycal never writes to Google. A feed cannot be the target of a sync.
- Anyone with the secret address can read the calendar. If it leaks, **Reset** it in Google Calendar and enter the new one.
