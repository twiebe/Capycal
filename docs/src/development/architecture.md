# Architecture

Capycal follows a pragmatic ports-and-adapters layout. The crate is a library (`src/lib.rs`) plus three binaries: the `capycal` CLI (`src/main.rs`), the `capycal-ui` menu bar app for macOS (`src/bin/capycal-ui/`, feature `ui`) and the `capycal-app` tray app for Linux (`src/bin/capycal-app/`, feature `gtk`), which reuse the same sync engine. The domain core (iCal processing, sync loop, etag diff, UID mapping) knows nothing about CalDAV, EventKit, the file system or the config file format. Adapters plug into small traits (ports).

## Dependency rule

`ical/`, `sync/runner.rs`, `sync/diff.rs`, `sync/status.rs` and `state/mapping.rs` must not depend on `config`, the adapters (`caldav`, `eventkit`), `reqwest` or `std::fs`. Config is translated into domain types (`RunnerSettings`, `Transform`) at the edge, in the coordinator. Time is passed in as a parameter instead of read from the wall clock.

## Source tree

```
src/
  lib.rs               Library root: module tree and the public API for front ends
  main.rs              The `capycal` binary: parse the CLI and hand over to `cli::execute`
  bin/capycal-ui/       The menu bar app (feature `ui`, macOS only; a stub elsewhere)
    main.rs            Module gating, stub main on other platforms
    args.rs            -c / --config of both desktop apps
    app.rs             macOS glue: tao event loop, tray menu, dialogs (rfd)
    text_window_macos.rs macOS glue shared by the windows: toolbar row above a read-only text view, focus, close
    log_window_macos.rs macOS glue: the native AppKit log window
    panel_window_macos.rs macOS glue: header, buttons and text ("Problems")
    about_window_macos.rs macOS glue: About window like libadwaita's (built-in icon, links)
    dialog_window_macos.rs macOS glue: centered dialog (icon, heading, spinner, message, links, popup, buttons, footnote) changed in place
    backend.rs         Config, ConfigLock and Coordinator on the tokio thread; commands in, updates out; applies a SettingsChange (purge, keychain copy, ConfigEditor save, keychain cleanup, reload, resume)
    menu.rs            Menu model from a snapshot, status lines, relative time, menu bar icon state
    problems.rs        Problems window text: current problems in full plus recent warnings and errors
    dialog.rs          Dialog contents (heading, message, buttons, links, spinner) shared by the dialog models
    purge.rs           Remove Synced Events dialog model: counting, choosing the syncs, confirm, removing, result
    purge_all.rs       Remove All Events dialog model: counting, warning, confirm by typing the calendar's name, removing, result
    settings_view.rs   Settings window as a display-free view (SettingsView of typed controls with stable ids) and its input handling; the glue only draws it
    preview.rs         the Settings preview: a sample event and the copy a sync makes of it (SyncConfig::preview)
    settings_window_macos.rs macOS glue: sidebar and editor drawn from a SettingsView, updated in place while its shape stays
    settings.rs        Settings model: syncs and accounts of the config, selection, drafts, changes on disk; save and delete build one SettingsChange
    sync_form.rs       Sync form: texts, field errors, fields, transforms, what a save does to existing copies
    transform_form.rs  Transform forms: fields per action, parsing and errors, titles and summaries
    account_form.rs    Account form: URL, user name, password in the keychain (KeychainOp)
    picker.rs          Calendar picker: calendars per account and Calendar.app, matched with [calendars] entries, key proposals
    sponsor.rs         Ways to sponsor (OPTIONS, logos in assets/), when to ask, its dialog and answers
    logview.rs         Log view model (filter, follow, trimming, line colors), export
    autostart.rs       Start at Login: SMAppService, LaunchAgent fallback
  bin/capycal-app/      The Linux tray app (feature `gtk`), sharing the models above by #[path]
    app.rs             GTK glue: application, backend thread, actions, Problems, purge and sponsor dialogs
    tray.rs            StatusNotifierItem tray via ksni (Linux only)
    tray_icon.rs       Tray icon pixels: badge and fade per state, ARGB
    log_window.rs      Log window on a GtkTextBuffer, driven by logview
    settings_window.rs Settings window drawn from the shared SettingsView (DropDowns, entries, toggle buttons), updated in place
    text_window.rs     Heading, header buttons and read-only text ("Problems")
    dialog_window.rs   adw::Dialog built from dialog::View, changed in place
    about_window.rs    About on one page like the macOS window (tagline, links)
    menu_window.rs     The menu as a window: no tray, a second start, macOS
    icons.rs           App and symbolic icons for the icon theme when not installed
    autostart.rs       Start at Login: XDG autostart entry
  cli/
    mod.rs             CLI flags, dispatch, logging init, CliError, config loading, --validate, --only
    discover.rs        --discover (CalDAV) and --discover-eventkit, snippet formatting shared with the app
    generate.rs        --generate-config starter config
    purge.rs           --purge and --purge-all (with its confirmation)
    status.rs          --status: lock holder and per-sync state, read-only
    service.rs         --install-service / --uninstall-service: systemd user unit
    run.rs             --once, --full-sync and daemon mode with signal handling
  config_watch.rs      ConfigWatcher: debounced notifications when the config file changes
  logging.rs           LogBuffer tracing layer and file + buffer logging for front ends
  config.rs            TOML config types, validation, transform compilation, credential resolution
  config_migrate.rs    Config file versions: steps on the toml_edit document, backup and write back on load
  config_edit.rs       ConfigEditor: writes accounts, calendars and syncs into capycal.toml for the desktop apps (toml_edit, keeps comments and order, validates before an atomic save)
  source.rs            EventSource / EventSink ports
  error.rs             Error types
  caldav/
    client.rs          CalDAV HTTP client (PROPFIND, REPORT, PUT, DELETE, retry policy); discovery reads privileges and component sets
    auth.rs            ResolvedAuth (Basic auth credentials)
    source.rs          CalDavSource + CalDavSink (port implementations)
  eventkit/
    client.rs          macOS EventKit adapter (cfg(target_os = "macos")): reads/writes EKEvent
    convert.rs         Pure EventKit <-> iCal conversion, etag and notes marker (all platforms)
    tzid.rs            TZIDs NSTimeZone does not know: Windows and prefixed names, VTIMEZONE offsets (all platforms)
  ics/
    mod.rs             IcsSource: read-only iCalendar feed (ics_url), one GET split into events per UID
  ical/
    mod.rs             Processing pipeline, line unfolding, shared iCal helpers
    filter.rs          Field filtering (STRUCTURAL_FIELDS always, `fields` on top), VTIMEZONE carry-over
    timezone.rs        VTIMEZONE offsets: local <-> UTC for zones the event defines (buffers, unknown TZIDs)
    window.rs          Window: whether an event might have an occurrence in the days around now
    transform.rs       Transform (action: set, regex, remove, buffer, alarm, skip; conditions) and application
    marker.rs          X-CAPYCAL-SOURCE/SYNC stamping and detection
  sync/
    runner.rs          Generic sync loop (source -> transform -> sink) and full sync
    diff.rs            Etag-based changed/removed detection shared by sources
    status.rs          SyncStatus, CycleStats and the StatusReporter port
    coordinator.rs     Multi-sync management, shared semaphores, lifecycle, reload
    maintenance.rs     Purge: find a sync's sink events by marker, delete them, clear state;
                       the next source event for the Settings preview
    factory.rs         AnySource / AnySink, adapter construction (the one place to add a backend)
  state/
    mod.rs             SyncState and the StateStore port
    store.rs           Atomic JSON file store, moves corrupt files aside
    lock.rs            ConfigLock: one process per config, sync name collision check
    mapping.rs         Source UID -> sink UID/href mapping
```

