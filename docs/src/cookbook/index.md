# Recipes

Common setups, each first as the steps in the desktop app's Settings, then as a complete config. Each config passes `capycal --validate`; replace URLs, user names and calendar names with yours. Recipes that use Calendar.app calendars only validate on macOS.

- [Work calendar as private blockers](work-blockers.md): your family sees "Busy" when you are in meetings.
- [Two-way blockers](two-way.md): work and personal calendars block each other, without loops.
- [Weekends, nights and business hours](time-filters.md): copy only the events that matter on the other side.
- [Travel buffers and reminders](buffers-and-reminders.md): block travel time, add or keep alarms.
- [Calendar.app to Nextcloud](calendar-app-to-nextcloud.md): an Exchange or Google calendar through Calendar.app to a CalDAV server.
- [Calendar.app as target](eventkit-target.md): private CalDAV events as blockers in an Exchange calendar.
- [Google Calendar as blockers](google-feed.md): a Google or Outlook.com calendar through its secret iCal address, without OAuth.

All recipes use a dedicated, empty calendar or the main calendar of the other side as the target. Capycal only changes and deletes events it created itself, recognised by their marker.
