use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use tokio::sync::{Notify, Semaphore, watch};
use tokio::task::JoinSet;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::error::{Error, Result};
use crate::ical;
use crate::ical::marker;
use crate::ical::transform::{Transform, TransformContext};
use crate::ical::window::Window;
use crate::source::{EventSink, EventSource, SinkEvent, SourceEvent};
use crate::state::mapping::{SinkRecord, UidMapping};
use crate::state::{StateStore, SyncState};
use crate::sync::status::{CycleStats, FullSyncProgress, StatusReporter, SyncStatus};

/// UUID namespace for deterministic sink UIDs
const CAPYCAL_UUID_NS: Uuid = Uuid::from_bytes([
    0xca, 0x1c, 0xa1, 0x00, 0xca, 0x1c, 0xa1, 0x00, 0xca, 0x1c, 0xa1, 0x00, 0xca, 0x1c, 0xa1, 0x00,
]);

/// Source href prefix of a mapping entry rebuilt for a sink event whose
/// source event no longer exists, so its removal is retried like any other.
const ORPHAN_PREFIX: &str = "orphan:";
/// From this many synced events on, removing more than half of them waits
/// for the next cycle (`SyncState::removals_deferred`).
const MASS_REMOVAL_MIN: usize = 5;

/// Waits before retrying after one, two, three and more failed cycles in a
/// row, so a short outage does not wait for the whole interval.
const ERROR_RETRY: [Duration; 4] = [
    Duration::from_secs(60),
    Duration::from_secs(120),
    Duration::from_secs(300),
    Duration::from_secs(900),
];

struct PreparedEvent {
    source_uid: String,
    sink_uid: String,
    sink_href: String,
    source_href: String,
    source_etag: String,
    ical_data: String,
    is_update: bool,
}

/// What the runner needs to know about a sync, independent of the config format.
pub struct RunnerSettings {
    pub name: String,
    pub source_name: String,
    pub sink_name: String,
    pub source_url: String,
    pub fields: HashSet<String>,
    pub transforms: Vec<Transform>,
    pub interval: Duration,
    /// A full sync runs on the first cycle of each day in this zone.
    pub time_zone: chrono_tz::Tz,
    /// Only source events in these days around now are copied.
    pub window: Option<Window>,
    /// Changes when the config changes what the copies look like (opaque).
    pub output_key: String,
}

/// Starts the next cycle of a running sync early, optionally as a full sync.
#[derive(Default)]
pub struct Wake {
    notify: Notify,
    full_sync: AtomicBool,
}

impl Wake {
    pub fn sync_now(&self) {
        self.notify.notify_one();
    }

    pub fn full_sync(&self) {
        self.full_sync.store(true, Ordering::SeqCst);
        self.notify.notify_one();
    }
}

pub struct SyncRunner<Src: EventSource, Snk: EventSink, St: StateStore> {
    pub name: String,
    pub source_name: String,
    pub sink_name: String,
    source: Src,
    sink: Snk,
    allowed_fields: HashSet<String>,
    transforms: Vec<Transform>,
    transform_ctx: TransformContext,
    interval: Duration,
    time_zone: chrono_tz::Tz,
    window: Option<Window>,
    output_key: String,
    sink_semaphore: Arc<Semaphore>,
    store: Arc<St>,
    state: SyncState,
}

