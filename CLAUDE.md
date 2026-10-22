# CLAUDE.md

Guidelines for working on Capycal.

## Language

- All code, comments, commit messages, documentation, and file content must be in **English**
- Conversations with the user may be in German, but all artifacts are English

## Style

- Never use mdashes (--). Use a regular dash (-) instead
- Keep code lean. Don't add features, abstractions, or error handling beyond what's needed
- Prefer editing existing files over creating new ones
- Don't add docstrings or comments to code you didn't change
- Only add comments where the logic isn't self-evident

## After every new feature

- Update the user documentation in `docs/src/` (mdBook, see `docs/src/SUMMARY.md`): app windows in `app/`, CLI, config file and server in `cli/` (flags in `cli/reference.md`), shared concepts in `concepts/` (transforms in `concepts/transforms.md`) where every option names its place in Settings (`> **In the app:** ...`) and its key, recipes in `cookbook/` with the app steps before the TOML, every key in `reference/options.md`. Moving a page needs a redirect in `docs/book.toml`. Check it with `make docs`
- Update `README.md` only if the feature list, install summary or quick start changes; details belong in the book
- Update `docs/src/development/architecture.md` if modules, ports or the sync algorithm changed
- Add tests for the new feature
- Document new options in `capycal.example.toml`
- Update `CLAUDE.md` if architecture or conventions changed
- Changed a window, dialog, menu or their texts: follow `docs/src/development/ui-guidelines.md` (model first, both renderers, spacing scale, accent / orange, no red, "…", errors on leaving a field, undo over questions, keyboard and accessibility labels), run the `/ui-review` skill before committing, and add a new pattern to the guidelines. `settings_view::tests::ui_rules` checks the mechanical rules

## Rust conventions

- Edition 2024
- Run `cargo fmt` before committing
- Run `cargo clippy -- -D warnings` - zero warnings policy. That covers only the default build: after touching the apps also run `make clippy-ui` / `make clippy-gtk` (`make lint` runs fmt check, `clippy` and `clippy-ui`)
- All tests must pass: `cargo test`, plus `make test-ui` / `make test-gtk` for the apps
- Use `thiserror` for error types
- Use `tracing` for logging (not `log` or `println!`)
- Async runtime: `tokio`
- HTTP client: `reqwest`
- Config format: TOML via `serde` + `toml`

## Architecture

Moving incrementally toward ports & adapters (no crate split, generics instead of `dyn`).

