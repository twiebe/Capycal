# Removing events

## Remove Synced Events

**Remove Synced Events…** under Advanced settings > Troubleshooting on a sync's page removes what Capycal created for that sync, and keeps it from creating the events again:

1. A dialog opens at once and the app counts the sync's events, in the background ("Counting synced events" with a spinner in the dialog, also in the menu). **Cancel** or closing the dialog stops here. On a CalDAV target these are the events carrying the marker of exactly this sync; on a Calendar.app target the events whose notes marker names this sync (for events of older versions, which only name the source calendar: those of the sync's source).
2. The dialog then asks "Remove N events?" and names the sync, the number of events and the target calendar. The source calendars are never touched. If the target cannot be read, it says so and only offers **Done**. **Remove Events** is red and not the default button: Return and Esc pick **Cancel**.
3. On **Remove Events**, the app stops the sync, deletes its events (with the target calendar's `concurrency`), forgets its sync state and disables it (`enabled = false` in the config): the menu shows "work-to-home: disabled". Meanwhile the dialog shows "Removing synced events" with a progress bar ("264 of 1501 events removed"), **Show Log** and **Close**; closing it does not stop the removal.
4. When done, the dialog says how many events were removed, and any that could not be. Failures also show up in the menu and the Problems window.

The sync stays disabled, also after restarting the app, until you switch it on in Settings; it then copies its source again. Renaming or deleting a sync in Settings removes its events by itself. If the target calendar cannot be read, nothing is deleted and the sync keeps running.

This is the same as `capycal --purge <sync>` (see [CLI reference](../cli/reference.md#purge)), without stopping the app first.

## Remove All Events

**Remove All Events…** under Advanced settings > Troubleshooting on a sync's page deletes **every event** in the sync's target calendar, also those Capycal did not create. Use it only on a calendar that holds nothing but Capycal's copies, for example after copies of an old version or a broken config piled up there.

1. A dialog opens and counts the events in the target. An empty calendar or one that cannot be read ends here.
2. A warning in an orange box says how many events are deleted and lists the syncs writing into the calendar, which are switched off afterwards. **Continue…** goes on, **Cancel** stops.
3. A second warning asks you to type the calendar's name as Settings shows it. **Remove All Events** is enabled only once the name matches.
4. The app stops the syncs writing into the calendar, deletes the events with a progress bar, forgets their sync state and switches them off (`enabled = false`). Closing the dialog does not stop the removal. If the calendar cannot be read, nothing is deleted and the syncs keep running.

Switch the syncs on again in Settings to copy their sources from scratch. This is the same as `capycal --purge-all <calendar>` (see [CLI reference](../cli/reference.md#purge)), except that the CLI does not switch the syncs off.