impl<Src: EventSource, Snk: EventSink + Clone + 'static, St: StateStore> SyncRunner<Src, Snk, St> {
    pub fn new(
        settings: RunnerSettings,
        source: Src,
        sink: Snk,
        sink_semaphore: Arc<Semaphore>,
        store: Arc<St>,
    ) -> Result<Self> {
        // Without a usable state the first cycle is a full sync, which
        // rebuilds the state from the markers in the sink
        let mut state = store.load(&settings.name)?.unwrap_or_else(|| SyncState {
            last_full_sync: Some(DateTime::UNIX_EPOCH),
            ..Default::default()
        });
        // A state from before output keys takes the current one, so an
        // upgrade does not fully sync everything at once
        if state.output_key.is_none() {
            state.output_key = Some(settings.output_key.clone());
        }

        let transform_ctx = TransformContext {
            source_name: settings.source_name.clone(),
            source_url: settings.source_url,
            sync_name: settings.name.clone(),
        };

        Ok(Self {
            name: settings.name,
            source_name: settings.source_name,
            sink_name: settings.sink_name,
            source,
            sink,
            allowed_fields: settings.fields,
            transforms: settings.transforms,
            transform_ctx,
            interval: settings.interval,
            time_zone: settings.time_zone,
            window: settings.window,
            output_key: settings.output_key,
            sink_semaphore,
            store,
            state,
        })
    }

    /// Run a single sync cycle, a full sync if one is due.
    pub async fn sync_once(&mut self) -> Result<CycleStats> {
        let (_never, mut shutdown) = watch::channel(false);
        self.cycle(Utc::now(), &mut shutdown, false).await
    }

    /// Run a full sync now.
    pub async fn full_sync_once(&mut self) -> Result<CycleStats> {
        let (_never, mut shutdown) = watch::channel(false);
        self.cycle(Utc::now(), &mut shutdown, true).await
    }

    async fn cycle(
        &mut self,
        now: DateTime<Utc>,
        shutdown: &mut watch::Receiver<bool>,
        full: bool,
    ) -> Result<CycleStats> {
        self.cycle_reporting(now, shutdown, full, &|_| {}).await
    }

    /// Run a full sync if `full` is set, the state was lost or
    /// the last one was on an earlier day; else an incremental
    /// cycle. A full sync reports its progress to `progress`.
    async fn cycle_reporting(
        &mut self,
        now: DateTime<Utc>,
        shutdown: &mut watch::Receiver<bool>,
        full: bool,
        progress: &(dyn Fn(FullSyncProgress) + Sync),
    ) -> Result<CycleStats> {
        let last = match self.state.last_full_sync {
            Some(last) => last,
            // A state from before full syncs: count from now, so an upgrade
            // does not fully sync everything at once
            None => {
                self.state.last_full_sync = Some(now);
                self.store.save(&self.name, &self.state)?;
                now
            }
        };
        let day = |t: DateTime<Utc>| t.with_timezone(&self.time_zone).date_naive();
        let due = day(now) != day(last);
        let reason = if full {
            "requested"
        } else if self.state.full_sync_pending {
            "not completed before"
        } else if self.state.output_key.as_deref() != Some(self.output_key.as_str()) {
            "settings changed"
        } else if last == DateTime::UNIX_EPOCH {
            "no usable state"
        } else if due {
            "daily"
        } else if self.changed_elsewhere(shutdown).await? {
            "target changed elsewhere"
        } else {
            let result = self.sync_once_at(now, shutdown).await;
            return self.note_own_writes(result).await;
        };
        let result = self.full_sync_at(now, shutdown, reason, progress).await;
        self.note_own_writes(result).await
    }

    /// Whether events of this sync changed in the sink since its last
    /// writes: another device runs the same sync, or someone edited or
    /// deleted copies. Moves the sink token on either way.
    async fn changed_elsewhere(&mut self, shutdown: &mut watch::Receiver<bool>) -> Result<bool> {
        let Some(token) = self.state.sink_token.clone() else {
            return Ok(false);
        };
        let changes = tokio::select! {
            biased;
            _ = stopped(shutdown) => return Err(Error::Cancelled),
            result = self.sink.changes_since(Some(&token)) => result,
        };
        let changes = match changes {
            Ok(Some(changes)) => changes,
            Ok(None) => return Ok(false),
            Err(Error::SyncTokenExpired) => {
                // Nothing tells what happened since: compare everything
                self.state.sink_token = None;
                return Ok(true);
            }
            Err(e) => {
                debug!(sync = %self.name, error = %e, "cannot check the target for changes");
                return Ok(false);
            }
        };
        let own: HashSet<&str> = self
            .state
            .uid_mapping
            .iter()
            .map(|(_, record)| file_name(&record.sink_href))
            .collect();
        let elsewhere = changes
            .hrefs
            .iter()
            .filter(|h| own.contains(file_name(h)))
            .count();
        if elsewhere > 0 {
            info!(sync = %self.name, events = elsewhere, "synced events changed in the target, running a full sync");
        }
        if changes.token != token {
            self.state.sink_token = Some(changes.token);
            self.store.save(&self.name, &self.state)?;
        }
        Ok(elsewhere > 0)
    }

    /// Move the sink token past this cycle's own writes, so the next
    /// `changed_elsewhere` sees only changes made since. A change elsewhere
    /// while the cycle ran counts as own.
    async fn note_own_writes(&mut self, result: Result<CycleStats>) -> Result<CycleStats> {
        let Ok(stats) = &result else {
            return result;
        };
        let wrote = stats.created + stats.updated + stats.deleted > 0;
        if self.state.sink_token.is_some() && !wrote {
            return result;
        }
        let mut changes = self
            .sink
            .changes_since(self.state.sink_token.as_deref())
            .await;
        if matches!(changes, Err(Error::SyncTokenExpired)) {
            changes = self.sink.changes_since(None).await;
        }
        match changes {
            Ok(Some(changes)) => {
                self.state.sink_token = Some(changes.token);
                self.store.save(&self.name, &self.state)?;
            }
            Ok(None) => {}
            Err(e) => debug!(sync = %self.name, error = %e, "cannot read the target's sync token"),
        }
        result
    }

    /// Run a cycle that stops early with `Error::Cancelled` once `shutdown` is
    /// signalled: during the fetch, between pushes and deletes, and inside an
    /// in-flight put only if the sink says that is safe.
    async fn sync_once_at(
        &mut self,
        now: DateTime<Utc>,
        shutdown: &mut watch::Receiver<bool>,
    ) -> Result<CycleStats> {
        info!(sync = %self.name, "sync cycle started");
        let started = Instant::now();

        // Fetch into a copy, so a cancelled or failed fetch leaves the state untouched
        let mut fetched = self.state.clone();
        let (changed, mut removed) = tokio::select! {
            biased;
            _ = stopped(shutdown) => return Err(Error::Cancelled),
            result = self.source.fetch_changes(&mut fetched) => result?,
        };
        // Deferred before the state changes, so the next cycle checks the same removals
        let synced: HashSet<&str> = self
            .state
            .uid_mapping
            .iter()
            .map(|(_, record)| record.source_href.as_str())
            .collect();
        let lost = removed
            .iter()
            .filter(|h| synced.contains(h.as_str()))
            .count();
        let added = changed
            .iter()
            .filter(|e| !synced.contains(e.href.as_str()))
            .count();
        let current = synced.len() - lost + added;
        let synced = synced.len();
        if self.defer_removals(lost, synced, current)? {
            return Ok(CycleStats {
                deferred: lost,
                ..CycleStats::default()
            });
        }
        // A cancelled cycle or a failed put restores the old cursor, so the batch is fetched again
        let old_cursor = (self.state.sync_token.take(), self.state.ctag.take());
        self.state = fetched;
        // The fetch started from the old state, which may still wait
        self.state.removals_deferred = false;

        // Retry failed deletes, unless the href came back in the meantime
        for href in std::mem::take(&mut self.state.pending_removals) {
            if !removed.contains(&href) && !changed.iter().any(|e| e.href == href) {
                removed.push(href);
            }
        }

        let mut stats = CycleStats::default();

        if changed.is_empty() && removed.is_empty() {
            debug!(sync = %self.name, "no changes detected");
        } else {
            info!(
                sync = %self.name,
                changed = changed.len(),
                removed = removed.len(),
                "changes detected"
            );

            // Filter and transform (CPU-bound, sequential)
            let mut prepared = Vec::new();
            for event in &changed {
                if let Some(p) = self.prepare_or_remove(event, now, &mut removed) {
                    prepared.push(p);
                }
            }

            let (cancelled, put_failed) = self
                .push(
                    &prepared,
                    removed,
                    Vec::new(),
                    shutdown,
                    &mut stats,
                    &|_| {},
                )
                .await;

            // Successful puts are repeated next cycle as updates through their mapping.
            // An event the sink keeps rejecting makes the source report its batch again
            // every cycle until the put succeeds.
            if cancelled || put_failed {
                (self.state.sync_token, self.state.ctag) = old_cursor;
            }
            self.store.save(&self.name, &self.state)?;
            if cancelled {
                info!(
                    sync = %self.name,
                    took = %took(started),
                    created = stats.created,
                    updated = stats.updated,
                    deleted = stats.deleted,
                    "sync cycle cancelled, progress saved"
                );
                return Err(Error::Cancelled);
            }
        }

        let took = took(started);
        if stats.errors > 0 {
            warn!(
                sync = %self.name,
                %took,
                created = stats.created,
                updated = stats.updated,
                deleted = stats.deleted,
                errors = stats.errors,
                "sync cycle done with errors"
            );
        } else {
            info!(
                sync = %self.name,
                %took,
                created = stats.created,
                updated = stats.updated,
                deleted = stats.deleted,
                "sync cycle done"
            );
        }

        Ok(stats)
    }

    /// Compare the whole source with the sink and repair the difference:
    /// rebuild the mapping from the sink markers, remove duplicates, events
    /// of removed sources and events the transforms now reject, and put
    /// every remaining event whose stored content differs. Cancellation
    /// works as in `sync_once_at`; a cancelled full sync is not recorded.
    async fn full_sync_at(
        &mut self,
        now: DateTime<Utc>,
        shutdown: &mut watch::Receiver<bool>,
        reason: &str,
        progress: &(dyn Fn(FullSyncProgress) + Sync),
    ) -> Result<CycleStats> {
        info!(sync = %self.name, reason, "full sync started");
        progress(FullSyncProgress::default());
        let started = Instant::now();
        // Saved with any progress; cleared once the full sync completes
        self.state.full_sync_pending = true;

        // An empty cursor makes every source report all its current events
        let mut fetched = SyncState::default();
        let (events, _) = tokio::select! {
            biased;
            _ = stopped(shutdown) => return Err(Error::Cancelled),
            result = self.source.fetch_changes(&mut fetched) => result?,
        };
        let listed = tokio::select! {
            biased;
            _ = stopped(shutdown) => return Err(Error::Cancelled),
            result = self.sink.list_synced(&self.name, &self.source_name) => result?,
        };

        let sources: HashMap<String, &SourceEvent> = events
            .iter()
            .filter_map(|e| Some((ical::extract_uid(&e.calendar_data)?, e)))
            .collect();
        let rebuilt = rebuild_mapping(&self.name, &self.state.uid_mapping, listed, &sources);
        let lost = rebuilt
            .mapping
            .iter()
            .filter(|(uid, _)| !sources.contains_key(*uid))
            .count();
        // full_sync_pending is set, so the next cycle is a full sync again
        if self.defer_removals(lost, rebuilt.mapping.len(), sources.len())? {
            return Ok(CycleStats {
                deferred: lost,
                ..CycleStats::default()
            });
        }
        self.state.uid_mapping = rebuilt.mapping;
        // The comparison below finds every removal again
        self.state.pending_removals.clear();
        // Until the full sync completed, the next cycle starts from scratch
        self.state.sync_token = None;
        self.state.ctag = None;

        let mut removed: Vec<String> = self
            .state
            .uid_mapping
            .iter()
            .filter(|(uid, _)| !sources.contains_key(*uid))
            .map(|(_, record)| record.source_href.clone())
            .collect();
        let mut prepared = Vec::new();
        let mut unchanged = 0;
        for event in &events {
            let Some(p) = self.prepare_or_remove(event, now, &mut removed) else {
                continue;
            };
            let stored = rebuilt.stored.get(&p.source_uid);
            if stored.is_some_and(|stored| self.sink.is_unchanged(stored, &p.ical_data)) {
                self.record(&p, p.sink_href.clone());
                unchanged += 1;
            } else {
                prepared.push(p);
            }
        }

        let mut stats = CycleStats::default();
        let total = Some(prepared.len() + removed.len() + rebuilt.stray.len());
        progress(FullSyncProgress { done: 0, total });
        let (cancelled, put_failed) = self
            .push(
                &prepared,
                removed,
                rebuilt.stray,
                shutdown,
                &mut stats,
                &|done| progress(FullSyncProgress { done, total }),
            )
            .await;
        if !cancelled {
            if !put_failed {
                (self.state.sync_token, self.state.ctag) = (fetched.sync_token, fetched.ctag);
            }
            self.state.last_full_sync = Some(now);
            self.state.full_sync_pending = false;
            self.state.output_key = Some(self.output_key.clone());
        }
        self.store.save(&self.name, &self.state)?;
        if cancelled {
            info!(
                sync = %self.name,
                took = %took(started),
                created = stats.created,
                updated = stats.updated,
                deleted = stats.deleted,
                "full sync cancelled, progress saved"
            );
            return Err(Error::Cancelled);
        }

        let took = took(started);
        if stats.errors > 0 {
            warn!(
                sync = %self.name,
                %took,
                created = stats.created,
                updated = stats.updated,
                unchanged,
                deleted = stats.deleted,
                duplicates = rebuilt.duplicates,
                legacy_markers = rebuilt.legacy,
                errors = stats.errors,
                "full sync done with errors"
            );
        } else {
            info!(
                sync = %self.name,
                %took,
                created = stats.created,
                updated = stats.updated,
                unchanged,
                deleted = stats.deleted,
                duplicates = rebuilt.duplicates,
                legacy_markers = rebuilt.legacy,
                "full sync done"
            );
        }
        Ok(stats)
    }

    /// Whether to wait with removing `removed` of `synced` events: once, when
    /// the source now has less than half as many events (`current`). A source
    /// that is briefly empty or incomplete (Calendar.app still loading at
    /// login, a server in maintenance) would otherwise empty the sink, while a
    /// source cleared on purpose loses its copies one cycle later. A source
    /// that replaced its events with as many new ones is not deferred.
    fn defer_removals(&mut self, removed: usize, synced: usize, current: usize) -> Result<bool> {
        let most = synced >= MASS_REMOVAL_MIN && removed > 0 && current * 2 < synced;
        if most && !self.state.removals_deferred {
            self.state.removals_deferred = true;
            self.store.save(&self.name, &self.state)?;
            info!(
                sync = %self.name,
                removed,
                synced,
                "most synced events are gone from the source, removing them in the next cycle"
            );
            return Ok(true);
        }
        self.state.removals_deferred = false;
        Ok(false)
    }

    /// Prepare an event for the sink. A synced event the transforms now reject
    /// (e.g. moved to a weekend) is added to `removed` like a deleted one.
    fn prepare_or_remove(
        &self,
        event: &SourceEvent,
        now: DateTime<Utc>,
        removed: &mut Vec<String>,
    ) -> Option<PreparedEvent> {
        match self.prepare_event(event, now) {
            Ok(Some(p)) => return Some(p),
            Ok(None) => {
                let synced = self
                    .state
                    .uid_mapping
                    .find_by_source_href(&event.href)
                    .is_some();
                if synced && !removed.contains(&event.href) {
                    removed.push(event.href.clone());
                }
            }
            Err(e) => {
                error!(sync = %self.name, href = %event.href, error = %e, "failed to process event");
            }
        }
        None
    }

    fn record(&mut self, p: &PreparedEvent, sink_href: String) {
        self.state.uid_mapping.insert(
            p.source_uid.clone(),
            SinkRecord {
                sink_uid: p.sink_uid.clone(),
                sink_href,
                source_href: p.source_href.clone(),
                source_etag: p.source_etag.clone(),
            },
        );
    }

    /// Put `prepared` to the sink (IO-bound, concurrent with the shared
    /// semaphore), then delete the sink events of the `removed` source hrefs
    /// and the `stray` sink hrefs, which have no mapping entry. Completed work
    /// is recorded in the state. `progress` gets the number of puts and
    /// deletes done so far. Returns (cancelled, a put failed).
    async fn push(
        &mut self,
        prepared: &[PreparedEvent],
        removed: Vec<String>,
        stray: Vec<String>,
        shutdown: &mut watch::Receiver<bool>,
        stats: &mut CycleStats,
        progress: &(dyn Fn(usize) + Sync),
    ) -> (bool, bool) {
        let mut cancelled = false;
        let mut put_failed = false;
        let mut pushes = JoinSet::new();
        // Counted in the tasks: their results are only joined after all
        // puts are started
        let puts_done = Arc::new(AtomicUsize::new(0));
        let mut deletes_done = 0;
        for (i, p) in prepared.iter().enumerate() {
            progress(puts_done.load(Ordering::Relaxed));
            let permit = tokio::select! {
                biased;
                _ = stopped(shutdown) => {
                    cancelled = true;
                    break;
                }
                permit = self.sink_semaphore.clone().acquire_owned() => permit.unwrap(),
            };
            let sink = self.sink.clone();
            let href = p.sink_href.clone();
            let ical = p.ical_data.clone();

            let puts_done = puts_done.clone();
            pushes.spawn(async move {
                let result = sink.put_event(&href, &ical).await;
                drop(permit);
                puts_done.fetch_add(1, Ordering::Relaxed);
                (i, result)
            });
        }

        // Puts the sink cannot abandon safely run to completion, so their mapping is recorded
        let cancel_puts = self.sink.cancel_safe();
        if cancelled && cancel_puts {
            pushes.abort_all();
        }
        loop {
            let joined = tokio::select! {
                joined = pushes.join_next() => joined,
                _ = stopped(shutdown), if cancel_puts && !cancelled => {
                    cancelled = true;
                    pushes.abort_all();
                    continue;
                }
            };
            let Some(joined) = joined else { break };
            progress(puts_done.load(Ordering::Relaxed));
            match joined {
                Ok((i, Ok(put_result))) => {
                    let p = &prepared[i];
                    self.record(p, put_result.href);
                    if p.is_update {
                        stats.updated += 1;
                    } else {
                        stats.created += 1;
                    }
                }
                Ok((i, Err(e))) => {
                    stats.errors += 1;
                    put_failed = true;
                    error!(sync = %self.name, href = %prepared[i].sink_href, error = %e, "failed to push event");
                }
                Err(e) if e.is_cancelled() => {}
                Err(e) => {
                    stats.errors += 1;
                    put_failed = true;
                    error!(sync = %self.name, error = %e, "push task panicked");
                }
            }
        }

        // Handle deletions. A cancelled delete is safe: deletes are idempotent
        // and the mapping is dropped only after success.
        for href in removed {
            let result = if cancelled {
                None
            } else {
                tokio::select! {
                    biased;
                    _ = stopped(shutdown) => None,
                    result = self.handle_deletion(&href) => Some(result),
                }
            };
            deletes_done += 1;
            progress(puts_done.load(Ordering::Relaxed) + deletes_done);
            match result {
                Some(Ok(true)) => stats.deleted += 1,
                Some(Ok(false)) => {}
                Some(Err(e)) => {
                    stats.errors += 1;
                    error!(sync = %self.name, href, error = %e, "failed to delete synced event");
                    self.state.pending_removals.push(href);
                }
                None => {
                    cancelled = true;
                    self.state.pending_removals.push(href);
                }
            }
        }

        // Stray sink events carry the marker, so the next full sync finds them again
        for href in stray {
            if cancelled {
                break;
            }
            let result = tokio::select! {
                biased;
                _ = stopped(shutdown) => {
                    cancelled = true;
                    break;
                }
                result = self.sink.delete_event(&href) => result,
            };
            deletes_done += 1;
            progress(puts_done.load(Ordering::Relaxed) + deletes_done);
            match result {
                Ok(()) => {
                    stats.deleted += 1;
                    info!(sync = %self.name, sink_href = %href, "deleted stray synced event");
                }
                Err(e) => {
                    stats.errors += 1;
                    error!(sync = %self.name, href, error = %e, "failed to delete stray synced event");
                }
            }
        }

        (cancelled, put_failed)
    }

    /// Sync every interval until shutdown. `wake` starts the next cycle
    /// early, as a full sync if asked for.
    pub async fn run(
        &mut self,
        mut shutdown: watch::Receiver<bool>,
        wake: &Wake,
        reporter: &dyn StatusReporter,
    ) {
        let mut status = SyncStatus::new(&self.name);
        let mut failures = 0;
        loop {
            status.running = true;
            reporter.report(&status);

            let now = Utc::now();
            let full = wake.full_sync.swap(false, Ordering::SeqCst);
            let was_full = AtomicBool::new(false);
            let base = status.clone();
            let progress = |p| {
                was_full.store(true, Ordering::Relaxed);
                let mut status = base.clone();
                status.full_sync = Some(p);
                reporter.report(&status);
            };
            let result = self
                .cycle_reporting(now, &mut shutdown, full, &progress)
                .await;
            status.running = false;
            if was_full.load(Ordering::Relaxed) && !matches!(result, Err(Error::Cancelled)) {
                status.full_syncs += 1;
            }
            match result {
                // A cancelled cycle keeps the result of the last completed one
                Err(Error::Cancelled) => {}
                result => {
                    // Failed puts and deletes are retried sooner too
                    let failed = !matches!(&result, Ok(stats) if stats.errors == 0);
                    failures = if failed { failures + 1 } else { 0 };
                    if let Err(e) = &result {
                        error!(sync = %self.name, error = %e, "sync cycle failed");
                    }
                    status.last_run = Some(now);
                    status.last_result = Some(result.map_err(|e| e.to_string()));
                }
            }
            reporter.report(&status);

            tokio::select! {
                biased;
                _ = stopped(&mut shutdown) => {
                    info!(sync = %self.name, "shutting down");
                    break;
                }
                _ = tokio::time::sleep(self.next_wait(failures)) => {},
                _ = wake.notify.notified() => debug!(sync = %self.name, "woken for an early sync"),
            }
        }
    }

    /// The wait before the next cycle: the interval, or after `failures`
    /// failed cycles in a row the next of `ERROR_RETRY`, at most the interval.
    fn next_wait(&self, failures: usize) -> Duration {
        match failures.checked_sub(1) {
            Some(i) => ERROR_RETRY[i.min(ERROR_RETRY.len() - 1)].min(self.interval),
            None => self.interval,
        }
    }

    fn prepare_event(
        &self,
        event: &SourceEvent,
        now: DateTime<Utc>,
    ) -> Result<Option<PreparedEvent>> {
        if marker::is_synced_event(&event.calendar_data) {
            debug!(href = %event.href, "skipping synced-back event");
            return Ok(None);
        }

        if let Some(window) = self.window
            && !window.contains(&event.calendar_data, now)
        {
            debug!(href = %event.href, "event outside the sync window");
            return Ok(None);
        }

        let source_uid = ical::extract_uid(&event.calendar_data)
            .ok_or_else(|| Error::ICalParse(format!("no UID found in event at {}", event.href)))?;

        let existing = self.state.uid_mapping.get(&source_uid);
        let sink_uid = match existing {
            Some(record) => record.sink_uid.clone(),
            None => generate_sink_uid(&self.name, &source_uid),
        };

        let Some(ical_data) = ical::process_event(
            &event.calendar_data,
            &self.allowed_fields,
            &self.transforms,
            &self.transform_ctx,
            &source_uid,
            &sink_uid,
            &self.name,
            &self.source_name,
            now,
        ) else {
            debug!(href = %event.href, "event filtered out by transform");
            return Ok(None);
        };

        let sink_href = match existing {
            Some(record) => record.sink_href.clone(),
            None => format!("{}.ics", sink_uid),
        };

        Ok(Some(PreparedEvent {
            source_uid,
            sink_uid,
            sink_href,
            source_href: event.href.clone(),
            source_etag: event.etag.clone(),
            ical_data,
            is_update: existing.is_some(),
        }))
    }

    async fn handle_deletion(&mut self, deleted_href: &str) -> Result<bool> {
        let found = self
            .state
            .uid_mapping
            .find_by_source_href(deleted_href)
            .map(|(uid, record)| (uid.to_string(), record.sink_href.clone()));

        let Some((uid, sink_href)) = found else {
            debug!(sync = %self.name, href = %deleted_href, "deleted source href not found in mapping, skipping");
            return Ok(false);
        };

        self.sink.delete_event(&sink_href).await?;
        self.state.uid_mapping.remove(&uid);
        info!(
            sync = %self.name,
            source_uid = %uid,
            sink_href = %sink_href,
            "deleted synced event"
        );

        Ok(true)
    }
}