## Library and binaries

The modules are private to the library. `lib.rs` re-exports only what a front end needs (`Config`, `Coordinator`, `Error`, ...), so the compiler still reports unused code inside the modules. The CLI lives in the library too (`cli::execute`), which keeps `main.rs` a few lines long. The library and the default build have no GUI dependencies; a GUI binary brings its own as optional dependencies behind a feature.

## Ports

```rust
pub trait EventSource: Send + Sync {
    /// Changed events and removed hrefs since the last sync.
    fn fetch_changes(&self, state: &mut SyncState)
        -> impl Future<Output = Result<(Vec<SourceEvent>, Vec<String>)>> + Send;
}

pub trait EventSink: Send + Sync {
    fn put_event(&self, href: &str, ical_data: &str)
        -> impl Future<Output = Result<PutResult>> + Send;
    fn delete_event(&self, href: &str) -> impl Future<Output = Result<()>> + Send;
    /// The events this sync created in the sink, found by marker.
    fn list_synced(&self, sync_name: &str, source_name: &str)
        -> impl Future<Output = Result<Vec<SinkEvent>>> + Send;
    /// Hrefs changed since `token` and a new token; None if the sink cannot tell.
    fn changes_since(&self, token: Option<&str>)
        -> impl Future<Output = Result<Option<SinkChanges>>> + Send { ready(Ok(None)) }
    /// Whether an in-flight put may be abandoned on shutdown (default false).
    fn cancel_safe(&self) -> bool { false }
}

pub trait StateStore: Send + Sync {
    /// None if missing, or corrupt and moved aside.
    fn load(&self, sync_name: &str) -> Result<Option<SyncState>>;
    fn save(&self, sync_name: &str, state: &SyncState) -> Result<()>;
    fn remove(&self, sync_name: &str) -> Result<()>;
}

pub trait StatusReporter: Send + Sync {
    fn report(&self, status: &SyncStatus);
}
```

The sync's `target` calendar in the config is the sink in the code. CalDAV and EventKit implement both `EventSource` and `EventSink`, so every combination works: CalDAV to CalDAV, EventKit to CalDAV, CalDAV to EventKit, EventKit to EventKit. A feed (`ics::IcsSource`) is only an `EventSource`: validation rejects it as a target and `build_sink` refuses it. `AnySource` / `AnySink` in `sync/factory.rs` are enums that dispatch to the concrete adapter, so the runner stays generic without `dyn`.

## Status and sync now

A running sync reports its `SyncStatus` (running flag, start time of the last cycle, and either the `CycleStats` with created/updated/deleted/error counts or the error message) through the `StatusReporter` port when a cycle starts and when it ends. A full sync also reports its progress in `SyncStatus::full_sync` (`FullSyncProgress`: changes written of a total that is known once source and sink are read) and counts finished full syncs in `full_syncs`, so the desktop apps can follow a requested full sync even when a normal cycle runs first. The coordinator implements the port with a `tokio::sync::watch` channel holding the status of all syncs; `Coordinator::status()` subscribes to it. `Coordinator::sync_now(name)` wakes one sync (or all with `None`) through a per-runner `Wake`, so the next cycle starts without waiting for the interval; `Coordinator::full_sync(name)` does the same and sets the `Wake`'s flag, so that cycle is a full sync. The CLI uses neither.

