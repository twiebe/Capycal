# Quick start: desktop app

The desktop app runs the syncs of one config file in the background. This example copies a work calendar (for example an Exchange account added to Calendar.app) as blockers into a calendar of your iCloud account.

## 1. Prepare the calendars

In Calendar.app, add the accounts you want to sync (System Settings > Internet Accounts) and create an empty calendar for the blockers, here "Work Blockers" in iCloud.

## 2. Start the app

Start `Capycal.app` from Applications (see [Installation](../installation.md#macos-app) for the first start). A calendar icon appears in the menu bar; the app has no Dock icon. On the first start the app writes a commented-out starter config to `~/Library/Application Support/capycal/capycal.toml` and opens **Settings**.

## 3. Add the sync

Settings starts an assistant. Once you choose Calendar.app, it says why and macOS asks whether "Capycal" may access your calendars; allow it, so the lists can show them.

1. **Welcome to Capycal**: choose the card "Calendars in Calendar.app", then **Continue**.
2. **Copy events from**: the work calendar, for example "Calendar" under "Exchange". The calendars are listed under the account they belong to.
3. **Into**: "Work Blockers" under "iCloud". Calendars you may not write to are marked "read-only" and cannot be chosen.
4. **What should the copies show?**: the card "Busy blocker" (copies titled "Busy (Work)", after the source calendar). Each card shows how a copied event will look.
5. **Name the sync**: change the proposed name (made from the two calendars) to `work-to-blockers`, then **Create**.

For a CalDAV calendar (Nextcloud, Fastmail, ...) choose the card "A CalDAV account" in the first step (or go **Back** to it), or **Add Account…** in Settings later: server URL, user name and password (the password goes into the Keychain), then its calendars appear in the lists.

Settings writes the config for you, keeping it readable:

```toml
[calendars.exchange_calendar]
eventkit_calendar = "A1B2C3D4-..."

[calendars.icloud_work_blockers]
eventkit_calendar = "E5F6..."

[syncs.work-to-blockers]
source = "exchange_calendar"
target = "icloud_work_blockers"
interval = "5m"
transforms = [
  { field = "SUMMARY", action = "set_field", value = "Busy" },
]
```

You can still edit it by hand with **Open in Editor…** under Settings > General; the app reloads when the file changes.

## 4. Allow calendar access

If you have not allowed calendar access yet, macOS asks on the first sync. Allow it. If you dismissed the prompt, enable Capycal in System Settings > Privacy & Security > Calendars and choose **Sync Now**.

## 5. Check the status

The menu shows one line per sync, for example "work-to-blockers: synced just now". **Show Log** under Settings > General opens the live log if something goes wrong. Turn on **Start Capycal at login** in Settings > General to keep the syncs running after a restart. To take the blockers out of the target calendar again, use **Remove Synced Events**.

See [Desktop app](menu.md) for every menu item and [macOS and Calendar.app](../concepts/calendar-app.md) for what is synced from Calendar.app.
