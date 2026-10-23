# Full sync

A normal cycle only processes what changed in the source since the last one. A full sync compares the whole source with the target:

> **In the app:** **Full Sync Now** under Advanced settings > Troubleshooting starts one. Capycal runs one by itself once a day.

1. Read every current event of the source in the sync's [event window](syncs.md#event-window).
2. Find the events this sync created in the target by their marker. The state is rebuilt from these markers, so the state files are a cache that can be lost.
3. Create missing events and update events whose content differs. Remove Capycal's own events that are duplicates, whose source event is gone, or that the current `fields` and transforms exclude. Events Capycal did not create are never touched, and neither is the source.

## When events disappear from the source

When events are gone from the source and it now has less than half as many events as were synced (from 5 synced events on), Capycal waits one cycle before it removes their copies. A source can be briefly empty or incomplete without an error, for example Calendar.app still loading its accounts right after login. If the events are back in the next cycle, nothing was removed; if they are still gone, for example because you cleared the calendar, the next cycle removes them. A source that replaced its events with as many new ones is not held back. The log says "most synced events are gone from the source, removing them in the next cycle", and the desktop app's menu shows "synced ..., N to remove next cycle". Failed or incomplete answers of the source (errors, cut-off listings) never remove anything.

## Unchanged events

An event whose stored content is unchanged is not written again. On a CalDAV target the comparison ignores the `DTSTAMP`, `LAST-MODIFIED`, `CREATED` and `SEQUENCE` lines, line folding and line endings. On a Calendar.app target Capycal compares the fields it writes (title, location, URL, notes with the marker, start, end, all-day flag, alarms and availability; times as instants, alarms in any order), so unchanged events keep their modification date and are not uploaded to the account again. An event with an old-style marker is written again to update the marker.

## When it runs

A full sync runs:

- when a sync has no usable state: its first start, after the state directory was lost, or when the state file is corrupt (it is moved aside, see [Troubleshooting](../troubleshooting.md#lost-or-corrupt-state));
- once a day: on the first cycle of a day (local time of the computer, UTC in the container image) after the last full sync was on an earlier day. A computer that was off runs it on its first cycle. State files from versions before full syncs count from their first cycle, so an upgrade doesn't run every full sync at once;
- after a change of the sync's source or target calendar, `fields`, transforms or event window, so existing copies get the new content and a new target gets every event. A change of `interval` alone does not trigger one. Copies in an old target stay there: run `capycal --purge <sync>` before you change the target or rename the sync;
- on the first cycle after a disabled sync is enabled again;
- when this sync's events in a CalDAV target were changed or removed by someone else: another computer running the same sync, a purge there, or a hand edit (see [One sync on several computers](several-computers.md));
- on request: `capycal --full-sync` (see [CLI reference](../cli/reference.md#full-sync)) or **Full Sync Now** in the desktop app (Settings, the sync's Advanced settings > Troubleshooting).

Versions before 0.3.0 had `full_sync_interval` to set how often it runs; the [config update](../cli/config-file.md#updates-of-the-config-file) removes it. A full sync of a CalDAV target downloads every event of the target calendar once a day, which also notices copies changed by hand on targets that cannot report changes (Calendar.app, CalDAV servers without sync-collection). A full sync stopped by a shutdown or reload, or one that failed, keeps its progress and is not recorded; the next cycle is a full sync again, also when it was only requested.

Calendar.app targets: Capycal marks its events with a notes line naming the sync, the source calendar and the source event. Events written by older versions carry only `[capycal:<source>]`. A full sync rewrites the marker of such events when the state knows them; without state it deletes them and creates them again with the full marker. If two syncs read the same source into the same Calendar.app calendar, an old-style event of one sync is not in the state of the other, so the first full sync of the other may delete it; its own sync creates it again at its next full sync.