`Coordinator::pause(name)` stops one sync and remembers its name in memory; its status stays in the list with `paused` set. `start` and `reload` skip paused syncs and forget the names of syncs no longer configured, so a pause survives reloads but not a restart of the process. `Coordinator::resume` builds and spawns the runner again, sharing the sink semaphores of the running config.

## Markers and state as a cache

Every event Capycal creates carries a marker naming the sync and the source event:

- CalDAV (and iCal in general): `X-CAPYCAL-SOURCE:<source calendar>/<source uid>` and `X-CAPYCAL-SYNC:<sync>` (`ical/marker.rs`). Sink hrefs are deterministic (`capycal-<uuidv5(sync, source uid)>.ics`).
- EventKit keeps no custom properties, so the sink writes a notes line `[capycal:sync=<sync>;source=<source calendar>;uid=<source uid>]`, values percent-encoded where they contain `%`, `;`, `=`, `]` or line breaks (`eventkit/convert.rs`: `NotesMarker`, `find_marker`, `with_marker`). The legacy `[capycal:<source>]` line of older versions is still read everywhere; every put replaces all marker lines with the current one. Reading an EventKit event with either marker emits `X-CAPYCAL-SOURCE`, so a bidirectional setup skips it (loop prevention). A legacy marker names no sync, so purge and listing attribute it by source.

`EventSink::list_synced` returns the events of one sync from the sink (`SinkEvent`: href, sink UID if known, source UID from the marker, stored iCal if available). CalDAV lists the calendar, fetches it with a multiget and keeps the events with exactly this `X-CAPYCAL-SYNC` (`caldav::source::synced_events`, shared with purge). EventKit queries the sink calendar 20 years back and 4 ahead in chunks, deduplicated by identifier, and returns no UID and no content.

So the state file (`SyncState`: cursor, `UidMapping`, `pending_removals`, `last_full_sync`) is a cache: everything in it can be rebuilt from the source and the sink markers.

## Full sync

The runner's `cycle` picks an incremental cycle or a full sync (`full_sync_at`, internally the reconcile). A full sync runs when

