# Calendar.app to Nextcloud

Your employer's Exchange (or Google Workspace) calendar is in Calendar.app on your Mac, but not reachable over CalDAV. Capycal reads it through EventKit and writes blockers to a Nextcloud calendar. Run it with the [desktop app](../app/menu.md) or a [LaunchAgent](../cli/server.md#launchd-headless-macos).

**In the app:** **Add Sync…**, pick the Exchange calendar under its account in the source list (the app asks for calendar access first) and the Nextcloud calendar as the target. The window of days is **Calendar.app events** under Advanced settings.

**In the config:**

```toml
[daemon]
log_level = "info"

# The Exchange account's calendar as shown in Calendar.app
[accounts.nextcloud]
url = "https://cloud.example.org/remote.php/dav/"
username = "alice"
password = { file = "nextcloud.secret" }

[calendars.work]
eventkit_calendar = "Exchange/Calendar"

[calendars.nextcloud]
account = "nextcloud"
url = "https://cloud.example.org/remote.php/dav/calendars/alice/work-blockers/"

[syncs.work-to-nextcloud]
source = "work"
target = "nextcloud"
interval = "5m"
days_past = 7
days_future = 180

transforms = [
  { field = "SUMMARY", action = "set_field", value = "Busy ({{source_name}})" },
]
```

- Find the exact `eventkit_calendar` value with `capycal --discover-eventkit`. The account name ("Exchange" here) is the one in Calendar.app's sidebar; use the identifier if names are ambiguous.
- Calendar.app delivers recurring events as single occurrences, so the target gets one event per occurrence within the window.
- The window here is one week back and six months ahead, which keeps each cycle fast. Blockers older than a week are removed from Nextcloud.
- `nextcloud.secret` is read from the config's directory (`~/Library/Application Support/capycal/` by default). Make it readable only by you.

This config only validates on macOS. On Linux, `--validate` reports `eventkit_calendar is only supported on macOS`.