/// The time since `started` for a log line, such as "3.98s".
pub(crate) fn took(started: Instant) -> String {
    format!("{:.2}s", started.elapsed().as_secs_f64())
}

/// The mapping rebuilt from the sink markers, and what the sink holds too much.
#[derive(Debug)]
struct Rebuilt<S> {
    mapping: UidMapping,
    /// Sink hrefs to delete: duplicates and legacy events without a mapping entry.
    stray: Vec<String>,
    duplicates: usize,
    legacy: usize,
    /// What the sink listed of the kept sink event, per source UID.
    stored: HashMap<String, S>,
}

impl<S> Default for Rebuilt<S> {
    fn default() -> Self {
        Self {
            mapping: UidMapping::default(),
            stray: Vec::new(),
            duplicates: 0,
            legacy: 0,
            stored: HashMap::new(),
        }
    }
}

/// Rebuild the mapping from the events `listed` in the sink. Entries of the
/// `old` mapping stay unless the sink names another source UID for their
/// event (old entries may point to events outside the sink listing). Of
/// several sink events for one source UID, the one in the old mapping is
/// kept, else the one at the deterministic href, else the first; the others
/// are duplicates. A legacy EventKit event (no source UID) is attributed by
/// the old mapping or removed, so the sync creates it again with a full marker.
fn rebuild_mapping<S>(
    sync_name: &str,
    old: &UidMapping,
    listed: Vec<SinkEvent<S>>,
    sources: &HashMap<String, &SourceEvent>,
) -> Rebuilt<S> {
    // CalDAV listings return absolute paths where the mapping may hold a relative href
    fn key(href: &str) -> &str {
        href.rsplit('/').next().unwrap_or(href)
    }
    let old_uid_by_href: HashMap<&str, &str> = old
        .iter()
        .map(|(uid, r)| (key(&r.sink_href), uid))
        .collect();

    let mut rebuilt = Rebuilt::default();
    let mut groups: BTreeMap<String, Vec<SinkEvent<S>>> = BTreeMap::new();
    for event in listed {
        let uid = event
            .source_uid
            .clone()
            .or_else(|| old_uid_by_href.get(key(&event.href)).map(|u| u.to_string()));
        match uid {
            Some(uid) => groups.entry(uid).or_default().push(event),
            None => {
                rebuilt.legacy += 1;
                rebuilt.stray.push(event.href);
            }
        }
    }

    let owner: HashMap<&str, &str> = groups
        .iter()
        .flat_map(|(uid, events)| events.iter().map(move |e| (key(&e.href), uid.as_str())))
        .collect();
    for (uid, record) in old.iter() {
        if owner.get(key(&record.sink_href)).is_none_or(|o| *o == uid) {
            rebuilt.mapping.insert(uid.to_string(), record.clone());
        }
    }

    for (uid, mut events) in groups {
        let old_key = rebuilt
            .mapping
            .get(&uid)
            .map(|r| key(&r.sink_href).to_string());
        let deterministic = format!("{}.ics", generate_sink_uid(sync_name, &uid));
        let pos = events
            .iter()
            .position(|e| Some(key(&e.href)) == old_key.as_deref())
            .or_else(|| events.iter().position(|e| key(&e.href) == deterministic))
            .unwrap_or(0);
        let keep = events.swap_remove(pos);
        rebuilt.duplicates += events.len();
        rebuilt.stray.extend(events.into_iter().map(|e| e.href));
        rebuilt.stored.insert(uid.clone(), keep.stored);
        if old_key.as_deref() == Some(key(&keep.href)) {
            continue;
        }
        let source_href = match sources.get(&uid) {
            Some(source) => source.href.clone(),
            None => format!("{ORPHAN_PREFIX}{}", keep.href),
        };
        let record = SinkRecord {
            sink_uid: keep
                .uid
                .unwrap_or_else(|| generate_sink_uid(sync_name, &uid)),
            sink_href: keep.href,
            source_href,
            // Unknown: an event whose put fails is reported as changed again
            source_etag: String::new(),
        };
        rebuilt.mapping.insert(uid, record);
    }
    rebuilt
}

/// Resolves once shutdown is signalled, and never if the sender is gone without signalling.
async fn stopped(shutdown: &mut watch::Receiver<bool>) {
    if shutdown.wait_for(|&stop| stop).await.is_err() {
        std::future::pending::<()>().await;
    }
}

/// The last path segment: the sink hrefs of the mapping are relative, the
/// ones a sink reports may be absolute.
fn file_name(href: &str) -> &str {
    href.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(href)
}