- the state is missing or corrupt: `FsStateStore::load` moves a file it cannot parse to `<name>.json.corrupt-<unix seconds>` and logs a warning; without a state the runner sets `last_full_sync` to the Unix epoch, meaning "never";
- the local date of `now` (in `RunnerSettings::time_zone`, the coordinator passes `ical::transform::local_tz`) differs from that of `last_full_sync`: once a day. A state saved before the field existed (`None`) is set to the first cycle's `now` and saved, so an upgrade does not fully sync everything at once;
- it was requested: `Wake::full_sync` (`Coordinator::full_sync`, the app's Full Sync, `--full-sync` in daemon mode) or `SyncRunner::full_sync_once` (`--full-sync --once`);
- the last one did not complete: `full_sync_at` sets `SyncState::full_sync_pending` when it starts and clears it on completion, so a cancelled or failed full sync, even a requested one, runs again in the next cycle. A disabled sync gets `full_sync_pending` too (`mark_disabled` in the coordinator), so enabling it starts with a full sync;
- the sink changed elsewhere: `changed_elsewhere` asks `EventSink::changes_since(SyncState::sink_token)` at the start of a cycle and compares the changed hrefs by file name with the mapping's sink hrefs. One of them, or an expired token (`Error::SyncTokenExpired`), makes the cycle a full sync. After every cycle that wrote, `note_own_writes` moves the token past the cycle's own writes, so they don't count. The CalDAV sink answers with a sync-collection REPORT without `calendar-data` (`CalDavClient::sync_collection_hrefs`); a server without sync-collection and the EventKit sink answer `None`, which skips the check. This lets several computers run the same sync with their own state;
- the output settings changed: `RunnerSettings::output_key` (from `SyncConfig::output_key`: source and target calendar, `fields`, transforms, event window) differs from `SyncState::output_key`, which a completed full sync stores. A state without one takes the current key, so an upgrade does not fully sync everything at once.

Algorithm:

1. Fetch the source against an empty `SyncState`: without cursor and mapping, CalDAV (sync-collection without token or the ctag/etag fallback), EventKit and feeds (`EtagDiff` against no known etags, a feed without its HTTP validator) report every current event. Then `list_synced` the sink. Both are cancellable and change nothing.
2. `rebuild_mapping`: group the sink events by source UID (a legacy EventKit event without UID is attributed through the old mapping by sink href, compared by last path segment because CalDAV listings are absolute). Old entries stay unless the sink names another source UID for their event (they may point outside the listing range). Of several sink events for one UID keep the one in the old mapping, else the one at the deterministic href, else the first; the others and unattributed legacy events are "stray" and deleted directly. A rebuilt entry without source event gets the source href `orphan:<sink href>`, so its removal goes through the normal path.
3. Every mapped source UID that is no longer in the source, and every mapped event the transforms now reject, is removed with the usual guarantees: the mapping entry is dropped after a successful delete, a failed delete goes to `pending_removals`.
4. Every other source event is processed. `list_synced` returns with each sink event what the sink keeps of it (`EventSink::Stored`), and `EventSink::is_unchanged` tells whether a put would change it. If it would not, only the mapping is updated; otherwise the event is put to its mapped href (update in place) or created. CalDAV keeps the stored iCal and compares with `ical::same_content` (lines in order after unfolding, ignoring line endings, blank lines and `DTSTAMP`, `LAST-MODIFIED`, `CREATED`, `SEQUENCE`). EventKit keeps the fields `apply_update` sets (`convert::StoredEvent`, read while listing) and compares them with `convert::is_unchanged`: title, location, URL, notes including the marker line (a legacy marker counts as a change), start and end as instants (all-day boundaries as local dates, a `DURATION` end from the start), the all-day flag, alarms as a set, and availability only if the calendar supports it. A text the iCal leaves out (title, location, URL, notes) counts as empty, since a put clears it, as on CalDAV; times it leaves out are not compared, since a put keeps them.
5. Puts, deletes and stray deletes share `push` with the incremental cycle. On success the cursor from step 1 and `last_full_sync = now` are saved. If a put failed, the cursor stays empty, so the next cycle fetches everything again. A cancelled full sync saves its progress (mapping, pending removals) with an empty cursor, without `last_full_sync` and with `full_sync_pending`, so the next cycle runs it again and nothing is duplicated.

## Purge

`sync::maintenance::count_synced_events` and `purge_sync` find the events one sync created in its sink: on CalDAV by listing the calendar, fetching the events with a multiget and keeping those whose `X-CAPYCAL-SYNC` is exactly the sync's name; on EventKit by the notes marker naming the sync, or for a legacy marker the sync's source (`count_events_by_marker`, `delete_events_by_marker`, sharing `synced_events` with the sink listing). EventKit calls run in `spawn_blocking`, each inside an `autoreleasepool`. `purge_sync` deletes with the sink's `concurrency` (`delete_concurrently`), reports the number of handled events to a `Progress` callback (the menu bar app sends it throttled as `Update::PurgeProgress`), counts single failures and then removes the sync's state through the `StateStore`. `--purge` and the menu bar app share it; `Coordinator::purge` pauses the sync first and resumes it if the sink could not be read. `count_calendar_events` and `purge_calendar` do the same for every event of one calendar (CalDAV `list_events`, EventKit `count_all_events` / `delete_all_events`) and remove the state of every sync with that target; `--purge-all` and the menu bar app's Remove All Events share them, and `Coordinator::purge_calendar` pauses the syncs writing into the calendar meanwhile. `next_source_event` reads a sync's source with an empty state and returns the first event starting at or after now (by DTSTART), for the Settings window's preview.

## Config lock

Two processes running the same config would write the same state files and create duplicates. `ConfigLock::acquire(config, config_path)` takes an exclusive `flock` (`File::try_lock`) on `<state_dir>/locks/<hash>.lock`, where the hash is a truncated UUIDv5 of the canonical config path (stable across builds, unlike `std::hash`). The file holds pid, config path and sync names; the lock is released when the `ConfigLock` is dropped or the process exits, and the file is left in place. After locking its own file, a process probes the other lock files with a shared lock: one that is still held and names a sync of the same name means another config shares the state directory and a state file, so acquiring fails. Both processes write their own file before probing, so of two configs starting at once at least one sees the other. The CLI locks for daemon mode, `--once`, `--full-sync`, `--purge` and `--purge-all`; its sync names are recorded at start and not updated on reload. `ConfigLock::update` rewrites the names of a held lock (and checks them for collisions, keeping the old names on failure); the menu bar app calls it on every reload and holds the lock for its lifetime.

## Config watcher

`ConfigWatcher::new(path)` returns the watcher and a `tokio::sync::mpsc::Receiver<()>`; changes are reported while the watcher is alive. It uses `notify` (inotify on Linux, FSEvents on macOS), ignores access events (inotify reports the open of every reload, which would loop), debounces on a small thread (a change is reported 500 ms after the last event) and watches the parent directory non-recursively, filtering by file name. That way a replace by rename (how TextEdit, vim and VS Code save), deletion and re-creation are seen, which a watch on the file itself would miss after the first rename. Several changes in a row become one message (the channel holds at most one). The CLI daemon creates a watcher when `watch_config` is set and turns a change into the same reload as `SIGHUP`. The menu bar app always watches, creating the config directory first so a config written later by "Config > Edit" is picked up. Capycal never writes the config itself except the starter config, the commented calendar list of "Config > Edit" and a [config migration](#config-migration), whose reloads are harmless.

## Config migration

`config_migrate::migrate` brings a `toml_edit::DocumentMut` from its `version` (missing = 1) to `CONFIG_VERSION`, one function per step (`v1_to_v2`: removes `full_sync_interval`, renames the `eventkit_*` window keys of Calendar.app sources and removes them elsewhere), and returns notes of what changed. Steps work on the document, not the config types, so comments and order stay and old steps keep working when the types change. `Config::read` runs it before parsing and validates the result in memory; when something changed, it also returns a `ConfigUpdate`. `Config::load` is `read` plus `ConfigUpdate::write`, which copies the original to `<file>.<YYYYMMDD-HHMMSS>.bak` next to the real file (symlinks resolved, same permissions) and replaces the file through `config_edit::write_atomic`. A failing write is a warning: the migrated text is used in memory. The CLI reads and writes the update only for commands that run syncs or change state, after logging is up so the notes are logged; `--validate` and `--status` never write; `--migrate-config` only writes, for the systemd system unit's `ExecStartPre=-+`, as the sandboxed daemon may not write `/etc`. `ConfigEditor::load` migrates in memory only. A version above `CONFIG_VERSION` is an error naming the newer Capycal.

## Log buffer

`LogBuffer` is a tracing `Layer` that formats each event into a `LogLine` (timestamp, level, target and the full text with message and `key=value` fields), keeps the last N lines in a ring buffer and sends new lines through a `tokio::sync::broadcast` channel. `subscribe()` returns the backlog and a receiver under one lock, so a live log view has no gap or overlap; `snapshot()` is the export fallback when the log file cannot be read. `init_with_buffer(level, file, capacity)` installs the global logger of a front end: a `LogFile` (`default_log_file("capycal-ui")` is `~/Library/Logs/capycal/capycal-ui.log` on macOS) plus the buffer, and returns both.

`LogFile` is the file writer (`MakeWriter` over a `Mutex`; `tracing-appender` only rotates by time). Opening it rotates a non-empty file away, so each session starts a fresh file; a write that would push the file over `LOG_FILE_MAX_SIZE` (10 MB) rotates it first, so an event is never split across files. Rotation shifts `<file>` to `<file>.1`, `.1` to `.2` and so on and deletes `.5` (`LOG_FILES_KEPT`). The writer counts its rotations since start, so `session_content()` (the export of the menu bar app) concatenates `<file>.<rotations>` down to `.1` and the current file: the whole unfiltered session, without earlier sessions. It reads under the lock, so no rotation happens in between. Parts deleted by the retention are replaced by a note in the first line. The CLI keeps its own stdout logging (pretty or JSON).

## Menu bar app

`capycal-ui` is a driving adapter like the CLI: it only uses the library's public API (`Config`, `ConfigLock`, `Coordinator`, `LogBuffer`, ...). Its GUI crates (`tao`, `tray-icon`/`muda`, `rfd`, `objc2-service-management`, `objc2-app-kit`) are optional dependencies enabled by the `ui` feature and declared for macOS only, so `cargo build` never compiles them. On other platforms `capycal-ui` is a stub that exits with a message; its display-free modules still compile under `cfg(test)`, so their tests run on Linux.

Threading:

```
main thread (AppKit)                         "capycal-sync" thread (tokio runtime)
tao event loop                               Backend::run
  tray icon + muda menu  --- Command ------>   Config::load, ConfigLock, Coordinator
  log window (AppKit)    <-- Snapshot -----    status watch channel (throttled 300 ms)
  rfd dialogs                (EventLoopProxy user event)
```

- AppKit requires the event loop, the tray icon and the windows on the main thread. The tray icon is created on `StartCause::Init`. `ActivationPolicy::Accessory` (and `LSUIElement` in the bundle) hides the Dock icon.
- Menu clicks arrive through `MenuEvent::set_event_handler`, forwarded as user events. Menu item ids are stable strings (`menu::Action::id`), so the menu can be rebuilt from `menu::build(snapshot, start_at_login, now)` on every snapshot and every 30 seconds (for "synced N min ago").
- The app writes the starter config at start if the file is missing. A config without syncs is no `Problem`; the menu shows it as not configured yet.
- The backend owns config, lock and coordinator and handles `SyncNow`, `FullSync`, `Reload`, `EditConfig`, `CountPurge`, `Purge` (which disables the purged syncs in the config afterwards), `CountCalendar`, `PurgeCalendar` (the same for all syncs writing into the calendar), `SetEnabled`, the Settings commands (`DiscoverAccount`, `TestAccount`, `ListCalendarApp`, `SaveSettings`) and `Shutdown`. It answers with `Update`s: a `Snapshot` after every change, `ConfigChanged` after every reload, and `ConfigReady`, `PurgePreview`, `PurgeProgress`, `CalendarCounted`, `CalendarPurgeProgress`, `CalendarPurged`, `AccountCalendars`, `AccountTested`, `CalendarAppCalendars` and `SettingsSaved` for the commands that need an answer. `ListCalendarApp` calls EventKit (the access prompt may block for minutes), so it runs in `spawn_blocking`; keychain and credential lookups do too; counting and purging run in the command loop, with an `activity` line in the snapshot meanwhile. The confirmations stay on the main thread: Full Sync asks in a dialog before sending the command, Remove Synced Events opens its window before counting and enables Remove Events when the `PurgePreview` arrives. A missing, invalid or locked config is a `Problem` in the snapshot, not a fatal error: the app keeps running and "Config > Reload" or a change of the config file retries. A failed reload leaves the running syncs and the lock's sync names as they were.
- Log window: a native AppKit window (no WebKit, which would start helper processes and grow the memory). It is split into a platform-independent model and thin platform glue, so a later GTK window (`GtkTextView`) can reuse the model:
  - `logview::LogView` holds the received lines (at most the buffer capacity), the level and search filter and the follow state. Every operation returns a `Change`: reset the text, trim N UTF-16 units from the start, append runs of text with one `Tone` (error, warn, normal, dim) each, scroll to the end. New lines are only appended; a filter change or "Clear View" rebuilds. The view may grow to capacity plus a tenth before the oldest lines are trimmed, so trimming is not per line. Follow turns off when the view is scrolled up away from the end and on again when it is scrolled down to the end; resizing and trimming do not move the offset up, so they leave it alone.
  - `text_window_macos.rs` holds what the windows share: `TextWindow` creates the window with a toolbar row pinned to the top and `NSTextView::scrollableTextView` filling the rest (Auto Layout anchors), makes the text read-only and selectable, brings the window to the front (`focus`) and clears the delegate and closes the window when dropped. Each window keeps its own `define_class!` target and clears the targets of its controls in its `Drop`, before the `TextWindow` is dropped.
  - `panel_window_macos.rs` (`PanelWindow`) is a header, a row of buttons (reported as `PanelInput::Button(index)` through the sender's tag) and a monospaced text; `set_text` keeps the scroll position when the text is unchanged. "Problems" shows `problems::text` with Edit Config, Reload Config and Show Log, or an empty state (`problems::nothing_to_show`) without problems and recent warnings; the app refreshes it with the menu and opens it on `Update::ReloadFailed` (a config or lock error while loading). "Remove Synced Events" opens with "Counting synced events" and a disabled Remove Events button, sends `Command::CountPurge` and fills in `PurgePreview::confirmation` when the preview arrives; a preview for a window closed in the meantime is ignored. The text view is the first responder, so Cmd-A and Cmd-C work through the Edit menu.
  - `log_window_macos.rs` builds the log window on a `TextWindow` (toolbar row with `NSSearchField`, level `NSPopUpButton`, "Follow" checkbox, "Clear View" and "Export" buttons; the search field takes the free width with a minimum of 240 pt) and applies `Change`s to the text storage with `NSAttributedString` runs (monospaced system font, semantic colors for dark mode). One `define_class!` object (`CapycalLogTarget`) is the controls' target, the observer of `NSViewBoundsDidChangeNotification` on the clip view and the window delegate. It only sends a `logview::Input` as a user event; the event loop then reads the control values and calls the model, so nothing re-enters the window from an AppKit callback.
  - `LogBuffer::subscribe()` gives the backlog and a broadcast receiver. While the window is open the event loop wakes every 250 ms and drains the receiver; lagging behind the broadcast inserts a "lines skipped" marker. Closing the window (`windowWillClose:`) drops it with its views and the receiver; AppKit holds the target only weakly, and the drop clears the targets, the delegate and the observer. Showing it again starts from the backlog. Showing the window activates the app with `activateIgnoringOtherApps` and makes the window key (the search field is the initial first responder): an accessory app is not activated by opening a window, and a window that is not key takes no text input. `activate()` is not enough, since it may decline under cooperative activation (macOS 14).
- Quit hides the icon and windows, sends `Shutdown` and waits up to 35 seconds for the syncs to stop (longer than the coordinator waits for one sync).
- Start at Login uses `SMAppService.mainApp` inside an app bundle and falls back to a LaunchAgent plist (`app.capycal-ui`) otherwise or on error, logging which method was used.

Packaging lives in `packaging/macos/`: `build-app.sh` assembles `Capycal.app` (`capycal-ui` as `CFBundleExecutable`, the `capycal` CLI next to it, `Info.plist` with the calendar usage strings and `LSUIElement`) and signs it ad hoc; `build-dmg.sh` makes the release disk image.

Both apps share `dialog::layout` (margins, dialog width, text and log window sizes) and follow the platform guides for dialogs: the last button is the default unless `View::destructive`, which shows it in red and makes Cancel the default; dialogs have no window title, their heading says it; window titles name the menu item ("Log", "Problems", "Settings"). Errors on macOS use the app's dialog (`App::show_error`), the GTK app uses `adw::AlertDialog`. A `View::choice` is a popup above the buttons (`NSPopUpButton`, `gtk::DropDown`); choosing an item reports it as button `FIRST_CHOICE` plus its index, like the links with `FIRST_LINK`, so the dialog models handle it in `press`. `View::lines` follow the message as "label: text" with a bold label (`View::runs`; an attributed string on macOS, Pango markup in GTK).

## Linux desktop app

`capycal-app` (feature `gtk`, binary in `src/bin/capycal-app/`) is the same driving adapter with GTK 4 and libadwaita windows. It includes the display-free modules of `capycal-ui` with `#[path]` (`args`, `backend`, `dialog`, `logview`, `menu`, `problems`, `purge`, `sponsor`), so both apps share the menu model, icon state, Problems text, log view model and the dialog models; each uses only part of them, hence `allow(dead_code)` on those includes.

```
main thread (GLib main loop)                 "capycal-sync" thread (tokio)       ksni thread
adw::Application (held, no main window)      Backend::run                       StatusNotifierItem
  windows, dialogs  --- Command ------------>                                    + dbusmenu over D-Bus
  async_channel     <-- Update --------------                                   menu click -> Action
  async_channel     <-- Action, host online/offline ----------------------------
```

- The tray is a StatusNotifierItem (`ksni`, blocking API with its own runtime thread), Linux only. `Tray::update` replaces the menu items, icon state and tooltip; the D-Bus menu is updated in place, so an open menu stays open. A left click opens the menu (`MENU_ON_ACTIVATE`). The icon is a symbolic name per `menu::Icon` (`capycal-tray-*-symbolic`, SVGs with the GTK `error` and KDE `ColorScheme-*` classes, so GNOME and Plasma draw them in the panel's colors), with `icons::cache_dir` as theme path when not installed; the app icon as ARGB pixmaps with a red or blue dot (`tray_icon.rs`) is the fallback. `assume_sni_available` lets the app start before the panel; if no StatusNotifierWatcher appears within ten seconds, the app opens `menu_window.rs` (the `menu::build` items as libadwaita rows, rebuilt when they change) with a banner about the GNOME extension. The same window opens when the app is started a second time (GApplication `activate`) and, on macOS, at start.
- The GApplication id `app.capycal` makes the app single-instance. Windows (`log_window.rs`, `text_window.rs`, `settings_window.rs`) report back through callbacks that run on the main loop; `defer` moves their work to an idle callback, so a window callback never re-enters a borrowed `App`; Quit drops the windows (destroying them without close callbacks) before it waits for the syncs. The log window renders `logview::Change`s into a `GtkTextBuffer` with tags (it converts the model's UTF-16 trim counts to characters) and polls the broadcast receiver every 250 ms.
- Dialogs are an `adw::Dialog` (`dialog_window.rs`) built from `dialog::View`, since `adw::AlertDialog` closes on every response: Remove Synced Events, Remove All Events, Sponsor and the Full Sync question (`dialog::full_sync_view`, with the two circular arrows like the macOS app's `arrow.triangle.2.circlepath`). Errors and "already running" are `adw::AlertDialog`s. About is an `adw::Dialog` (`about_window.rs`) laid out like the macOS About window, so the tagline (`dialog::quip`) and the links (`dialog::about_links`) are on its only page; `adw::AboutDialog` puts both on a Details page.
- Icons: packages install them into `/usr/share/icons/hicolor`; otherwise `icons::install` writes the app icon and the symbolic SVGs (heart, calendar with minus, the GitHub mark of the sponsor link) into the user cache and adds it to the icon theme search path.
- Start at Login writes an XDG autostart entry (`autostart.rs`); the sponsor answers are a `key=value` file (`SponsorState::to_text`).
- The windows build on macOS too (Homebrew's gtk4 and libadwaita), without the tray, for developing them on a Mac.

## Sync algorithm

Each cycle is incremental unless a full sync is due (see [Full sync](#full-sync)):

1. **Fetch changes** from the source (RFC 6578 sync-token, ctag/etag diff, EventKit query, or a feed's conditional GET)
2. **Skip** events that carry a Capycal marker (prevents sync loops) and, with `RunnerSettings::window` (`SyncConfig::window`), events outside the days around `now` (`ical::window::Window::contains`: overlap of the event's span, a series until its `UNTIL`, local times read as UTC with a day of slack). A synced event that is skipped is removed like a filtered one; as an incremental cycle only sees changed events, the daily full sync moves the window
3. **Process** each event: skip a cancelled one (`transform::is_cancelled`, master VEVENT only; the runner then removes an earlier copy like a filtered event), apply the transforms to the whole event, each only where its conditions match (a condition can look at a property that is not copied; `add_alarm` only collects its alarm), filter fields (`filter::STRUCTURAL_FIELDS`, `fields` and the fields a matching `set_field` set), add the collected alarms, stamp markers, assign a deterministic sink UID
4. **Push** to the sink, limited by the per-calendar semaphore. If any put fails, the cursor from before the fetch (`sync_token`, `ctag`) is kept while the mappings of the successful puts are saved, so the next cycle fetches the batch again: a change log source reports the whole batch again (the events already put are updated in place through their mapping), a ctag/etag source diffs against the mappings and reports only the failed event. An event the sink keeps rejecting is fetched again every cycle until its put succeeds; it is counted in `CycleStats.errors` each time and does not block other events
5. **Delete** sink events whose source event was removed (the mapping is dropped only after a successful delete). Before the state changes, `defer_removals` stops a cycle that removes events when at least 5 are synced and the source now has less than half as many (synced minus removed plus new events; a full sync counts the fetched events) and sets `SyncState::removals_deferred` (a full sync checks the rebuilt mapping against the listing the same way and stays pending). The cycle succeeds with `CycleStats::deferred`; the next one that finds the events gone again removes them, so a source that is briefly empty without an error (Calendar.app loading at login) removes nothing, while a cleared calendar loses its copies one cycle later. The CalDAV client follows truncated sync-collection results (507) and rejects answers that are no multistatus. Failed deletes are kept in `pending_removals` and retried in the next cycle, because RFC 6578 sources report a removal only once
6. **Persist** state atomically (write to a temp file, then rename)

Between cycles `SyncRunner::run` waits `interval`. After a cycle that failed or counted `CycleStats.errors` it waits the next step of `ERROR_RETRY` (1, 2, 5, then 15 minutes, at most `interval`) instead, so an outage longer than the CalDAV client's own retries does not wait a whole interval; a successful cycle starts over. A cancelled cycle changes nothing.

### Cancellation

Shutdown (also the stop before a reload or a purge) cancels a running cycle, incremental or full, instead of waiting for it, since a CalDAV request in retry backoff can take over 30 seconds. The runner watches its shutdown receiver at these points and ends the cycle with `Error::Cancelled`, which leaves the last reported result unchanged:

- **Fetch**: the source runs against a copy of `SyncState` that replaces the state only on success. A cancelled (or failed) fetch is dropped with any request or backoff sleep in it and leaves the state exactly as before; nothing was written to the sink yet.
- **Waiting for a sink permit and between deletes**: no new put or delete starts.
- **In-flight puts**: aborted only if `EventSink::cancel_safe()` is true. CalDAV is: the sink href is derived from the sink UID (or taken from the mapping), so a put whose result was lost is repeated to the same resource. EventKit is not: a new `EKEvent` gets its identifier from EventKit on save, and a put dropped before its mapping is recorded would create a duplicate next cycle. Its puts are local and have no retry waits, so the runner simply lets them finish.
- **In-flight deletes**: always cancellable. Deletes are idempotent and the mapping is dropped only after success.

After a cancellation during the push phase the runner keeps the mappings of the completed puts, adds every delete that did not complete to `pending_removals`, restores the cursor from before the fetch (`sync_token`, `ctag`) and saves the state. The next cycle therefore fetches the unprocessed changes again: a change log source reports them again from the old token, a ctag/etag source sees the old ctag differ and diffs the etags against the mappings. Events that were already put are updated in place through their mapping.

The coordinator waits up to 30 seconds (`SHUTDOWN_TIMEOUT`) for a stopped sync's task and aborts it after that, so a hung task cannot save state while a reload's new runner or a purge works on the same sync. A panic inside a runner is caught (`CatchUnwind`), logged and reported as the sync's last result, so the sync is not shown as running forever.

## Feed specifics

- `IcsSource::fetch_changes` sends one GET with `If-None-Match` or `If-Modified-Since` from the validator it keeps in `SyncState::ctag` (`etag:` or `modified:` prefix). 304 means no changes; an empty state (full sync) never sends a validator.
- `split_feed` unfolds the body, rejects one without `BEGIN:VCALENDAR` (an HTML sign-in page), and groups the VEVENTs by UID, so a series and its overrides become one source event with the VTIMEZONEs its TZIDs name. The UID is the href. The etag is a v5 UUID of the content without `DTSTAMP` and `UID` lines, since feeds such as Google's stamp every event with the request time. Some feeds are generated on each request with new random UIDs: `keep_uids` gives an event whose UID is not in the mapping the UID (href and `UID` line) of a mapped event that is gone from the feed and had the same etag, in href order for identical events, so it counts as unchanged. `EtagDiff` against the mapping then gives changed and removed events. An event of such a feed whose content changed becomes a new copy plus a removal, and a full sync (empty mapping) replaces their copies once. Events without UID are skipped.
- The address is a `Credential` (`CalendarConfig::ics_url`), resolved in `build_source`. It is never logged: errors use `reqwest::Error::without_url` and name the calendar key, `{{source_url}}` is `ics://feed`, and `SyncConfig::output_key` only hashes it. A `user:password@` in the address becomes Basic auth (reqwest). `webcal://` is HTTPS.
- No retry loop: a failed GET fails the cycle, and the runner retries it after `ERROR_RETRY` (see [Sync algorithm](#sync-algorithm)).

## EventKit specifics

- Apple limits EventKit predicate queries to about 4 years. `all_events` uses chunked queries, each inside its own `autoreleasepool`. The source's `fetch_changes` runs the whole query in `spawn_blocking` inside an `autoreleasepool`, so it does not stall the tokio workers of other syncs and a shutdown can drop the cycle while it waits.
- As a source, recurring events are synced as individual occurrences without RRULE. When a single occurrence is deleted, EventKit simply stops returning it; an RRULE in the target would bring it back.
- As a sink, a recurring event becomes a native series: `convert::parse_series` turns the RRULE into a `Recurrence` (an `EKRecurrenceRule` in its terms), EXDATEs and cancelled overrides into occurrences to remove and the other overrides into occurrences to change. `client.rs` saves the master with the rule, then `apply_exceptions` finds each occurrence by its `occurrenceDate` and removes or changes it with `EKSpan::ThisEvent`. A put rewrites an existing series as a whole (deleted with `EKSpan::FutureEvents`, saved new), because EventKit keeps the exceptions of an existing series. Deletes and purge remove whole series. A full sync never skips a series (`StoredEvent::recurring`). Rules EventKit can't represent are logged and written as their first occurrence; RDATE is left out.
- Occurrence identity is `{calendarItemExternalIdentifier}_{start rounded to the minute}`.
- Timed events keep their timezone (`DTSTART;TZID=`) so local times survive DST changes. NSTimeZone only knows IANA names, so `parse_ical` and `parse_series` first run `eventkit/tzid.rs` (`resolve_timezones`): Windows names map to IANA zones, prefixed IANA names lose the prefix, other names defined by a VTIMEZONE become UTC values computed from its observances. `ical_to_nsdate` treats floating times and still unknown names as local time. `apply_update` also sets `EKEvent.timeZone` (`ical_time_zone`), without which Calendar.app keeps a timed event at the same wall-clock time in every zone; `convert::is_unchanged` treats a stored timed event without a zone (`StoredEvent::floating`) as changed, so a full sync fixes older copies. All-day events use local dates. EventKit ends an all-day event at the last second of its last day, iCal at the following midnight: `apply_update` writes the end one second before the iCal end, and `all_day_end` reads one second after the stored end, which gives the following day for both conventions.
- Alarms map to `VALARM` (relative or absolute `TRIGGER`), availability "free" maps to `TRANSP:TRANSPARENT`. `STATUS` is read-only in EventKit.
- capycal-created events are recognised by a `[capycal:sync=...;source=...;uid=...]` line in the notes (or the legacy `[capycal:<source>]`), because EventKit does not keep custom iCal properties. See [Markers](#markers-and-state-as-a-cache).
- Everything except the ObjC calls lives in `eventkit/convert.rs` and is unit tested on Linux.