- The crate is a library (`src/lib.rs`) plus thin binaries. `src/main.rs` is the `capycal` CLI and only calls `cli::execute`
- Show the version as `capycal::VERSION` (from `build.rs`: the crate version, plus the commit unless HEAD is tagged `v<version>`), not `CARGO_PKG_VERSION`; that stays only where a plain version is needed (User-Agent). `make release-tag VERSION=x.y.z` bumps, commits and tags a release
- Modules are private in `lib.rs`; expose items to binaries through `pub use` re-exports, not `pub mod`, so dead code is still caught. The one exception is `pub mod cli`, which `main.rs` uses directly
- No GUI dependencies in the library or the default build. The `capycal` CLI must keep working without them
- Ports: `src/source.rs` defines `EventSource` / `EventSink` (put, delete, `list_synced`: the sync's events in the sink by marker, `changes_since`: hrefs changed in the sink since a token, default `None`); `src/state/mod.rs` defines `SyncState` and the `StateStore` trait (load/save/remove per sync; `load` returns `None` for a missing or corrupt state)
- CalDAV implementation: `src/caldav/` (client, auth, source)
- Feed implementation: `src/ics/` (`IcsSource`, source only): `ics_url` (a `Credential`, the address is a secret: never log it, `{{source_url}}` is `ics://feed`) is fetched with a conditional GET (validator in `SyncState::ctag`), split by UID into source events and diffed with `EtagDiff` on a content hash without `DTSTAMP` and `UID`; `keep_uids` maps new random UIDs of a regenerated feed back to gone mapped events with the same hash. Validation rejects a feed as target, `build_sink` and the purge helpers refuse it
- EventKit implementation: `src/eventkit/` - `client.rs` is the ObjC adapter (macOS only, behind `cfg(target_os = "macos")`), `convert.rs` the pure converter compiled on all platforms
- Sync pipeline: `src/sync/runner.rs` (generic over source, sink and store; configured via `RunnerSettings`). The config calls the sink `target` (`SyncConfig::target`); code and ports keep "sink", user-facing text (docs, logs, errors) says "target"
- Shutdown cancels a running cycle (see "Cancellation" in `docs/src/development/architecture.md`). A sink may only return `true` from `EventSink::cancel_safe` if repeating a put to the requested href updates the same event
- Adapter factory: `src/sync/factory.rs` defines `AnySource` / `AnySink` (enum dispatch over CalDAV, feeds (source only) and, on macOS, EventKit) and `build_source` / `build_sink`. This is the single place to add a backend
- CLI: `src/main.rs` only parses flags and calls `cli::execute`, which initializes logging and dispatches to `src/cli/` (`discover`, `generate`, `purge`, `run`, `service`, `status`). Commands return `CliResult`; `cli::execute` is the only place that reports a `CliError` and picks the exit code - never call `process::exit` elsewhere. CLI code builds CalDAV clients via `sync::factory::caldav_client` and touches state only through `StateStore`
- Composition root: `src/sync/coordinator.rs` maps `SyncConfig` to `RunnerSettings` and builds one `SyncRunner<AnySource, AnySink, FsStateStore>` per sync via `build_runner`
- iCal processing: `src/ical/` (filter, transform, marker, unfold). `ical::filter::STRUCTURAL_FIELDS` (identity, time, recurrence, `STATUS`, `TRANSP`) are always kept; `fields` only adds content properties and is optional. `process_event` returns `None` for a cancelled master VEVENT (`transform::is_cancelled`), so the runner removes the copy like a filtered event; a cancelled override keeps its `STATUS`
- Shared iCal helpers (`property_name`, `extract_tzid`, `valarm_lines`, `format_utc`, `PRODID`) live in `src/ical/mod.rs` - reuse them instead of re-implementing. VTIMEZONE offsets (`vtimezones`, `Zone::to_utc` / `to_local`) live in `src/ical/timezone.rs`; `add_buffer` shifts TZID times through them, or through `chrono_tz` for an IANA TZID without VTIMEZONE (Calendar.app sources)
- Etag change detection: `src/sync/diff.rs` (`EtagDiff`, fed from `UidMapping::source_etags`) is shared by the CalDAV ctag fallback and the EventKit source - don't re-implement changed/removed detection in adapters
- Keychain: `src/keychain.rs` (`keyring-core` with the native store per platform: `apple-native-keyring-store`, `zbus-secret-service-keyring-store` on async-io, no libdbus/OpenSSL; service `app.capycal`); `Credential::Keychain` reads through it; writes go through `set_password` / `delete_password` / `rename_entry`, and `would_prompt` checks without a prompt whether macOS would ask (re-exported as `*_keychain_*`). Tests use `keychain::use_test_store`, never the real store; `make test-keychain` checks the real one
- State persistence: `src/state/store.rs` (`FsStateStore`, atomic JSON files)
- Sync status: `src/sync/status.rs` (`SyncStatus`, `StatusReporter` port); the coordinator publishes it through a `watch` channel (`Coordinator::status`) and wakes runners via `Coordinator::sync_now`
- State is a cache: every sink event carries a marker naming sync and source event (CalDAV: `X-CAPYCAL-SOURCE` / `X-CAPYCAL-SYNC`; EventKit: the notes line `[capycal:sync=...;source=...;uid=...]`, legacy `[capycal:<source>]` still read and replaced on put). Never add state that cannot be rebuilt from the source and the sink markers
- Full sync (internally reconcile, `SyncRunner::full_sync_at`): full source fetch with an empty cursor plus `list_synced`, rebuilds the mapping, removes duplicates/orphans/rejected events, skips puts the sink reports as `EventSink::is_unchanged` (on what `list_synced` captured as `EventSink::Stored`: CalDAV the iCal via `ical::same_content`, EventKit the written fields via `convert::is_unchanged`). Triggered by a missing/corrupt state, `full_sync_interval` (`SyncState::last_full_sync`), changes of this sync's copies made elsewhere (`EventSink::changes_since` from `SyncState::sink_token`, CalDAV sync-collection without data; the token moves past own writes after each cycle; lets several computers run one sync), a changed `RunnerSettings::output_key` (`SyncConfig::output_key`, stored as `SyncState::output_key` after a completed full sync; a state without one takes the current key), `Coordinator::full_sync` / `Wake::full_sync` and `--full-sync`. It is the only full resync; nothing clears the state to resync
- Removal guard: when at least `MASS_REMOVAL_MIN` synced events exist, events are removed and the source now has less than half as many (synced - removed + new), the runner waits one cycle (`SyncState::removals_deferred`, reported as `CycleStats::deferred`) and removes them only if the next cycle finds them gone again. A briefly empty source (Calendar.app loading at login) keeps its copies. Don't turn this back into an error that needs a purge. Deletes that fail are retried next cycle from `SyncState::pending_removals`
- Purge: `sync::maintenance::{count_synced_events, purge_sync}` find a sync's sink events by marker (CalDAV: `X-CAPYCAL-SYNC` of exactly this sync; EventKit: the notes marker of this sync, legacy markers by source) and are shared by `--purge` and the menu bar app; `count_calendar_events` / `purge_calendar` (every event of a calendar, state of the syncs writing into it) are shared by `--purge-all` and Remove All Events (`Coordinator::purge_calendar` pauses those syncs). `sync::maintenance::next_source_event` feeds the Settings preview's "Next event"
- Config lock: `src/state/lock.rs` (`ConfigLock`, one process per config, sync name collision check). Commands that sync or clear state must hold it
- Config writer: `src/config_edit.rs` (`ConfigEditor`) writes the config types back with `toml_edit`; one table per change, header comment and position kept, defaults left out, `save` validates and writes atomically through symlinks with the old permissions. The apps never write TOML text themselves
- Config watcher: `src/config_watch.rs` (`ConfigWatcher` on `notify`, watches the parent directory, filters by file name, ignores access events to avoid reload loops, debounces itself). Used by the CLI daemon (`watch_config`) and always by the menu bar app; a change takes the same path as SIGHUP / "Reload Config"
- Front-end logging: `src/logging.rs` (`LogBuffer` layer, `init_with_buffer`). The CLI keeps its own stdout logging init in `src/cli/mod.rs`. Both put a reloadable `EnvFilter` (`logging::level_filter`) first on the registry and apply `log_level` after a successful reload through the returned `LogLevel`; `RUST_LOG` (and `-v`) disable that
- Menu bar app: `src/bin/capycal-ui/` (feature `ui`). GUI deps are optional, macOS-only target deps enabled by `ui`; on other platforms `main` is a stub and only the display-free modules compile (`cfg(any(target_os = "macos", test))`). The backend gets `Command`s and answers with `Update`s; blocking EventKit work (access prompt) goes to `spawn_blocking`, never the main thread. New windows build on `text_window_macos::TextWindow` (toolbar row, read-only text view, focus, close on drop) and clear their control targets in `Drop`; a text with a header and buttons (Problems) is a `panel_window_macos::PanelWindow`; a question or progress without a long text is a `dialog_window_macos::DialogWindow` showing a `dialog::View` from a display-free model (`purge.rs`, `purge_all.rs`, `sponsor.rs`, `keychain_notice.rs`)
- Paused syncs: `Coordinator::pause` / `resume` / `purge`; the paused set lives only in memory and is only the mechanism of a purge (the app then writes `enabled = false`; a sync is switched on and off in Settings, not in the menu). `enabled = false` (`SyncConfig::enabled`) is the persistent switch: validated, no runner, `SyncStatus::disabled`, skipped by `--once`; `mark_disabled` sets `full_sync_pending` so enabling starts with a full sync
- Dependency rule: domain modules (`ical/`, `sync/runner.rs`, `sync/diff.rs`, `sync/status.rs`, `state/mapping.rs`) must not depend on `config`, concrete adapters, `reqwest` or `std::fs`.
- Config: `src/config.rs`. Every config struct has `#[serde(deny_unknown_fields)]` (`Credential` has a hand-written `Deserialize` for the same reason), so a new struct needs it too. `[daemon]`, `accounts`, `calendars` and `syncs` may be missing. `[accounts.<name>]` hold url and credentials; a calendar is Calendar.app with `eventkit_calendar`, a feed with `ics_url`, else CalDAV; every CalDAV calendar references one with `account` (required, no credentials on calendars); credentials resolve in `sync::factory::caldav_client`. A config without syncs is valid for the library (the app shows it as not set up); the CLI refuses to run it (`cli::require_syncs`). Syncs are `[syncs.<name>]` tables: `deserialize_syncs` reads them in file order (needs the `toml` feature `preserve_order`) and takes `SyncConfig::name` from the key, so duplicate names are a TOML error; the old `[[syncs]]` array fails with an error explaining the new format. Transforms are a `transforms = [...]` list of inline tables (TOML 1.1) or `[[syncs.<name>.transforms]]` tables
- Transforms: `ical::transform::Transform` is an `Action` (typed enum, each variant carries only its validated data) plus `when: Vec<Vec<Condition>>` (`Test::{Contains, Matches, Empty, TimeRange, AllDay}`, `negate`), shared by every action; every group must have a matching condition (a plain condition is a group of one, `{ any = [...] }` in the config a larger one, never nested), checked on the current master VEVENT, unescaped. `skip` is the only filter. `config.rs` converts `TransformConfig` (`when: Vec<ConditionConfig>`, `ConditionConfig::any`, `ConditionConfig::tests` for the flat list) into it via `SyncConfig::compile_transforms`, the single place for transform validation (it also rejects a condition on a property an earlier unconditional `set_field` / `remove_field` fixed). Adding an action means: `TransformAction` variant, an `Action` variant, its arm in `TransformConfig::compile`, and its arm in `apply_transforms`. `process_event` runs the transforms on the whole source event, then filters by `fields` plus `Applied::set_fields` (only the properties a matching `set_field` set: a set whose conditions did not match must not let the source value through), then adds `Applied::alarms`, so source alarms still need `"VALARM"` in `fields`
- Time is injected: the runner reads the clock once per cycle and passes `now` into `ical::process_event`

## Platform considerations

- EventKit ObjC code (`src/eventkit/client.rs`) is gated with `#[cfg(target_os = "macos")]`; `src/eventkit/convert.rs` has no objc dependencies and must stay that way
- ObjC calls in EventKit require `unsafe` blocks
- Every EventKit operation must run inside `objc2::rc::autoreleasepool` to prevent memory leaks
- EventKit objects (`EKEventStore`, `EKEvent`) are not `Send + Sync` - don't store them in structs that cross thread boundaries
- Linux builds must always work without EventKit dependencies

## Menu bar app (capycal-ui)

- Main thread: tao event loop, tray icon (created on `StartCause::Init`), windows and rfd dialogs. Sync work: `backend::Backend` on a tokio runtime in a background thread. They talk only through `backend::Command` and `UserEvent`s sent via `EventLoopProxy`
- Keep `app.rs` and `log_window_macos.rs` thin glue. Logic goes into display-free, platform-independent modules with unit tests: `menu.rs` (menu model, icon state), `problems.rs` (Problems window text), `purge.rs` (Remove Synced Events dialog), `purge_all.rs` (Remove All Events dialog: two warnings, the calendar name typed into `dialog::View::entry`, reported as `dialog::TYPED`), `keychain_notice.rs` (the note before a Keychain prompt; the backend checks `Keychain::prompts`, i.e. `keychain::would_prompt`, before the first load and waits for `Command::KeychainNoticeClosed`), `sponsor.rs` (ways to sponsor in `OPTIONS` with a monochrome PDF logo from `assets/`, when to ask; the answers live in `NSUserDefaults`), `updates.rs` (update check: versions, the latest release, when to check and offer, the update dialog, General's note; `InstalledBy` in `NSUserDefaults`, written by the Homebrew cask, switches the check off by default and replaces Update Now with the package manager's command), `updater.rs` (GitHub API, DMG download with checksum, `replace_bundle`, `relaunch`; `app.rs` runs it on a thread of its own, not in the backend), `logview.rs` (log view model, export), `dialog.rs` (dialog contents shared by the dialog models, About taglines), `args.rs` (`-c` / `--config` of both desktop apps), `autostart.rs` (plist), `backend.rs`, and for the Settings window `settings_view.rs` (the window as a `SettingsView` of controls with stable `FieldId`s, input in, `Reply`s out; `settings_window_macos.rs` only draws it and updates in place while `SettingsScreen::same_shape`; a sync is a `Header` (name, on/off switch) and its pipeline: `Stage`s (From, Keep, one per transform with its `TransformRow`, Into) joined by `Joint`s whose "+" inserts a transform there, a `Preview` the renderers put in a column of its own, and a `Footer` save bar; conditions of an open transform are joined by `Segments` (and / or) and an "or" group is drawn in an `AnyOf`/`AnyOfEnd` box, each condition in a `Condition`/`ConditionEnd` card dragged by its handle to the nearest `DropMark` (`Moved(FieldId::Condition(i), slot)`, a slot of `TransformForm::drops`: each gap plus, at the edge of a box, a place inside it; `TransformForm` keeps the conditions flat with their joins and writes them grouped); an account uses `Group`/`GroupEnd` boxes. GTK keeps the widgets a rebuild removed until the next rebuild (`retired`), since the clicked one may still be finishing its click; the new-sync wizard is a second view (`SettingsScreen::wizard_view`, a sheet on macOS, a modal window in GTK, Settings stays behind it) drawn from `Control::Hero`, `Steps`, `Choices` (cards or grouped rows, with event previews) and `Footer`, on macOS through the `CardButton` class), `preview.rs` (the preview's sample event and its copy through `SyncConfig::preview`, the library's own pipeline), `settings.rs` (lists, selection, drafts, changes on disk, `SettingsChange`), `sync_form.rs`, `transform_form.rs` (fields per action, titles, summaries), `account_form.rs` (`KeychainOp`), `picker.rs`. The backend applies a `SettingsChange` in the order purge, keychain copy, `ConfigEditor` save, keychain cleanup, reload, resume; tests replace `backend::Keychain`, never the real keychain. The GTK app reuses these models with its own glue. macOS-only windows besides the log window: `about_window_macos.rs`, `settings_window_macos.rs`
- No WebKit / web views: windows are native AppKit views via objc2-app-kit. ObjC callbacks (`define_class!` targets, delegates, observers) only forward an input as a user event; logic runs in the event loop. AppKit holds targets and delegates weakly: keep them in the owning Rust struct and clear them on drop
- Menu item ids come from `menu::Action::id`; the menu is built from the model. Replacing it closes an open menu, so `app.rs` updates texts and states in place while the shape stays the same (`menu::same_shape`) and only rebuilds when items come or go
- Linux desktop app: `src/bin/capycal-app/` (feature `gtk`: gtk4, libadwaita, async-channel, ksni on Linux). It includes the display-free `capycal-ui` modules with `#[path]` and keeps only GTK glue: tray (`tray.rs`, StatusNotifierItem via ksni; icon pixels in `tray_icon.rs`), windows (`log_window`, `text_window`, `settings_window` (draws the shared `SettingsView`; `gtk::DropDown` cannot grey out items, so `settings_view` ignores choices of disabled ones), `dialog_window` on `adw::Dialog`, `about_window`, `menu_window` for desktops without a tray, a second start and macOS), `icons.rs`, XDG `autostart.rs`. Window callbacks reach the app through `defer` (an idle callback), never by borrowing it directly: a callback can run while the app is borrowed, e.g. when Quit drops the windows. Logic belongs in the shared models

## Packaging and deployment

- License: GPL-3.0-only (`LICENSE`); contributions come in under MIT (`CONTRIBUTING.md`, PR template) so app store builds may include them. Every package ships `LICENSE`, `CONTRIBUTORS.md` (the MIT notices of contributions) and `THIRD-PARTY-NOTICES.txt` (licenses of the compiled-in crates, generated by `cargo about` from `about.toml` / `about.hbs`: run `make notices` after changing dependencies, CI runs `make check-notices`, which also rejects a license `about.toml` does not accept). About > Acknowledgements shows all of them (`dialog::acknowledgements`, embedded with `include_str!`)

- Linux CLI: `packaging/linux/Dockerfile.musl` builds the static musl binaries (`make dist-linux`); `packaging/linux/build-packages.sh` makes tarball, .deb and .rpm from `Cargo.toml` metadata (man page and completions come from `capycal --man` / `--completions`)
- Linux desktop app: `packaging/linux/build-app-packages.sh` makes the `capycal-app` .deb and .rpm with the desktop entry and icons
- AUR: `packaging/linux/aur/make-pkgbuild.sh` fills the templates of `capycal-bin` (musl tarballs) and `capycal-app-bin` (desktop app `.deb`) from the release checksums; `publish.sh` pushes them with `.SRCINFO` when `AUR_SSH_KEY` is set
- Homebrew: `packaging/homebrew/make-homebrew.sh` fills the formula `capycal` and cask `capycal-app` templates; the release job pushes them to the tap `twiebe/homebrew-capycal` when `HOMEBREW_TAP_TOKEN` is set
- Services: `contrib/` (systemd system and user units, launchd LaunchAgent, `docker/compose.yml` for the published image); `Dockerfile` / `docker-compose.yml` build and run the CLI from source in a container, `docker-compose.dev.yml` adds Radicale for development
- Container image: the release job `image` puts the musl binaries into `packaging/docker/Dockerfile` (distroless static, nonroot, `XDG_CONFIG_HOME=/etc` so the default config is `/etc/capycal/capycal.toml`) and pushes `ghcr.io/twiebe/capycal` for linux/amd64 and linux/arm64
- macOS: `packaging/macos/` (`Info.plist` with `@VERSION@` placeholders, `build-app.sh`, `build-dmg.sh`, app icon in `icon/`: `make-icon.py` generates the checked-in `capycal.svg`, `AppIcon.iconset/`, the book favicons in `docs/theme/` and the tray icons of both apps (GTK `capycal-tray-*-symbolic.svg`, macOS template PDFs `assets/tray*.pdf`; the app adds the red dot of the error icon), `build-app.sh` runs `iconutil` and signs the bundle ad hoc)

## Testing

- Unit tests live in `#[cfg(test)] mod tests` within each file
- State store tests use `std::process::id()` in temp dir paths to avoid parallel test collisions
- Config tests use inline TOML strings
- Tests of waits (backoff, sync interval) use `#[tokio::test(start_paused = true)]` (tokio `test-util` is a dev-dependency), so they run instantly and deterministically
- End-to-end: `tests/caldav_e2e.rs` runs the `capycal` binary against a throwaway Radicale server (`make test-caldav`, `scripts/test-caldav.sh`). The tests are `#[ignore]`d and only create calendars named `capycal-test-...`. Add a case there for every sync behaviour change
- Calendar.app: `live_tests` in `src/eventkit/client.rs` (`make test-eventkit`, macOS only, `#[ignore]`d) write through the real adapter into new calendars named `capycal-test-<pid>-<test>` and remove them on drop. Never touch a calendar without that prefix in tests
- The EventKit ObjC adapter cannot be unit tested on Linux. Keep its logic in `src/eventkit/convert.rs` (tested on Linux) and keep `client.rs` to reading/writing EKEvent fields and NSCalendar/NSTimeZone date conversions

## Security

- Never log passwords or tokens. `Credential` has a custom `Debug` impl that redacts plain passwords
- Sync names are sanitized before use in file paths (no path traversal)
- XML user input is escaped before interpolation into CalDAV requests
- `xml_unescape`: `&amp;` must be replaced last to prevent double-unescaping

## CalDAV specifics

- iCal line folding: always unfold before parsing (RFC 5545 section 3.1)
- XML parsing: multiline-aware (calendar-data spans multiple lines)
- The top-level `sync-token` of a sync-collection answer is searched outside the `response` elements (Radicale sends it first)
- Discovery skips collections whose `supported-calendar-component-set` lacks `VEVENT`; `DiscoveredCalendar::writable` comes from `current-user-privilege-set` (absent = writable)
- CDATA: strip `<![CDATA[...]]>` wrappers from XML content
- Sync-token: try RFC 6578 sync-collection first, fall back to ctag/etag
- PUT: unconditional (no If-None-Match or If-Match) for simplicity
- Retry: exponential backoff on 408/429/5xx and connect/timeout errors, respect Retry-After header. Above that, `SyncRunner::run` retries a failed cycle (an error or `CycleStats.errors`) after `ERROR_RETRY` (1, 2, 5, 15 min, at most `interval`) instead of waiting the interval

## EventKit specifics

- Pure converter (`src/eventkit/convert.rs`): `EkEventData` (event read into plain data, dates already converted to local strings) -> iCal via `to_ical`; incoming iCal -> `SinkUpdate` via `parse_ical`; etag composition, occurrence href, notes marker (`NotesMarker`, `find_marker`, `with_marker`) and `query_chunks`. The ObjC side only fills `EkEventData` (`read_event`) and applies `SinkUpdate` (`apply_update`)
- Full sync skip decision: `client.rs` reads the fields `apply_update` sets into `convert::StoredEvent` while listing (`read_stored`) and converts times with NSCalendar; `convert::is_unchanged` compares them with the `SinkUpdate` (instants, all-day dates, alarms as a set, notes with marker, availability only if the calendar supports it). Keep it in sync with `apply_update`
- Changed/removed detection uses `sync::diff::EtagDiff`; calendar event iteration goes through `for_each_calendar_event` (chunked queries, calendar id filter, one autorelease pool per chunk). The source query runs in `spawn_blocking` (`fetch_blocking`), like the purge helpers in `sync::maintenance`
- Apple limits EventKit predicate queries to ~4 years. Use chunked queries for longer ranges
- Recurring events, source side: sync as individual occurrences (no RRULE). Deleted single occurrences of a series are invisible to EventKit - RRULE would re-create them in the target
- Recurring events, sink side: native series. `parse_ical` reads only the master VEVENT; `parse_series` maps RRULE, EXDATE and overrides to `Series` (unsupported rules are an `Err`, logged, first occurrence only; RDATE ignored). `client.rs` sets an `EKRecurrenceRule`, then `apply_exceptions` removes or changes occurrences found by `occurrenceDate`. A put rewrites an existing series as a whole; delete and purge remove the whole series (`series_span`); full sync never skips a series (`StoredEvent::recurring`)
- Cancelled events (`EKEventStatus::Canceled`): skip
- All-day end: EventKit stores the last second of the last day, iCal the following midnight. Write via `apply_update` (one second before), read via `all_day_end` (one second after)
- Timezone: use `NSCalendar` to convert to local time, emit `DTSTART;TZID=` to preserve DST correctness. Sink side: `tzid::resolve_timezones` (pure, tested on Linux) rewrites TZIDs NSTimeZone doesn't know (Windows names, prefixed IANA names, VTIMEZONE-only names to UTC) before parsing; floating times are local. `apply_update` sets `EKEvent.timeZone` from DTSTART (none for all-day and floating); `StoredEvent::floating` makes a full sync rewrite timed events without a zone
- `requestFullAccessToEventsWithCompletion`: callback-based, bridge to sync via `mpsc::channel`
- Alarms: source emits `VALARM` per `EKAlarm`; sink parses `VALARM` blocks (their `DESCRIPTION` must not leak into event notes) and replaces the event alarms
- Availability: `EKEventAvailability::Free` <-> `TRANSP:TRANSPARENT`, else `OPAQUE`. Only set when the target calendar supports it. `EKEvent.status` is read-only