fn generate_sink_uid(sync_name: &str, source_uid: &str) -> String {
    let namespace = Uuid::new_v5(&CAPYCAL_UUID_NS, sync_name.as_bytes());
    let uid = Uuid::new_v5(&namespace, source_uid.as_bytes());
    format!("capycal-{uid}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ical::transform::{Action, Condition, Test};
    use crate::source::{PutResult, SinkChanges};
    use chrono::TimeDelta;
    use std::collections::BTreeMap;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    type Batch = Result<(Vec<SourceEvent>, Vec<String>)>;

    /// Returns one scripted batch per fetch, then empty batches.
    struct FakeSource {
        batches: Mutex<VecDeque<Batch>>,
        fetch_count: Mutex<usize>,
        /// Simulate a request in retry backoff after the state was updated
        stall: bool,
    }

    impl FakeSource {
        fn new(batches: Vec<Batch>) -> Self {
            Self {
                batches: Mutex::new(batches.into()),
                fetch_count: Mutex::new(0),
                stall: false,
            }
        }
    }

    impl EventSource for FakeSource {
        async fn fetch_changes(
            &self,
            state: &mut SyncState,
        ) -> Result<(Vec<SourceEvent>, Vec<String>)> {
            let count = {
                let mut count = self.fetch_count.lock().unwrap();
                *count += 1;
                *count
            };
            state.sync_token = Some(format!("token-{count}"));
            if self.stall {
                tokio::time::sleep(BACKOFF).await;
            }
            self.batches
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Ok((Vec::new(), Vec::new())))
        }
    }

    /// Longer than any wait in the tests, like the retry backoff of a CalDAV request
    const BACKOFF: Duration = Duration::from_secs(31);

    /// A change log: returns the events after the sync token, which counts the events seen.
    struct LogSource {
        events: Vec<SourceEvent>,
        removed: Vec<String>,
    }

    impl EventSource for LogSource {
        async fn fetch_changes(
            &self,
            state: &mut SyncState,
        ) -> Result<(Vec<SourceEvent>, Vec<String>)> {
            let seen: usize = state
                .sync_token
                .as_deref()
                .map_or(0, |t| t.parse().unwrap());
            state.sync_token = Some(self.events.len().to_string());
            let removed = if seen == 0 {
                self.removed.clone()
            } else {
                vec![]
            };
            Ok((self.events[seen..].to_vec(), removed))
        }
    }

    /// A ctag/etag source: on a ctag change, reports the events whose etag differs from the mapping.
    struct EtagSource {
        events: Vec<SourceEvent>,
        ctag: String,
    }

    impl EventSource for EtagSource {
        async fn fetch_changes(
            &self,
            state: &mut SyncState,
        ) -> Result<(Vec<SourceEvent>, Vec<String>)> {
            if state.ctag.as_deref() == Some(self.ctag.as_str()) {
                return Ok((vec![], vec![]));
            }
            let known = state.uid_mapping.source_etags();
            let changed = self
                .events
                .iter()
                .filter(|e| known.get(e.href.as_str()) != Some(&e.etag.as_str()))
                .cloned()
                .collect();
            state.ctag = Some(self.ctag.clone());
            Ok((changed, vec![]))
        }
    }

    #[derive(Default)]
    struct SinkLog {
        puts: Vec<(String, String)>,
        deletes: Vec<String>,
        fail_put: bool,
        /// Puts to this href fail
        fail_href: Option<String>,
        fail_delete: bool,
        /// Simulate a sink (like EventKit) that assigns its own identifiers
        assign_href_prefix: Option<String>,
        /// Puts after this many take BACKOFF to complete
        slow_after: Option<usize>,
        cancel_safe: bool,
        /// The events in the sink by href
        stored: BTreeMap<String, String>,
        fail_list: bool,
        /// A change log of hrefs, like sync-collection; None for a sink without one
        journal: Option<Vec<String>>,
        /// The journal no longer accepts tokens
        expired: bool,
    }

    #[derive(Clone, Default)]
    struct FakeSink(Arc<Mutex<SinkLog>>);

    impl FakeSink {
        fn log(&self) -> std::sync::MutexGuard<'_, SinkLog> {
            self.0.lock().unwrap()
        }
    }

    impl EventSink for FakeSink {
        type Stored = String;

        async fn put_event(&self, href: &str, ical_data: &str) -> Result<PutResult> {
            let (href, slow) = {
                let mut log = self.log();
                if log.fail_put || log.fail_href.as_deref() == Some(href) {
                    return Err(Error::CalDav("put failed".to_string()));
                }
                let slow = log.slow_after.is_some_and(|n| log.puts.len() >= n);
                log.puts.push((href.to_string(), ical_data.to_string()));
                let href = match &log.assign_href_prefix {
                    Some(prefix) if !href.starts_with(prefix.as_str()) => format!("{prefix}{href}"),
                    _ => href.to_string(),
                };
                log.stored.insert(href.clone(), ical_data.to_string());
                if let Some(journal) = &mut log.journal {
                    journal.push(href.clone());
                }
                (href, slow)
            };
            if slow {
                tokio::time::sleep(BACKOFF).await;
            }
            Ok(PutResult {
                etag: "\"sink-etag\"".to_string(),
                href,
            })
        }

        async fn delete_event(&self, href: &str) -> Result<()> {
            let mut log = self.log();
            if log.fail_delete {
                return Err(Error::CalDav("delete failed".to_string()));
            }
            log.deletes.push(href.to_string());
            log.stored.remove(href);
            if let Some(journal) = &mut log.journal {
                journal.push(href.to_string());
            }
            Ok(())
        }

        /// Like CalDAV, or like EventKit (no UID) with `assign_href_prefix`.
        async fn list_synced(
            &self,
            sync_name: &str,
            source_name: &str,
        ) -> Result<Vec<SinkEvent<String>>> {
            let log = self.log();
            if log.fail_list {
                return Err(Error::CalDav("list failed".to_string()));
            }
            let caldav = log.assign_href_prefix.is_none();
            Ok(log
                .stored
                .iter()
                .filter(|(_, ical)| marker::has_sync_marker(ical, sync_name))
                .map(|(href, ical)| SinkEvent {
                    href: href.clone(),
                    uid: ical::extract_uid(ical).filter(|_| caldav),
                    source_uid: marker::source_uid(ical, source_name),
                    stored: ical.clone(),
                })
                .collect())
        }

        fn is_unchanged(&self, stored: &String, ical_data: &str) -> bool {
            ical::same_content(stored, ical_data)
        }

        async fn changes_since(&self, token: Option<&str>) -> Result<Option<SinkChanges>> {
            let log = self.log();
            let Some(journal) = &log.journal else {
                return Ok(None);
            };
            if log.expired && token.is_some() {
                return Err(Error::SyncTokenExpired);
            }
            let seen = token.map_or(0, |t| t.parse().unwrap());
            Ok(Some(SinkChanges {
                hrefs: journal[seen..].to_vec(),
                token: journal.len().to_string(),
            }))
        }

        fn cancel_safe(&self) -> bool {
            self.log().cancel_safe
        }
    }

    #[derive(Default)]
    struct MemStore(Mutex<HashMap<String, SyncState>>);

    impl StateStore for MemStore {
        fn load(&self, sync_name: &str) -> Result<Option<SyncState>> {
            Ok(self.0.lock().unwrap().get(sync_name).cloned())
        }

        fn save(&self, sync_name: &str, state: &SyncState) -> Result<()> {
            self.0
                .lock()
                .unwrap()
                .insert(sync_name.to_string(), state.clone());
            Ok(())
        }

        fn remove(&self, sync_name: &str) -> Result<()> {
            self.0.lock().unwrap().remove(sync_name);
            Ok(())
        }
    }

    const SYNC_NAME: &str = "work-to-personal";

    /// A state as saved before full syncs existed, so the first cycle is incremental.
    fn existing_state(store: &MemStore) {
        store
            .0
            .lock()
            .unwrap()
            .entry(SYNC_NAME.to_string())
            .or_default();
    }

    fn settings(transforms: Vec<Transform>) -> RunnerSettings {
        RunnerSettings {
            name: SYNC_NAME.to_string(),
            source_name: "work".to_string(),
            sink_name: "personal".to_string(),
            source_url: "https://caldav.example.com/work/".to_string(),
            fields: ["DTSTART", "DTEND"].iter().map(|f| f.to_string()).collect(),
            transforms,
            interval: Duration::from_secs(3600),
            time_zone: chrono_tz::UTC,
            window: None,
            output_key: "k1".to_string(),
        }
    }

    fn busy_transform() -> Vec<Transform> {
        vec![Transform::from(Action::Set {
            field: "SUMMARY".to_string(),
            value: "Busy ({{source_name}})".to_string(),
        })]
    }

    fn skip_all_day() -> Vec<Transform> {
        vec![Transform {
            action: Action::Skip,
            when: vec![vec![Condition {
                test: Test::AllDay,
                negate: false,
            }]],
        }]
    }

    fn event(uid: &str, etag: &str) -> SourceEvent {
        event_with_body(
            uid,
            etag,
            "DTSTART:20260105T090000Z\r\nDTEND:20260105T100000Z",
        )
    }

    fn event_with_body(uid: &str, etag: &str, body: &str) -> SourceEvent {
        let calendar_data = format!(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\nBEGIN:VEVENT\r\n\
             UID:{uid}\r\nDTSTAMP:20260101T000000Z\r\n{body}\r\n\
             SUMMARY:Secret meeting\r\nDESCRIPTION:Confidential\r\n\
             END:VEVENT\r\nEND:VCALENDAR\r\n"
        );
        SourceEvent {
            uid: uid.to_string(),
            href: format!("/work/{uid}.ics"),
            etag: etag.to_string(),
            calendar_data,
        }
    }

    fn runner(
        transforms: Vec<Transform>,
        store: &Arc<MemStore>,
        batches: Vec<Batch>,
        sink: &FakeSink,
    ) -> SyncRunner<FakeSource, FakeSink, MemStore> {
        existing_state(store);
        SyncRunner::new(
            settings(transforms),
            FakeSource::new(batches),
            sink.clone(),
            Arc::new(Semaphore::new(2)),
            store.clone(),
        )
        .unwrap()
    }

    /// One put at a time, so the order of puts is deterministic.
    fn serial_runner<Src: EventSource>(
        source: Src,
        store: &Arc<MemStore>,
        sink: &FakeSink,
    ) -> SyncRunner<Src, FakeSink, MemStore> {
        existing_state(store);
        SyncRunner::new(
            settings(vec![]),
            source,
            sink.clone(),
            Arc::new(Semaphore::new(1)),
            store.clone(),
        )
        .unwrap()
    }

    /// Run `r` until shutdown, which is signalled one second into the first cycle.
    /// Returns the runner and its last reported status; fails if stopping takes longer than `limit`.
    async fn run_and_stop<Src: EventSource + 'static>(
        mut r: SyncRunner<Src, FakeSink, MemStore>,
        limit: Duration,
    ) -> (SyncRunner<Src, FakeSink, MemStore>, SyncStatus) {
        let (reporter, mut statuses) = ChannelReporter::new();
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let task = tokio::spawn(async move {
            r.run(shutdown_rx, &Wake::default(), &reporter).await;
            r
        });
        tokio::time::sleep(Duration::from_secs(1)).await;
        shutdown_tx.send(true).unwrap();
        let r = tokio::time::timeout(limit, task)
            .await
            .expect("run did not stop in time")
            .unwrap();
        let mut last = None;
        while let Ok(status) = statuses.try_recv() {
            last = Some(status);
        }
        (r, last.unwrap())
    }

    fn stats(created: usize, updated: usize, deleted: usize, errors: usize) -> CycleStats {
        CycleStats {
            created,
            updated,
            deleted,
            errors,
            deferred: 0,
        }
    }

    fn sink_uid(source_uid: &str) -> String {
        generate_sink_uid(SYNC_NAME, source_uid)
    }

    #[tokio::test]
    async fn test_new_event_is_created_in_sink() {
        let store = Arc::new(MemStore::default());
        let sink = FakeSink::default();
        let mut r = runner(
            busy_transform(),
            &store,
            vec![Ok((vec![event("e1", "v1")], vec![]))],
            &sink,
        );
        let now = DateTime::from_timestamp(1767268800, 0).unwrap();

        let (_tx, mut shutdown) = watch::channel(false);
        assert_eq!(
            r.sync_once_at(now, &mut shutdown).await.unwrap(),
            stats(1, 0, 0, 0)
        );

        let log = sink.log();
        assert_eq!(log.puts.len(), 1);
        let (href, ical) = &log.puts[0];
        let expected_uid = sink_uid("e1");
        assert_eq!(href, &format!("{expected_uid}.ics"));
        assert!(ical.contains(&format!("UID:{expected_uid}")));
        assert!(!ical.contains("UID:e1"));
        assert!(ical.contains("DTSTART:20260105T090000Z"));
        assert!(ical.contains("DTEND:20260105T100000Z"));
        assert!(ical.contains("SUMMARY:Busy (work)"));
        assert!(!ical.contains("Secret meeting"));
        assert!(!ical.contains("Confidential"));
        assert!(ical.contains("X-CAPYCAL-SOURCE:work/e1"));
        assert!(ical.contains(&format!("X-CAPYCAL-SYNC:{SYNC_NAME}")));
        assert!(ical.contains("PRODID:-//capycal//EN"));
        assert!(ical.contains("DTSTAMP:20260101T120000Z"));

        let record = r.state.uid_mapping.get("e1").unwrap();
        assert_eq!(record.sink_uid, expected_uid);
        assert_eq!(record.source_href, "/work/e1.ics");
        assert_eq!(record.source_etag, "v1");
    }

    #[tokio::test]
    async fn test_no_changes_does_not_touch_sink() {
        let store = Arc::new(MemStore::default());
        let sink = FakeSink::default();
        let mut r = runner(vec![], &store, vec![], &sink);

        assert_eq!(r.sync_once().await.unwrap(), stats(0, 0, 0, 0));
        assert!(sink.log().puts.is_empty());
        assert!(sink.log().deletes.is_empty());
        let saved = store.load(SYNC_NAME).unwrap().unwrap();
        assert_eq!(saved.uid_mapping.len(), 0);
        assert_eq!(saved.sync_token, None);
    }

    #[tokio::test]
    async fn test_changed_event_updates_same_sink_href() {
        let store = Arc::new(MemStore::default());
        let sink = FakeSink::default();
        let batches = vec![
            Ok((vec![event("e1", "v1")], vec![])),
            Ok((vec![event("e1", "v2")], vec![])),
        ];
        let mut r = runner(vec![], &store, batches, &sink);

        assert_eq!(r.sync_once().await.unwrap(), stats(1, 0, 0, 0));
        assert_eq!(r.sync_once().await.unwrap(), stats(0, 1, 0, 0));

        let log = sink.log();
        assert_eq!(log.puts.len(), 2);
        assert_eq!(log.puts[0].0, log.puts[1].0);
        assert!(log.puts[1].1.contains(&format!("UID:{}", sink_uid("e1"))));
        assert_eq!(r.state.uid_mapping.len(), 1);
        assert_eq!(r.state.uid_mapping.get("e1").unwrap().source_etag, "v2");
    }

    #[tokio::test]
    async fn test_sink_assigned_href_is_reused_for_update_and_delete() {
        let store = Arc::new(MemStore::default());
        let sink = FakeSink::default();
        sink.log().assign_href_prefix = Some("ek-".to_string());
        let batches = vec![
            Ok((vec![event("e1", "v1")], vec![])),
            Ok((vec![event("e1", "v2")], vec![])),
            Ok((vec![], vec!["/work/e1.ics".to_string()])),
        ];
        let mut r = runner(vec![], &store, batches, &sink);

        r.sync_once().await.unwrap();
        let assigned = format!("ek-{}.ics", sink_uid("e1"));
        assert_eq!(r.state.uid_mapping.get("e1").unwrap().sink_href, assigned);

        r.sync_once().await.unwrap();
        assert_eq!(sink.log().puts[1].0, assigned);

        r.sync_once().await.unwrap();
        assert_eq!(sink.log().deletes, vec![assigned]);
    }

    #[tokio::test]
    async fn test_source_removal_deletes_from_sink_and_drops_mapping() {
        let store = Arc::new(MemStore::default());
        let sink = FakeSink::default();
        let batches = vec![
            Ok((vec![event("e1", "v1"), event("e2", "v1")], vec![])),
            Ok((vec![], vec!["/work/e1.ics".to_string()])),
        ];
        let mut r = runner(vec![], &store, batches, &sink);

        r.sync_once().await.unwrap();
        assert_eq!(r.sync_once().await.unwrap(), stats(0, 0, 1, 0));

        assert_eq!(sink.log().deletes, vec![format!("{}.ics", sink_uid("e1"))]);
        assert!(r.state.uid_mapping.get("e1").is_none());
        assert!(r.state.uid_mapping.get("e2").is_some());
    }

    fn six_events() -> Vec<SourceEvent> {
        (1..=6).map(|i| event(&format!("e{i}"), "v1")).collect()
    }

    fn hrefs(uids: &[&str]) -> Vec<String> {
        uids.iter().map(|uid| format!("/work/{uid}.ics")).collect()
    }

    #[tokio::test]
    async fn test_removing_most_synced_events_waits_for_the_next_cycle() {
        let store = Arc::new(MemStore::default());
        let sink = FakeSink::default();
        // The source refetches the same removals after a deferred cycle
        let batches = vec![
            Ok((six_events(), vec![])),
            Ok((vec![], hrefs(&["e1", "e2", "e3", "e4"]))),
            Ok((vec![], hrefs(&["e1", "e2", "e3", "e4"]))),
        ];
        let mut r = runner(vec![], &store, batches, &sink);
        r.sync_once().await.unwrap();
        let token = r.state.sync_token.clone();

        let waited = r.sync_once().await.unwrap();
        assert_eq!(waited.deferred, 4);
        assert!(sink.log().deletes.is_empty());
        assert_eq!(r.state.uid_mapping.len(), 6);
        // The same listing is fetched again next cycle, also by a new process
        assert_eq!(r.state.sync_token, token);
        assert!(store.load(SYNC_NAME).unwrap().unwrap().removals_deferred);

        assert_eq!(r.sync_once().await.unwrap(), stats(0, 0, 4, 0));
        assert!(!r.state.removals_deferred);
        assert_eq!(r.state.uid_mapping.len(), 2);
    }

    #[tokio::test]
    async fn test_a_source_that_recovers_keeps_its_copies() {
        let store = Arc::new(MemStore::default());
        let sink = FakeSink::default();
        let batches = vec![
            Ok((six_events(), vec![])),
            Ok((vec![], hrefs(&["e1", "e2", "e3", "e4"]))),
            Ok((vec![], vec![])),
            Ok((vec![], hrefs(&["e1", "e2", "e3", "e4"]))),
        ];
        let mut r = runner(vec![], &store, batches, &sink);
        r.sync_once().await.unwrap();
        assert_eq!(r.sync_once().await.unwrap().deferred, 4);
        // Back to normal: the wait is over without removing anything
        assert_eq!(r.sync_once().await.unwrap(), stats(0, 0, 0, 0));
        assert!(!r.state.removals_deferred);
        // A later mass removal waits again
        assert_eq!(r.sync_once().await.unwrap().deferred, 4);
        assert!(sink.log().deletes.is_empty());
    }

    #[tokio::test]
    async fn test_a_source_that_replaced_its_events_is_not_deferred() {
        let store = Arc::new(MemStore::default());
        let sink = FakeSink::default();
        // A feed that renames every event on each request
        let renamed = (1..=6).map(|i| event(&format!("n{i}"), "v1")).collect();
        let all = hrefs(&["e1", "e2", "e3", "e4", "e5", "e6"]);
        let batches = vec![Ok((six_events(), vec![])), Ok((renamed, all))];
        let mut r = runner(vec![], &store, batches, &sink);
        r.sync_once().await.unwrap();
        assert_eq!(r.sync_once().await.unwrap(), stats(6, 0, 6, 0));
        assert!(!r.state.removals_deferred);
    }

    #[tokio::test]
    async fn test_full_sync_with_an_empty_source_removes_one_cycle_later() {
        let store = Arc::new(MemStore::default());
        let sink = FakeSink::default();
        let batches = vec![
            Ok((six_events(), vec![])),
            Ok((vec![], vec![])),
            Ok((vec![], vec![])),
        ];
        let mut r = runner(vec![], &store, batches, &sink);
        r.sync_once().await.unwrap();

        assert_eq!(r.full_sync_once().await.unwrap().deferred, 6);
        assert!(sink.log().deletes.is_empty());
        assert_eq!(r.state.uid_mapping.len(), 6);
        assert!(r.state.full_sync_pending);

        // The next cycle is a full sync again and removes them
        assert_eq!(r.sync_once().await.unwrap(), stats(0, 0, 6, 0));
        assert_eq!(r.state.uid_mapping.len(), 0);
        assert!(!r.state.full_sync_pending);
    }

    #[tokio::test]
    async fn test_removal_of_unknown_href_is_ignored() {
        let store = Arc::new(MemStore::default());
        let sink = FakeSink::default();
        let batches = vec![Ok((vec![], vec!["/work/never-synced.ics".to_string()]))];
        let mut r = runner(vec![], &store, batches, &sink);

        r.sync_once().await.unwrap();

        assert!(sink.log().deletes.is_empty());
    }

    #[tokio::test]
    async fn test_event_with_capycal_marker_is_skipped() {
        let store = Arc::new(MemStore::default());
        let sink = FakeSink::default();
        let synced_back = event_with_body(
            "capycal-foreign",
            "v1",
            "DTSTART:20260105T090000Z\r\nX-CAPYCAL-SOURCE:personal/orig\r\nX-CAPYCAL-SYNC:personal-to-work",
        );
        let batches = vec![Ok((vec![synced_back, event("e1", "v1")], vec![]))];
        let mut r = runner(vec![], &store, batches, &sink);

        r.sync_once().await.unwrap();

        let log = sink.log();
        assert_eq!(log.puts.len(), 1);
        assert_eq!(log.puts[0].0, format!("{}.ics", sink_uid("e1")));
        assert!(r.state.uid_mapping.get("capycal-foreign").is_none());
    }

    #[tokio::test]
    async fn test_event_rejected_by_transform_is_not_pushed() {
        let store = Arc::new(MemStore::default());
        let sink = FakeSink::default();
        let all_day = event_with_body(
            "allday",
            "v1",
            "DTSTART;VALUE=DATE:20260105\r\nDTEND;VALUE=DATE:20260106",
        );
        let batches = vec![Ok((vec![all_day, event("timed", "v1")], vec![]))];
        let mut r = runner(skip_all_day(), &store, batches, &sink);

        assert_eq!(r.sync_once().await.unwrap(), stats(1, 0, 0, 0));

        let log = sink.log();
        assert_eq!(log.puts.len(), 1);
        assert!(log.puts[0].1.contains("X-CAPYCAL-SOURCE:work/timed"));
        assert!(r.state.uid_mapping.get("allday").is_none());
    }

    fn all_day(uid: &str, etag: &str) -> SourceEvent {
        event_with_body(
            uid,
            etag,
            "DTSTART;VALUE=DATE:20260105\r\nDTEND;VALUE=DATE:20260106",
        )
    }

    #[tokio::test]
    async fn test_synced_event_filtered_out_later_is_deleted_from_sink() {
        let store = Arc::new(MemStore::default());
        let sink = FakeSink::default();
        let batches = vec![
            Ok((vec![event("e1", "v1")], vec![])),
            Ok((vec![all_day("e1", "v2")], vec![])),
        ];
        let mut r = runner(skip_all_day(), &store, batches, &sink);

        assert_eq!(r.sync_once().await.unwrap(), stats(1, 0, 0, 0));
        let sink_href = r.state.uid_mapping.get("e1").unwrap().sink_href.clone();

        assert_eq!(r.sync_once().await.unwrap(), stats(0, 0, 1, 0));
        assert_eq!(sink.log().deletes, vec![sink_href]);
        assert!(r.state.uid_mapping.get("e1").is_none());
    }

    #[tokio::test]
    async fn test_failed_delete_of_filtered_out_event_is_retried() {
        let store = Arc::new(MemStore::default());
        let sink = FakeSink::default();
        let batches = vec![
            Ok((vec![event("e1", "v1")], vec![])),
            Ok((vec![all_day("e1", "v2")], vec![])),
            Ok((vec![], vec![])),
        ];
        let mut r = runner(skip_all_day(), &store, batches, &sink);
        r.sync_once().await.unwrap();

        sink.log().fail_delete = true;
        assert_eq!(r.sync_once().await.unwrap(), stats(0, 0, 0, 1));
        assert!(r.state.uid_mapping.get("e1").is_some());
        assert_eq!(r.state.pending_removals.len(), 1);

        sink.log().fail_delete = false;
        assert_eq!(r.sync_once().await.unwrap(), stats(0, 0, 1, 0));
        assert!(r.state.uid_mapping.get("e1").is_none());
        assert!(r.state.pending_removals.is_empty());
    }

    #[tokio::test]
    async fn test_event_without_uid_does_not_block_others() {
        let store = Arc::new(MemStore::default());
        let sink = FakeSink::default();
        let mut broken = event("x", "v1");
        broken.calendar_data = broken.calendar_data.replace("UID:x\r\n", "");
        let batches = vec![Ok((vec![broken, event("e1", "v1")], vec![]))];
        let mut r = runner(vec![], &store, batches, &sink);

        assert_eq!(r.sync_once().await.unwrap(), stats(1, 0, 0, 0));
        assert_eq!(sink.log().puts.len(), 1);
        assert_eq!(r.state.uid_mapping.len(), 1);
    }

    #[tokio::test]
    async fn test_put_failure_keeps_no_mapping_and_retries_later() {
        let store = Arc::new(MemStore::default());
        let sink = FakeSink::default();
        sink.log().fail_put = true;
        let batches = vec![
            Ok((vec![event("e1", "v1")], vec![])),
            Ok((vec![event("e1", "v1")], vec![])),
        ];
        let mut r = runner(vec![], &store, batches, &sink);

        assert_eq!(r.sync_once().await.unwrap(), stats(0, 0, 0, 1));
        assert!(r.state.uid_mapping.get("e1").is_none());

        sink.log().fail_put = false;
        assert_eq!(r.sync_once().await.unwrap(), stats(1, 0, 0, 0));
        assert_eq!(sink.log().puts.len(), 1);
        assert!(r.state.uid_mapping.get("e1").is_some());
    }

    #[tokio::test]
    async fn test_put_failure_on_update_keeps_old_mapping() {
        let store = Arc::new(MemStore::default());
        let sink = FakeSink::default();
        let batches = vec![
            Ok((vec![event("e1", "v1")], vec![])),
            Ok((vec![event("e1", "v2")], vec![])),
        ];
        let mut r = runner(vec![], &store, batches, &sink);

        r.sync_once().await.unwrap();
        sink.log().fail_put = true;
        r.sync_once().await.unwrap();

        assert_eq!(r.state.uid_mapping.get("e1").unwrap().source_etag, "v1");
    }

    #[tokio::test]
    async fn test_delete_failure_keeps_mapping_for_retry() {
        let store = Arc::new(MemStore::default());
        let sink = FakeSink::default();
        let removed = vec!["/work/e1.ics".to_string()];
        let batches = vec![
            Ok((vec![event("e1", "v1")], vec![])),
            Ok((vec![], removed.clone())),
            Ok((vec![], removed)),
        ];
        let mut r = runner(vec![], &store, batches, &sink);

        r.sync_once().await.unwrap();
        sink.log().fail_delete = true;
        r.sync_once().await.unwrap();
        assert!(r.state.uid_mapping.get("e1").is_some());

        sink.log().fail_delete = false;
        r.sync_once().await.unwrap();
        assert_eq!(sink.log().deletes, vec![format!("{}.ics", sink_uid("e1"))]);
        assert!(r.state.uid_mapping.get("e1").is_none());
    }

    #[tokio::test]
    async fn test_failed_delete_is_retried_when_source_reports_removal_once() {
        let store = Arc::new(MemStore::default());
        let sink = FakeSink::default();
        let batches = vec![
            Ok((vec![event("e1", "v1")], vec![])),
            Ok((vec![], vec!["/work/e1.ics".to_string()])),
        ];
        let mut r = runner(vec![], &store, batches, &sink);

        r.sync_once().await.unwrap();
        sink.log().fail_delete = true;
        assert_eq!(r.sync_once().await.unwrap(), stats(0, 0, 0, 1));
        let saved = store.load(SYNC_NAME).unwrap().unwrap();
        assert_eq!(saved.pending_removals, vec!["/work/e1.ics".to_string()]);

        sink.log().fail_delete = false;
        assert_eq!(r.sync_once().await.unwrap(), stats(0, 0, 1, 0));
        assert_eq!(sink.log().deletes, vec![format!("{}.ics", sink_uid("e1"))]);
        assert!(r.state.uid_mapping.get("e1").is_none());
        assert!(
            store
                .load(SYNC_NAME)
                .unwrap()
                .unwrap()
                .pending_removals
                .is_empty()
        );
    }

    #[tokio::test]
    async fn test_pending_removal_is_merged_without_duplicates() {
        let store = Arc::new(MemStore::default());
        let sink = FakeSink::default();
        let removed = vec!["/work/e1.ics".to_string()];
        let batches = vec![
            Ok((vec![event("e1", "v1")], vec![])),
            Ok((vec![], removed.clone())),
            Ok((vec![], removed)),
        ];
        let mut r = runner(vec![], &store, batches, &sink);

        r.sync_once().await.unwrap();
        sink.log().fail_delete = true;
        r.sync_once().await.unwrap();
        r.sync_once().await.unwrap();
        assert_eq!(r.state.pending_removals, vec!["/work/e1.ics".to_string()]);
    }

    #[tokio::test]
    async fn test_pending_removal_is_dropped_when_href_reappears() {
        let store = Arc::new(MemStore::default());
        let sink = FakeSink::default();
        let batches = vec![
            Ok((vec![event("e1", "v1")], vec![])),
            Ok((vec![], vec!["/work/e1.ics".to_string()])),
            Ok((vec![event("e1", "v2")], vec![])),
        ];
        let mut r = runner(vec![], &store, batches, &sink);

        r.sync_once().await.unwrap();
        sink.log().fail_delete = true;
        r.sync_once().await.unwrap();
        sink.log().fail_delete = false;
        r.sync_once().await.unwrap();

        assert!(sink.log().deletes.is_empty());
        assert!(r.state.pending_removals.is_empty());
        assert_eq!(r.state.uid_mapping.get("e1").unwrap().source_etag, "v2");
    }

    #[tokio::test]
    async fn test_sync_once_reports_changes_only_for_effective_removals() {
        let store = Arc::new(MemStore::default());
        let sink = FakeSink::default();
        let batches = vec![
            Ok((vec![event("e1", "v1")], vec![])),
            Ok((vec![], vec!["/work/never-synced.ics".to_string()])),
            Ok((vec![], vec!["/work/e1.ics".to_string()])),
            Ok((vec![], vec!["/work/e1.ics".to_string()])),
        ];
        let mut r = runner(vec![], &store, batches, &sink);

        r.sync_once().await.unwrap();
        assert_eq!(r.sync_once().await.unwrap(), stats(0, 0, 0, 0));

        sink.log().fail_delete = true;
        assert_eq!(r.sync_once().await.unwrap(), stats(0, 0, 0, 1));

        sink.log().fail_delete = false;
        assert_eq!(r.sync_once().await.unwrap(), stats(0, 0, 1, 0));
    }

    #[tokio::test]
    async fn test_delete_failure_does_not_abort_cycle() {
        let store = Arc::new(MemStore::default());
        let sink = FakeSink::default();
        let batches = vec![
            Ok((vec![event("e1", "v1")], vec![])),
            Ok((vec![event("e2", "v1")], vec!["/work/e1.ics".to_string()])),
        ];
        let mut r = runner(vec![], &store, batches, &sink);

        r.sync_once().await.unwrap();
        sink.log().fail_delete = true;
        assert_eq!(r.sync_once().await.unwrap(), stats(1, 0, 0, 1));

        assert!(r.state.uid_mapping.get("e2").is_some());
        let reloaded = store.load(SYNC_NAME).unwrap().unwrap();
        assert!(reloaded.uid_mapping.get("e2").is_some());
    }

    #[tokio::test]
    async fn test_state_persists_across_runner_instances() {
        let store = Arc::new(MemStore::default());
        let sink = FakeSink::default();
        let mut first = runner(
            vec![],
            &store,
            vec![Ok((vec![event("e1", "v1")], vec![]))],
            &sink,
        );
        first.sync_once().await.unwrap();
        drop(first);

        let batches = vec![
            Ok((vec![event("e1", "v2")], vec![])),
            Ok((vec![], vec!["/work/e1.ics".to_string()])),
        ];
        let mut second = runner(vec![], &store, batches, &sink);
        assert_eq!(second.state.sync_token.as_deref(), Some("token-1"));
        assert_eq!(
            second.state.uid_mapping.get("e1").unwrap().source_etag,
            "v1"
        );

        second.sync_once().await.unwrap();
        second.sync_once().await.unwrap();

        let expected_href = format!("{}.ics", sink_uid("e1"));
        let log = sink.log();
        assert_eq!(log.puts.len(), 2);
        assert_eq!(log.puts[1].0, expected_href);
        assert_eq!(log.deletes, vec![expected_href]);

        let reloaded = store.load(SYNC_NAME).unwrap().unwrap();
        assert!(reloaded.uid_mapping.get("e1").is_none());
        assert_eq!(reloaded.sync_token.as_deref(), Some("token-2"));
    }

    #[tokio::test]
    async fn test_source_error_is_propagated() {
        let store = Arc::new(MemStore::default());
        let sink = FakeSink::default();
        let batches = vec![Err(Error::CalDav("source down".to_string()))];
        let mut r = runner(vec![], &store, batches, &sink);

        assert!(r.sync_once().await.is_err());
        assert!(sink.log().puts.is_empty());
    }

    #[tokio::test]
    async fn test_run_stops_on_shutdown() {
        let store = Arc::new(MemStore::default());
        let sink = FakeSink::default();
        let mut r = runner(
            vec![],
            &store,
            vec![Ok((vec![event("e1", "v1")], vec![]))],
            &sink,
        );

        let (tx, rx) = watch::channel(false);
        tx.send(true).unwrap();
        let (reporter, _statuses) = ChannelReporter::new();
        tokio::time::timeout(
            Duration::from_secs(5),
            r.run(rx, &Wake::default(), &reporter),
        )
        .await
        .expect("run did not stop on shutdown");

        // Shutdown before the first fetch cancels the cycle right away
        assert_eq!(*r.source.fetch_count.lock().unwrap(), 0);
        assert!(sink.log().puts.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn test_shutdown_cancels_fetch_in_backoff_and_keeps_state() {
        let store = Arc::new(MemStore::default());
        let sink = FakeSink::default();
        let mut r = runner(
            vec![],
            &store,
            vec![Ok((vec![event("e1", "v1")], vec![]))],
            &sink,
        );
        r.source.stall = true;

        let (r, status) = run_and_stop(r, Duration::from_secs(1)).await;

        assert_eq!(*r.source.fetch_count.lock().unwrap(), 1);
        assert_eq!(r.state.sync_token, None);
        assert_eq!(store.load(SYNC_NAME).unwrap().unwrap().sync_token, None);
        assert!(sink.log().puts.is_empty());
        assert!(!status.running);
        assert_eq!(status.last_result, None);
    }

    #[tokio::test(start_paused = true)]
    async fn test_shutdown_during_cancel_safe_put_saves_progress_and_refetches_the_rest() {
        let store = Arc::new(MemStore::default());
        let mut known = SyncState::default();
        known.uid_mapping.insert(
            "gone".to_string(),
            SinkRecord {
                sink_uid: sink_uid("gone"),
                sink_href: format!("{}.ics", sink_uid("gone")),
                source_href: "/work/gone.ics".to_string(),
                source_etag: "v1".to_string(),
            },
        );
        store.save(SYNC_NAME, &known).unwrap();
        let source = || LogSource {
            events: vec![event("e1", "v1"), event("e2", "v1"), event("e3", "v1")],
            removed: vec!["/work/gone.ics".to_string()],
        };
        let sink = FakeSink::default();
        sink.log().cancel_safe = true;
        sink.log().slow_after = Some(1);

        // e1 is put, e2 is abandoned in backoff, e3 and the delete never start
        let (r, status) = run_and_stop(serial_runner(source(), &store, &sink), BACKOFF / 2).await;
        drop(r);
        assert_eq!(status.last_result, None);
        let saved = store.load(SYNC_NAME).unwrap().unwrap();
        assert_eq!(saved.sync_token, None);
        assert!(saved.uid_mapping.get("e1").is_some());
        assert!(saved.uid_mapping.get("e2").is_none());
        assert!(saved.uid_mapping.get("gone").is_some());
        assert_eq!(saved.pending_removals, vec!["/work/gone.ics".to_string()]);
        assert_eq!(sink.log().puts.len(), 2);

        // The next start fetches everything again and puts each event to its one href
        sink.log().slow_after = None;
        let mut next = serial_runner(source(), &store, &sink);
        assert_eq!(next.sync_once().await.unwrap(), stats(2, 1, 1, 0));
        let hrefs: HashSet<String> = sink.log().puts.iter().map(|(h, _)| h.clone()).collect();
        let expected: HashSet<String> = ["e1", "e2", "e3"]
            .iter()
            .map(|uid| format!("{}.ics", sink_uid(uid)))
            .collect();
        assert_eq!(hrefs, expected);
        let saved = store.load(SYNC_NAME).unwrap().unwrap();
        assert_eq!(saved.sync_token.as_deref(), Some("3"));
        assert!(saved.pending_removals.is_empty());
        assert!(saved.uid_mapping.get("gone").is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn test_shutdown_waits_for_put_that_is_not_cancel_safe() {
        let store = Arc::new(MemStore::default());
        let source = || LogSource {
            events: vec![event("e1", "v1"), event("e2", "v1")],
            removed: vec![],
        };
        let sink = FakeSink::default();
        sink.log().assign_href_prefix = Some("ek-".to_string());
        sink.log().slow_after = Some(0);

        // The in-flight put of e1 completes and is recorded, e2 never starts
        let (r, _) = run_and_stop(serial_runner(source(), &store, &sink), BACKOFF * 2).await;
        drop(r);
        let saved = store.load(SYNC_NAME).unwrap().unwrap();
        assert_eq!(saved.sync_token, None);
        let e1_href = format!("ek-{}.ics", sink_uid("e1"));
        assert_eq!(saved.uid_mapping.get("e1").unwrap().sink_href, e1_href);
        assert_eq!(sink.log().puts.len(), 1);

        sink.log().slow_after = None;
        let mut next = serial_runner(source(), &store, &sink);
        assert_eq!(next.sync_once().await.unwrap(), stats(1, 1, 0, 0));
        assert_eq!(sink.log().puts[1].0, e1_href);
    }

    fn put_hrefs(sink: &FakeSink) -> Vec<String> {
        sink.log().puts.iter().map(|(h, _)| h.clone()).collect()
    }

    #[tokio::test]
    async fn test_failed_put_refetches_batch_from_old_sync_token() {
        let store = Arc::new(MemStore::default());
        let sink = FakeSink::default();
        sink.log().assign_href_prefix = Some("ek-".to_string());
        sink.log().fail_href = Some(format!("{}.ics", sink_uid("e2")));
        let source = LogSource {
            events: vec![event("e1", "v1"), event("e2", "v1")],
            removed: vec![],
        };
        let mut r = serial_runner(source, &store, &sink);

        assert_eq!(r.sync_once().await.unwrap(), stats(1, 0, 0, 1));
        let saved = store.load(SYNC_NAME).unwrap().unwrap();
        assert_eq!(saved.sync_token, None);
        let e1_href = format!("ek-{}.ics", sink_uid("e1"));
        assert_eq!(saved.uid_mapping.get("e1").unwrap().sink_href, e1_href);
        assert!(saved.uid_mapping.get("e2").is_none());

        // e1 is updated in place, e2 is retried
        sink.log().fail_href = None;
        assert_eq!(r.sync_once().await.unwrap(), stats(1, 1, 0, 0));
        let e2_href = format!("ek-{}.ics", sink_uid("e2"));
        assert_eq!(
            put_hrefs(&sink),
            vec![
                format!("{}.ics", sink_uid("e1")),
                e1_href,
                format!("{}.ics", sink_uid("e2"))
            ]
        );
        let saved = store.load(SYNC_NAME).unwrap().unwrap();
        assert_eq!(saved.sync_token.as_deref(), Some("2"));
        assert_eq!(saved.uid_mapping.get("e2").unwrap().sink_href, e2_href);

        assert_eq!(r.sync_once().await.unwrap(), stats(0, 0, 0, 0));
    }

    #[tokio::test]
    async fn test_failed_put_restores_ctag_and_retries_only_failed_event() {
        let store = Arc::new(MemStore::default());
        let sink = FakeSink::default();
        sink.log().fail_href = Some(format!("{}.ics", sink_uid("e2")));
        let source = EtagSource {
            events: vec![event("e1", "v1"), event("e2", "v1")],
            ctag: "c1".to_string(),
        };
        let mut r = serial_runner(source, &store, &sink);

        assert_eq!(r.sync_once().await.unwrap(), stats(1, 0, 0, 1));
        assert_eq!(store.load(SYNC_NAME).unwrap().unwrap().ctag, None);
        assert_eq!(r.sync_once().await.unwrap(), stats(0, 0, 0, 1));

        sink.log().fail_href = None;
        assert_eq!(r.sync_once().await.unwrap(), stats(1, 0, 0, 0));
        assert_eq!(
            store.load(SYNC_NAME).unwrap().unwrap().ctag.as_deref(),
            Some("c1")
        );
        assert_eq!(r.sync_once().await.unwrap(), stats(0, 0, 0, 0));
        let e1_href = format!("{}.ics", sink_uid("e1"));
        let e2_href = format!("{}.ics", sink_uid("e2"));
        assert_eq!(put_hrefs(&sink), vec![e1_href, e2_href]);
    }

    #[tokio::test(start_paused = true)]
    async fn test_permanently_failing_put_does_not_block_other_events() {
        let store = Arc::new(MemStore::default());
        let sink = FakeSink::default();
        sink.log().fail_href = Some(format!("{}.ics", sink_uid("e2")));
        let source = LogSource {
            events: vec![event("e1", "v1"), event("e2", "v1")],
            removed: vec![],
        };

        let (mut r, status) = run_and_stop(serial_runner(source, &store, &sink), BACKOFF).await;
        assert_eq!(status.last_result, Some(Ok(stats(1, 0, 0, 1))));

        // New changes keep syncing, the batch with e2 is fetched again each cycle
        r.source.events.push(event("e3", "v1"));
        assert_eq!(r.sync_once().await.unwrap(), stats(1, 1, 0, 1));
        assert!(r.state.uid_mapping.get("e3").is_some());
        assert!(r.state.uid_mapping.get("e2").is_none());
        assert_eq!(store.load(SYNC_NAME).unwrap().unwrap().sync_token, None);
    }

    /// Forwards every reported status to a channel the test can await.
    struct ChannelReporter(tokio::sync::mpsc::UnboundedSender<SyncStatus>);

    impl ChannelReporter {
        fn new() -> (Self, tokio::sync::mpsc::UnboundedReceiver<SyncStatus>) {
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            (Self(tx), rx)
        }
    }

    impl StatusReporter for ChannelReporter {
        fn report(&self, status: &SyncStatus) {
            let _ = self.0.send(status.clone());
        }
    }

    #[tokio::test]
    async fn test_run_reports_each_cycle_and_wakes_before_interval() {
        let store = Arc::new(MemStore::default());
        let sink = FakeSink::default();
        let batches = vec![
            Ok((vec![event("e1", "v1")], vec![])),
            Err(Error::CalDav("server down".to_string())),
        ];
        // The interval is an hour, so only the wake can start the second cycle
        let mut r = runner(vec![], &store, batches, &sink);
        let (reporter, mut statuses) = ChannelReporter::new();
        let wake = Arc::new(Wake::default());
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let task = tokio::spawn({
            let wake = wake.clone();
            async move { r.run(shutdown_rx, &wake, &reporter).await }
        });
        let mut next = async || {
            tokio::time::timeout(Duration::from_secs(5), statuses.recv())
                .await
                .expect("no status reported")
                .unwrap()
        };

        let started = next().await;
        assert_eq!(started.name, SYNC_NAME);
        assert!(started.running);
        assert_eq!(started.last_result, None);
        let done = next().await;
        assert!(!done.running);
        assert!(done.last_run.is_some());
        assert_eq!(done.last_result, Some(Ok(stats(1, 0, 0, 0))));

        wake.sync_now();
        let started = next().await;
        assert!(started.running);
        assert_eq!(started.last_result, Some(Ok(stats(1, 0, 0, 0))));
        let failed = next().await;
        assert_eq!(
            failed.last_result,
            Some(Err("CalDAV request failed: server down".to_string()))
        );

        shutdown_tx.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap();
    }

    /// The waits between the starts of the first `cycles` cycles of `r`.
    async fn waits_between_cycles(
        mut r: SyncRunner<FakeSource, FakeSink, MemStore>,
        cycles: usize,
    ) -> Vec<Duration> {
        let (reporter, mut statuses) = ChannelReporter::new();
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let task =
            tokio::spawn(async move { r.run(shutdown_rx, &Wake::default(), &reporter).await });
        let mut starts = Vec::new();
        while starts.len() < cycles {
            if statuses.recv().await.unwrap().running {
                starts.push(tokio::time::Instant::now());
            }
        }
        shutdown_tx.send(true).unwrap();
        task.await.unwrap();
        starts.windows(2).map(|w| w[1] - w[0]).collect()
    }

    #[tokio::test(start_paused = true)]
    async fn test_failed_cycle_is_retried_sooner_with_growing_waits() {
        let store = Arc::new(MemStore::default());
        let sink = FakeSink::default();
        let down = || Err(Error::CalDav("server down".to_string()));
        let batches = vec![down(), down(), down(), Ok((vec![], vec![])), down()];
        let r = runner(vec![], &store, batches, &sink);
        let minutes = |m: u64| Duration::from_secs(m * 60);
        assert_eq!(
            waits_between_cycles(r, 6).await,
            // After a success the interval (an hour), the next failure starts over
            [minutes(1), minutes(2), minutes(5), minutes(60), minutes(1)]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn test_cycle_with_failed_puts_is_retried_sooner() {
        let store = Arc::new(MemStore::default());
        let sink = FakeSink::default();
        sink.log().fail_put = true;
        let r = runner(
            vec![],
            &store,
            vec![Ok((vec![event("e1", "v1")], vec![]))],
            &sink,
        );
        assert_eq!(waits_between_cycles(r, 2).await, [Duration::from_secs(60)]);
    }

    #[tokio::test(start_paused = true)]
    async fn test_retry_after_an_error_never_waits_longer_than_the_interval() {
        let store = Arc::new(MemStore::default());
        let sink = FakeSink::default();
        let down = || Err(Error::CalDav("server down".to_string()));
        let mut r = runner(vec![], &store, vec![down(), down(), down()], &sink);
        r.interval = Duration::from_secs(90);
        assert_eq!(
            waits_between_cycles(r, 4).await,
            [60, 90, 90].map(Duration::from_secs)
        );
    }

    /// A runner that loads whatever `store` holds, starting with a full sync without state.
    #[tokio::test]
    async fn test_run_reports_the_progress_of_a_full_sync() {
        let store = Arc::new(MemStore::default());
        let sink = FakeSink::default();
        let events = vec![event("e1", "v1"), event("e2", "v1"), event("e3", "v1")];
        // Without a state the first cycle is a full sync
        let mut r = fresh_runner(log_source(events), vec![], &store, &sink);
        let (reporter, mut statuses) = ChannelReporter::new();
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let task =
            tokio::spawn(async move { r.run(shutdown_rx, &Wake::default(), &reporter).await });

        let mut progress = Vec::new();
        let done = loop {
            let status = tokio::time::timeout(Duration::from_secs(5), statuses.recv())
                .await
                .expect("no status reported")
                .unwrap();
            if !status.running {
                break status;
            }
            progress.extend(status.full_sync);
        };
        shutdown_tx.send(true).unwrap();
        task.await.unwrap();

        // Unknown while reading, then counting up to every put
        assert_eq!(progress.first(), Some(&FullSyncProgress::default()));
        assert_eq!(
            progress.last(),
            Some(&FullSyncProgress {
                done: 3,
                total: Some(3)
            })
        );
        assert!(progress.windows(2).all(|w| w[0].done <= w[1].done));
        assert_eq!(done.full_sync, None);
        assert_eq!(done.full_syncs, 1);
        assert_eq!(done.last_result, Some(Ok(stats(3, 0, 0, 0))));
    }

    fn fresh_runner<Src: EventSource>(
        source: Src,
        transforms: Vec<Transform>,
        store: &Arc<MemStore>,
        sink: &FakeSink,
    ) -> SyncRunner<Src, FakeSink, MemStore> {
        SyncRunner::new(
            settings(transforms),
            source,
            sink.clone(),
            Arc::new(Semaphore::new(1)),
            store.clone(),
        )
        .unwrap()
    }

    fn log_source(events: Vec<SourceEvent>) -> LogSource {
        LogSource {
            events,
            removed: vec![],
        }
    }

    fn href(uid: &str) -> String {
        format!("{}.ics", sink_uid(uid))
    }

    /// Sync `events` with a fresh state, then forget the state.
    async fn synced_then_lost(events: Vec<SourceEvent>, store: &Arc<MemStore>, sink: &FakeSink) {
        let mut r = serial_runner(log_source(events), store, sink);
        r.sync_once().await.unwrap();
        store.remove(SYNC_NAME).unwrap();
    }

    #[tokio::test]
    async fn test_copies_changed_elsewhere_make_the_next_cycle_a_full_sync() {
        let store = Arc::new(MemStore::default());
        let sink = FakeSink::default();
        sink.log().journal = Some(Vec::new());
        let mut r = serial_runner(
            log_source(vec![event("e1", "v1"), event("e2", "v1")]),
            &store,
            &sink,
        );
        let full_syncs = |r: &SyncRunner<_, _, _>| r.state.last_full_sync;
        assert_eq!(r.sync_once().await.unwrap(), stats(2, 0, 0, 0));
        assert_eq!(r.state.sink_token.as_deref(), Some("2"), "own puts noted");
        let before = full_syncs(&r);
        assert_eq!(r.sync_once().await.unwrap(), stats(0, 0, 0, 0));
        assert_eq!(full_syncs(&r), before, "nothing changed elsewhere");

        // Another sync's event in the same calendar does not count
        sink.log()
            .journal
            .as_mut()
            .unwrap()
            .push("/cal/other.ics".into());
        r.sync_once().await.unwrap();
        assert_eq!(full_syncs(&r), before);

        // Another device or a person removed a copy
        sink.log().stored.remove(&href("e1"));
        let removed = format!("/cal/{}", href("e1"));
        sink.log().journal.as_mut().unwrap().push(removed);
        assert_eq!(r.sync_once().await.unwrap(), stats(0, 1, 0, 0));
        assert!(full_syncs(&r) > before);
        assert!(sink.log().stored.contains_key(&href("e1")));
        let after = full_syncs(&r);
        assert_eq!(r.sync_once().await.unwrap(), stats(0, 0, 0, 0));
        assert_eq!(
            full_syncs(&r),
            after,
            "its own repair is not a change elsewhere"
        );

        // A token the sink no longer knows compares everything once
        sink.log().expired = true;
        r.sync_once().await.unwrap();
        assert!(full_syncs(&r) > after);
        sink.log().expired = false;
        assert_eq!(r.state.sink_token.as_deref(), Some("5"));
    }

    #[tokio::test]
    async fn test_lost_state_is_rebuilt_from_the_sink_without_puts() {
        let store = Arc::new(MemStore::default());
        let sink = FakeSink::default();
        let events = || vec![event("e1", "v1"), event("e2", "v1")];
        synced_then_lost(events(), &store, &sink).await;

        let mut r = fresh_runner(log_source(events()), vec![], &store, &sink);
        assert_eq!(r.sync_once().await.unwrap(), stats(0, 0, 0, 0));
        assert_eq!(sink.log().puts.len(), 2);
        let saved = store.load(SYNC_NAME).unwrap().unwrap();
        for uid in ["e1", "e2"] {
            let record = saved.uid_mapping.get(uid).unwrap();
            assert_eq!(record.sink_href, href(uid));
            assert_eq!(record.sink_uid, sink_uid(uid));
            assert_eq!(record.source_href, format!("/work/{uid}.ics"));
            assert_eq!(record.source_etag, "v1");
        }
        assert_eq!(saved.sync_token.as_deref(), Some("2"));
        assert!(saved.last_full_sync.unwrap() > DateTime::UNIX_EPOCH);

        // The next cycle is incremental again
        assert_eq!(r.sync_once().await.unwrap(), stats(0, 0, 0, 0));
        assert_eq!(sink.log().puts.len(), 2);
    }

    #[tokio::test]
    async fn test_full_sync_updates_differing_and_creates_missing_events() {
        let store = Arc::new(MemStore::default());
        let sink = FakeSink::default();
        synced_then_lost(vec![event("e1", "v1")], &store, &sink).await;

        let moved = event_with_body("e1", "v2", "DTSTART:20260106T090000Z");
        let mut r = fresh_runner(
            log_source(vec![moved, event("e2", "v1")]),
            vec![],
            &store,
            &sink,
        );
        assert_eq!(r.sync_once().await.unwrap(), stats(1, 1, 0, 0));
        assert_eq!(put_hrefs(&sink), vec![href("e1"), href("e1"), href("e2")]);
        assert!(sink.log().puts[1].1.contains("DTSTART:20260106T090000Z"));
        assert_eq!(r.state.uid_mapping.get("e1").unwrap().source_etag, "v2");
    }

    #[tokio::test]
    async fn test_full_sync_puts_only_events_the_sink_reports_changed() {
        let store = Arc::new(MemStore::default());
        let sink = FakeSink::default();
        sink.log().assign_href_prefix = Some("ek-".to_string());
        synced_then_lost(vec![event("e1", "v1"), event("e2", "v1")], &store, &sink).await;

        let moved = event_with_body("e2", "v2", "DTSTART:20260106T090000Z");
        let mut r = fresh_runner(
            log_source(vec![event("e1", "v1"), moved]),
            vec![],
            &store,
            &sink,
        );
        assert_eq!(r.full_sync_once().await.unwrap(), stats(0, 1, 0, 0));
        assert_eq!(put_hrefs(&sink)[2..], [format!("ek-{}", href("e2"))]);
    }

    #[tokio::test]
    async fn test_full_sync_keeps_one_of_several_sink_events() {
        let store = Arc::new(MemStore::default());
        let sink = FakeSink::default();
        synced_then_lost(vec![event("e1", "v1")], &store, &sink).await;
        let copy = sink.log().stored[&href("e1")].clone();
        sink.log().stored.insert("/home/copy.ics".to_string(), copy);

        let mut r = fresh_runner(log_source(vec![event("e1", "v1")]), vec![], &store, &sink);
        assert_eq!(r.sync_once().await.unwrap(), stats(0, 0, 1, 0));
        assert_eq!(sink.log().deletes, vec!["/home/copy.ics".to_string()]);
        assert_eq!(r.state.uid_mapping.get("e1").unwrap().sink_href, href("e1"));
        assert_eq!(r.state.uid_mapping.len(), 1);
    }

    #[tokio::test]
    async fn test_full_sync_removes_events_of_deleted_source_events() {
        let store = Arc::new(MemStore::default());
        let sink = FakeSink::default();
        synced_then_lost(vec![event("e1", "v1"), event("gone", "v1")], &store, &sink).await;

        let mut r = fresh_runner(log_source(vec![event("e1", "v1")]), vec![], &store, &sink);
        assert_eq!(r.sync_once().await.unwrap(), stats(0, 0, 1, 0));
        assert_eq!(sink.log().deletes, vec![href("gone")]);
        assert!(r.state.uid_mapping.get("gone").is_none());
        assert!(r.state.pending_removals.is_empty());
    }

    #[tokio::test]
    async fn test_failed_removal_of_orphan_is_retried() {
        let store = Arc::new(MemStore::default());
        let sink = FakeSink::default();
        synced_then_lost(vec![event("gone", "v1")], &store, &sink).await;

        sink.log().fail_delete = true;
        let mut r = fresh_runner(log_source(vec![]), vec![], &store, &sink);
        assert_eq!(r.sync_once().await.unwrap(), stats(0, 0, 0, 1));
        let orphan = format!("{ORPHAN_PREFIX}{}", href("gone"));
        assert_eq!(r.state.pending_removals, vec![orphan]);
        assert!(r.state.uid_mapping.get("gone").is_some());

        sink.log().fail_delete = false;
        assert_eq!(r.sync_once().await.unwrap(), stats(0, 0, 1, 0));
        assert!(r.state.uid_mapping.get("gone").is_none());
        assert!(sink.log().stored.is_empty());
    }

    #[tokio::test]
    async fn test_full_sync_removes_events_the_transforms_now_reject() {
        let store = Arc::new(MemStore::default());
        let sink = FakeSink::default();
        let events = || vec![event("timed", "v1"), all_day("allday", "v1")];
        let mut first = serial_runner(log_source(events()), &store, &sink);
        assert_eq!(first.sync_once().await.unwrap(), stats(2, 0, 0, 0));
        drop(first);

        // Unchanged source events: only a full sync applies the new filter
        let mut r = fresh_runner(log_source(events()), skip_all_day(), &store, &sink);
        assert_eq!(r.full_sync_once().await.unwrap(), stats(0, 0, 1, 0));
        assert_eq!(sink.log().deletes, vec![href("allday")]);
        assert!(r.state.uid_mapping.get("allday").is_none());
        assert_eq!(sink.log().puts.len(), 2);
    }

    const WINDOW: Window = Window {
        days_past: 30,
        days_future: 30,
    };

    /// 2026-01-05 12:00 UTC, the day of `event`
    fn event_day() -> DateTime<Utc> {
        DateTime::from_timestamp(1767614400, 0).unwrap()
    }

    #[tokio::test]
    async fn test_events_outside_the_window_are_not_copied() {
        let store = Arc::new(MemStore::default());
        let sink = FakeSink::default();
        let later = event_with_body("later", "v1", "DTSTART:20270105T090000Z");
        let mut r = serial_runner(log_source(vec![event("e1", "v1"), later]), &store, &sink);
        r.window = Some(WINDOW);
        let (_tx, mut shutdown) = watch::channel(false);
        let stats_ = r.cycle(event_day(), &mut shutdown, false).await.unwrap();
        assert_eq!(stats_, stats(1, 0, 0, 0));
        assert!(r.state.uid_mapping.get("later").is_none());
    }

    #[tokio::test]
    async fn test_full_sync_removes_events_that_left_the_window() {
        let store = Arc::new(MemStore::default());
        let sink = FakeSink::default();
        let events = || {
            vec![
                event("e1", "v1"),
                event_with_body("e2", "v1", "DTSTART:20260301T090000Z"),
            ]
        };
        let mut r = serial_runner(log_source(events()), &store, &sink);
        r.window = Some(WINDOW);
        let (_tx, mut shutdown) = watch::channel(false);
        let stats_ = r.cycle(event_day(), &mut shutdown, false).await.unwrap();
        assert_eq!(stats_, stats(1, 0, 0, 0));
        // Two months later e1 is past the window and e2 in it
        let later = event_day() + chrono::TimeDelta::days(55);
        let stats_ = r.cycle(later, &mut shutdown, true).await.unwrap();
        assert_eq!(stats_, stats(1, 0, 1, 0));
        assert_eq!(sink.log().deletes, vec![href("e1")]);
        assert!(r.state.uid_mapping.get("e2").is_some());
    }

    /// Like a transform that now rejects them: the removal guard is for a
    /// source that lost its events, not for a changed setting
    #[tokio::test]
    async fn test_narrowing_the_window_removes_at_once() {
        let store = Arc::new(MemStore::default());
        let sink = FakeSink::default();
        let events = || {
            (1..=6)
                .map(|i| event(&format!("e{i}"), "v1"))
                .collect::<Vec<_>>()
        };
        let mut r = serial_runner(log_source(events()), &store, &sink);
        let (_tx, mut shutdown) = watch::channel(false);
        r.cycle(event_day(), &mut shutdown, false).await.unwrap();
        assert_eq!(r.state.uid_mapping.len(), 6);

        // A window that ends before the events
        r.window = Some(WINDOW);
        let later = event_day() + chrono::TimeDelta::days(60);
        assert_eq!(
            r.cycle(later, &mut shutdown, true).await.unwrap(),
            stats(0, 0, 6, 0)
        );
        assert_eq!(r.state.uid_mapping.len(), 0);
    }

    #[tokio::test]
    async fn test_changed_output_key_makes_the_next_cycle_a_full_sync() {
        let store = Arc::new(MemStore::default());
        let sink = FakeSink::default();
        let t0 = DateTime::from_timestamp(1767268800, 0).unwrap();
        let (_tx, mut shutdown) = watch::channel(false);
        let mut r = serial_runner(log_source(vec![event("e1", "v1")]), &store, &sink);
        r.cycle(t0, &mut shutdown, false).await.unwrap();
        // A state from before output keys takes the current one
        assert_eq!(r.state.output_key.as_deref(), Some("k1"));
        drop(r);

        // A failing sink listing shows whether a cycle was a full sync
        sink.log().fail_list = true;
        let mut r = SyncRunner::new(
            RunnerSettings {
                output_key: "k2".to_string(),
                ..settings(vec![])
            },
            log_source(vec![event("e1", "v1")]),
            sink.clone(),
            Arc::new(Semaphore::new(1)),
            store.clone(),
        )
        .unwrap();
        assert!(r.cycle(t0, &mut shutdown, false).await.is_err());
        assert_eq!(
            r.state.output_key.as_deref(),
            Some("k1"),
            "kept until a full sync completes"
        );

        sink.log().fail_list = false;
        r.cycle(t0, &mut shutdown, false).await.unwrap();
        assert_eq!(r.state.output_key.as_deref(), Some("k2"));
        assert_eq!(
            store
                .load(SYNC_NAME)
                .unwrap()
                .unwrap()
                .output_key
                .as_deref(),
            Some("k2")
        );
        sink.log().fail_list = true;
        r.cycle(t0, &mut shutdown, false).await.unwrap();
    }

    #[tokio::test]
    async fn test_full_sync_runs_once_a_day() {
        let store = Arc::new(MemStore::default());
        let sink = FakeSink::default();
        let mut r = serial_runner(log_source(vec![event("e1", "v1")]), &store, &sink);
        // 2026-01-01 12:00 UTC
        let t0 = DateTime::from_timestamp(1767268800, 0).unwrap();
        let (_tx, mut shutdown) = watch::channel(false);
        // A failing sink listing shows whether a cycle was a full sync
        sink.log().fail_list = true;

        // A state from before full syncs counts from its first cycle
        r.cycle(t0, &mut shutdown, false).await.unwrap();
        assert_eq!(
            store.load(SYNC_NAME).unwrap().unwrap().last_full_sync,
            Some(t0)
        );
        let late = t0 + TimeDelta::seconds(12 * 3600 - 1);
        r.cycle(late, &mut shutdown, false).await.unwrap();
        let midnight = late + TimeDelta::seconds(1);
        assert!(r.cycle(midnight, &mut shutdown, false).await.is_err());
        assert!(r.cycle(t0, &mut shutdown, true).await.is_err());

        sink.log().fail_list = false;
        assert_eq!(
            r.cycle(midnight, &mut shutdown, false).await.unwrap(),
            stats(0, 0, 0, 0)
        );
        assert_eq!(r.state.last_full_sync, Some(midnight));
        // Not again the same day
        sink.log().fail_list = true;
        let evening = midnight + TimeDelta::seconds(23 * 3600);
        r.cycle(evening, &mut shutdown, false).await.unwrap();
        assert_eq!(r.state.last_full_sync, Some(midnight));
        // After days off
        let later = midnight + TimeDelta::days(3);
        assert!(r.cycle(later, &mut shutdown, false).await.is_err());
    }

    #[tokio::test]
    async fn test_daily_full_sync_follows_the_time_zone() {
        let store = Arc::new(MemStore::default());
        let sink = FakeSink::default();
        let mut r = serial_runner(log_source(vec![event("e1", "v1")]), &store, &sink);
        // 2026-01-01 22:30 UTC, 23:30 in Berlin
        let t0 = DateTime::from_timestamp(1767306600, 0).unwrap();
        let (_tx, mut shutdown) = watch::channel(false);
        r.cycle(t0, &mut shutdown, false).await.unwrap();
        sink.log().fail_list = true;
        // An hour later: still Jan 1 in UTC, Jan 2 in Berlin
        let next = t0 + TimeDelta::hours(1);
        r.cycle(next, &mut shutdown, false).await.unwrap();
        r.time_zone = chrono_tz::Europe::Berlin;
        assert!(r.cycle(next, &mut shutdown, false).await.is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn test_cancelled_full_sync_is_not_recorded_and_runs_again() {
        let store = Arc::new(MemStore::default());
        let sink = FakeSink::default();
        sink.log().cancel_safe = true;
        sink.log().slow_after = Some(1);
        let events = || vec![event("e1", "v1"), event("e2", "v1"), event("e3", "v1")];

        // e1 is put, e2 is abandoned in backoff, e3 never starts
        let r = fresh_runner(log_source(events()), vec![], &store, &sink);
        let (r, status) = run_and_stop(r, BACKOFF / 2).await;
        drop(r);
        assert_eq!(status.last_result, None);
        let saved = store.load(SYNC_NAME).unwrap().unwrap();
        assert_eq!(saved.last_full_sync, Some(DateTime::UNIX_EPOCH));
        assert_eq!(saved.sync_token, None);
        assert!(saved.uid_mapping.get("e1").is_some());

        // The next start completes the full sync without duplicates
        sink.log().slow_after = None;
        let mut next = fresh_runner(log_source(events()), vec![], &store, &sink);
        next.sync_once().await.unwrap();
        let hrefs: HashSet<String> = sink.log().stored.keys().cloned().collect();
        let expected: HashSet<String> = ["e1", "e2", "e3"].iter().map(|u| href(u)).collect();
        assert_eq!(hrefs, expected);
        assert!(next.state.last_full_sync.unwrap() > DateTime::UNIX_EPOCH);
    }

    #[tokio::test(start_paused = true)]
    async fn test_cancelled_requested_full_sync_runs_again() {
        let store = Arc::new(MemStore::default());
        let sink = FakeSink::default();
        let events = vec![event("e1", "v1"), event("e2", "v1"), event("e3", "v1")];
        let mut r = serial_runner(log_source(events), &store, &sink);
        r.sync_once().await.unwrap();
        let first = r.state.last_full_sync;

        // A requested full sync that finds every copy outdated, stopped
        // while its second put is in backoff
        for ical in sink.log().stored.values_mut() {
            *ical = ical.replace("DTEND:20260105T100000Z", "DTEND:20260105T110000Z");
        }
        sink.log().cancel_safe = true;
        sink.log().slow_after = Some(4);
        let (stop, mut shutdown) = watch::channel(false);
        let task = tokio::spawn(async move {
            let result = r.cycle(Utc::now(), &mut shutdown, true).await;
            (r, result)
        });
        tokio::time::sleep(Duration::from_secs(1)).await;
        stop.send(true).unwrap();
        let (mut r, result) = task.await.unwrap();
        assert!(matches!(result, Err(Error::Cancelled)), "{result:?}");
        assert!(store.load(SYNC_NAME).unwrap().unwrap().full_sync_pending);

        // The next cycle is a full sync again, though nobody asked for it
        sink.log().slow_after = None;
        let (_stop, mut shutdown) = watch::channel(false);
        let later = Utc::now() + TimeDelta::seconds(60);
        r.cycle(later, &mut shutdown, false).await.unwrap();
        assert_eq!(r.state.last_full_sync, Some(later));
        assert_ne!(r.state.last_full_sync, first);
        assert!(!r.state.full_sync_pending);
    }

    /// iCal of a Calendar.app sink event with a legacy `[capycal:<source>]` marker,
    /// as the fake sink lists it: the sync marker without a source UID.
    fn legacy_ek_event() -> String {
        format!("BEGIN:VEVENT\r\nX-CAPYCAL-SYNC:{SYNC_NAME}\r\nEND:VEVENT")
    }

    #[tokio::test]
    async fn test_full_sync_replaces_unmapped_legacy_eventkit_events() {
        let store = Arc::new(MemStore::default());
        let sink = FakeSink::default();
        sink.log().assign_href_prefix = Some("ek-".to_string());
        sink.log()
            .stored
            .insert("ek-old".to_string(), legacy_ek_event());

        let mut r = fresh_runner(log_source(vec![event("e1", "v1")]), vec![], &store, &sink);
        assert_eq!(r.sync_once().await.unwrap(), stats(1, 0, 1, 0));
        assert_eq!(sink.log().deletes, vec!["ek-old".to_string()]);
        assert_eq!(
            r.state.uid_mapping.get("e1").unwrap().sink_href,
            format!("ek-{}", href("e1"))
        );
    }

    #[tokio::test]
    async fn test_full_sync_keeps_mapped_legacy_eventkit_events() {
        let store = Arc::new(MemStore::default());
        let sink = FakeSink::default();
        sink.log().assign_href_prefix = Some("ek-".to_string());
        sink.log()
            .stored
            .insert("ek-old".to_string(), legacy_ek_event());
        let mut state = SyncState::default();
        state.uid_mapping.insert(
            "e1".to_string(),
            SinkRecord {
                sink_uid: sink_uid("e1"),
                sink_href: "ek-old".to_string(),
                source_href: "/work/e1.ics".to_string(),
                source_etag: "v1".to_string(),
            },
        );
        store.save(SYNC_NAME, &state).unwrap();

        // The stored event still has the legacy marker, so it is put again with the new one
        let mut r = fresh_runner(log_source(vec![event("e1", "v1")]), vec![], &store, &sink);
        assert_eq!(r.full_sync_once().await.unwrap(), stats(0, 1, 0, 0));
        assert_eq!(put_hrefs(&sink), vec!["ek-old".to_string()]);
        assert!(sink.log().deletes.is_empty());
    }

    #[test]
    fn test_rebuild_mapping_prefers_the_mapped_then_the_deterministic_href() {
        let listed = |hrefs: &[&str]| -> Vec<SinkEvent<()>> {
            hrefs
                .iter()
                .map(|h| SinkEvent {
                    href: h.to_string(),
                    uid: None,
                    source_uid: Some("e1".to_string()),
                    stored: (),
                })
                .collect()
        };
        let sources = HashMap::new();
        let deterministic = format!("/home/{}", href("e1"));

        let rebuilt = rebuild_mapping(
            SYNC_NAME,
            &UidMapping::default(),
            listed(&["/home/a.ics", &deterministic, "/home/b.ics"]),
            &sources,
        );
        assert_eq!(rebuilt.mapping.get("e1").unwrap().sink_href, deterministic);
        assert_eq!(rebuilt.duplicates, 2);
        assert_eq!(
            rebuilt.mapping.get("e1").unwrap().source_href,
            format!("{ORPHAN_PREFIX}{deterministic}")
        );

        // A relative href in the old mapping matches the absolute listing
        let mut old = UidMapping::default();
        old.insert(
            "e1".to_string(),
            SinkRecord {
                sink_uid: "u".to_string(),
                sink_href: "b.ics".to_string(),
                source_href: "/work/e1.ics".to_string(),
                source_etag: "v1".to_string(),
            },
        );
        let rebuilt = rebuild_mapping(
            SYNC_NAME,
            &old,
            listed(&["/home/a.ics", &deterministic, "/home/b.ics"]),
            &sources,
        );
        assert_eq!(rebuilt.mapping.get("e1").unwrap().sink_href, "b.ics");
        let mut stray = rebuilt.stray.clone();
        stray.sort();
        assert_eq!(stray, vec!["/home/a.ics".to_string(), deterministic]);
    }

    #[test]
    fn test_generate_sink_uid_deterministic() {
        let uid1 = generate_sink_uid("work-to-personal", "source-uid-123");
        let uid2 = generate_sink_uid("work-to-personal", "source-uid-123");
        assert_eq!(uid1, uid2);
    }

    #[test]
    fn test_generate_sink_uid_different_syncs() {
        let uid1 = generate_sink_uid("sync-a", "source-uid-123");
        let uid2 = generate_sink_uid("sync-b", "source-uid-123");
        assert_ne!(uid1, uid2);
    }

    #[test]
    fn test_generate_sink_uid_format() {
        let uid = generate_sink_uid("test", "uid-1");
        assert!(uid.starts_with("capycal-"));
    }
}
