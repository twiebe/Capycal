use std::any::Any;
use std::collections::{BTreeSet, HashMap};
use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::sync::{Semaphore, watch};
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

use crate::config::{Config, SyncConfig};
use crate::error::{Error, Result};
use crate::state::StateStore;
use crate::state::store::FsStateStore;
use crate::sync::factory::{AnySink, AnySource, build_sink, build_source};
use crate::sync::maintenance;
use crate::sync::runner::{RunnerSettings, SyncRunner, Wake};
use crate::sync::status::{StatusReporter, SyncStatus};

/// Placeholder source URL for EventKit calendars, which have no URL.
const EVENTKIT_SOURCE_URL: &str = "eventkit://local";
/// Placeholder source URL for feeds, whose address is a secret.
const ICS_SOURCE_URL: &str = "ics://feed";
/// How long to wait for a sync task to finish after signalling shutdown.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(30);

struct RunnerHandle {
    name: String,
    handle: JoinHandle<()>,
    shutdown_tx: watch::Sender<bool>,
    wake: Arc<Wake>,
}

/// Runs the configured syncs as tasks and publishes their status.
#[derive(Default)]
pub struct Coordinator {
    runners: Vec<RunnerHandle>,
    status: watch::Sender<Vec<SyncStatus>>,
    /// Syncs not started until resumed, kept across reloads while configured.
    paused: BTreeSet<String>,
    /// Sink semaphores of the running config, shared with resumed syncs.
    semaphores: HashMap<String, Arc<Semaphore>>,
}

/// Keeps the status of every sync in one list, so front ends watch a single value.
impl StatusReporter for watch::Sender<Vec<SyncStatus>> {
    fn report(&self, status: &SyncStatus) {
        self.send_modify(|all| match all.iter_mut().find(|s| s.name == status.name) {
            Some(entry) => *entry = status.clone(),
            None => all.push(status.clone()),
        });
    }
}

type Runner = SyncRunner<AnySource, AnySink, FsStateStore>;

/// Wire the adapters, settings, sink semaphore and store for one sync.
fn build_runner(
    config: &Config,
    sync_config: &SyncConfig,
    semaphores: &HashMap<String, Arc<Semaphore>>,
    store: Arc<FsStateStore>,
) -> Result<Runner> {
    let source_cal = config
        .calendars
        .get(&sync_config.source)
        .ok_or_else(|| Error::Config(format!("unknown calendar '{}'", sync_config.source)))?;
    let target_cal = config
        .calendars
        .get(&sync_config.target)
        .ok_or_else(|| Error::Config(format!("unknown calendar '{}'", sync_config.target)))?;
    let source_url = match &source_cal.url {
        Some(url) => url,
        None if source_cal.is_ics() => ICS_SOURCE_URL,
        None => EVENTKIT_SOURCE_URL,
    };
    let output_key = sync_config.output_key(source_cal, target_cal);
    let settings = runner_settings(sync_config, source_url, output_key)?;
    let sink_sem = semaphores
        .get(&sync_config.target)
        .cloned()
        .unwrap_or_else(|| Arc::new(Semaphore::new(1)));
    let source = build_source(config, sync_config)?;
    let sink = build_sink(config, &sync_config.target)?;
    SyncRunner::new(settings, source, sink, sink_sem, store)
}

/// Build one runner per configured sync, failing as a whole on the first error.
fn build_runners(
    config: &Config,
    semaphores: &HashMap<String, Arc<Semaphore>>,
) -> Result<Vec<Runner>> {
    let store = Arc::new(FsStateStore::new(config.resolve_state_dir()?)?);
    config
        .syncs
        .iter()
        .filter(|s| s.enabled)
        .map(|sync_config| build_runner(config, sync_config, semaphores, store.clone()))
        .collect()
}

impl Coordinator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start every enabled sync of `config` except the paused ones. Paused
    /// syncs no longer in the config are forgotten.
    pub fn start(&mut self, config: &Config) -> Result<()> {
        let semaphores = build_semaphores(config);
        let runners = build_runners(config, &semaphores)?;
        let store = FsStateStore::new(config.resolve_state_dir()?)?;
        for sync in config.syncs.iter().filter(|s| !s.enabled) {
            // Only costs the full sync on enabling; not worth stopping the others
            if let Err(e) = mark_disabled(&store, &sync.name) {
                warn!(sync = %sync.name, error = %e, "cannot mark the disabled sync");
            }
        }
        self.semaphores = semaphores;
        self.paused
            .retain(|name| runners.iter().any(|r| &r.name == name));
        self.status.send_modify(|all| all.clear());
        let mut runners = runners.into_iter();
        for sync in &config.syncs {
            if !sync.enabled {
                self.status.report(&disabled_status(&sync.name));
                continue;
            }
            let runner = runners.next().expect("one runner per enabled sync");
            if self.paused.contains(&runner.name) {
                self.status.report(&paused_status(&runner.name));
            } else {
                self.spawn(runner);
            }
        }
        Ok(())
    }

    /// Build the new runners once before stopping the old ones, so an invalid
    /// config leaves the running syncs untouched. The runners actually spawned
    /// are built again after the old ones stopped: building loads the sync
    /// state, which the old runners may still update in their last cycle.
    pub async fn reload(&mut self, config: &Config) -> Result<()> {
        build_runners(config, &build_semaphores(config))?;
        self.stop_all().await;
        self.start(config)
    }

    /// Stop a running sync and keep it stopped until `resume`, also across
    /// reloads while it stays in the config. Returns false if no running or
    /// paused sync has this name.
    pub async fn pause(&mut self, name: &str) -> bool {
        if !self.runners.iter().any(|r| r.name == name) && !self.paused.contains(name) {
            return false;
        }
        self.stop_sync(name).await;
        self.paused.insert(name.to_string());
        self.status.report(&paused_status(name));
        true
    }

    /// Start a paused sync of `config` again. Does nothing if it is not paused.
    pub fn resume(&mut self, config: &Config, name: &str) -> Result<()> {
        if !self.paused.contains(name) {
            return Ok(());
        }
        let sync_config = config
            .syncs
            .iter()
            .find(|s| s.name == name)
            .ok_or_else(|| Error::Config(format!("sync '{name}' not found in config")))?;
        if !sync_config.enabled {
            self.paused.remove(name);
            self.status.report(&disabled_status(name));
            return Ok(());
        }
        let store = Arc::new(FsStateStore::new(config.resolve_state_dir()?)?);
        let runner = build_runner(config, sync_config, &self.semaphores, store)?;
        self.paused.remove(name);
        self.spawn(runner);
        Ok(())
    }

    /// Pause the sync, then delete the events it created from its sink and
    /// forget its state (see `maintenance::purge_sync`). Returns (deleted,
    /// errors). If the sink cannot be read, the sync is resumed and the
    /// error returned.
    pub async fn purge(
        &mut self,
        config: &Config,
        name: &str,
        progress: maintenance::Progress,
    ) -> Result<(usize, usize)> {
        self.pause(name).await;
        let purged = maintenance::purge_sync(config, name, progress).await;
        if purged.is_err()
            && let Err(e) = self.resume(config, name)
        {
            error!(sync = %name, error = %e, "cannot resume the sync after a failed purge");
        }
        purged
    }

    /// Pause every sync writing into `calendar`, then delete every event in
    /// it and forget their state (see `maintenance::purge_calendar`). If the
    /// calendar cannot be read, the syncs are resumed and the error returned.
    pub async fn purge_calendar(
        &mut self,
        config: &Config,
        calendar: &str,
        progress: maintenance::Progress,
    ) -> Result<(usize, usize)> {
        let writing: Vec<String> = (config.syncs.iter())
            .filter(|s| s.target == calendar)
            .map(|s| s.name.clone())
            .collect();
        for name in &writing {
            self.pause(name).await;
        }
        let purged = maintenance::purge_calendar(config, calendar, progress).await;
        if purged.is_err() {
            for name in &writing {
                if let Err(e) = self.resume(config, name) {
                    error!(sync = %name, error = %e, "cannot resume the sync after a failed purge");
                }
            }
        }
        purged
    }

    async fn stop_all(&mut self) {
        let names: Vec<String> = self.runners.iter().map(|r| r.name.clone()).collect();
        for name in &names {
            self.stop_sync(name).await;
        }
    }

    /// Status of every running sync, updated when a cycle starts or ends.
    pub fn status(&self) -> watch::Receiver<Vec<SyncStatus>> {
        self.status.subscribe()
    }

    /// Start the next cycle of the named sync, or of all syncs, without
    /// waiting for the interval. A sync in the middle of a cycle runs
    /// once more right after it. Returns false if no running sync matched.
    pub fn sync_now(&self, name: Option<&str>) -> bool {
        let mut matched = false;
        for runner in &self.runners {
            if name.is_none_or(|n| n == runner.name) {
                runner.wake.sync_now();
                matched = true;
            }
        }
        matched
    }

    /// Like `sync_now`, but the next cycle is a full sync: the whole source
    /// is compared with the sink. Returns false if no running sync matched.
    pub fn full_sync(&self, name: Option<&str>) -> bool {
        let mut matched = false;
        for runner in &self.runners {
            if name.is_none_or(|n| n == runner.name) {
                runner.wake.full_sync();
                matched = true;
            }
        }
        matched
    }

    fn spawn(&mut self, mut runner: Runner) {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let wake = Arc::new(Wake::default());
        let name = runner.name.clone();

        info!(sync = %name, source = %runner.source_name, target = %runner.sink_name, "sync started");

        self.status.report(&SyncStatus::new(&name));
        let reporter = self.status.clone();
        let task_wake = wake.clone();
        let task_name = name.clone();
        let handle = tokio::spawn(async move {
            let run = CatchUnwind(Box::pin(runner.run(shutdown_rx, &task_wake, &reporter)));
            // A panic would otherwise leave the sync shown as running forever
            if let Err(panic) = run.await {
                let message = panic_message(panic.as_ref());
                error!(sync = %task_name, error = %message, "sync stopped after an internal error");
                let mut status = SyncStatus::new(&task_name);
                status.last_result = Some(Err(format!(
                    "internal error, sync stopped until reload: {message}"
                )));
                reporter.report(&status);
            }
        });

        self.runners.push(RunnerHandle {
            name,
            handle,
            shutdown_tx,
            wake,
        });
    }

    async fn stop_sync(&mut self, name: &str) {
        if let Some(idx) = self.runners.iter().position(|r| r.name == name) {
            let runner = self.runners.remove(idx);
            let _ = runner.shutdown_tx.send(true);
            join(runner).await;
            self.status
                .send_modify(|all| all.retain(|status| status.name != name));
        }
    }

    /// Run one cycle of every sync, a full sync with `full`.
    pub async fn run_once(config: &Config, full: bool) -> Result<bool> {
        let store = Arc::new(FsStateStore::new(config.resolve_state_dir()?)?);
        let mut all_ok = true;
        let semaphores = build_semaphores(config);

        for sync_config in config.syncs.iter().filter(|s| !s.enabled) {
            mark_disabled(&store, &sync_config.name)?;
            info!(sync = %sync_config.name, "sync disabled, skipped");
        }
        for sync_config in config.syncs.iter().filter(|s| s.enabled) {
            let mut runner = build_runner(config, sync_config, &semaphores, store.clone())?;
            let result = if full {
                runner.full_sync_once().await
            } else {
                runner.sync_once().await
            };
            match result {
                Ok(_) => info!(sync = %sync_config.name, "sync done"),
                Err(e) => {
                    error!(sync = %sync_config.name, error = %e, "sync failed");
                    all_ok = false;
                }
            }
        }

        Ok(all_ok)
    }

    pub async fn shutdown(self) {
        info!("signaling shutdown to all syncs");
        for runner in &self.runners {
            let _ = runner.shutdown_tx.send(true);
        }

        for runner in self.runners {
            join(runner).await;
        }
    }
}

/// Make the first cycle of a disabled sync a full sync once it is enabled
/// again, whatever changed while it did not run.
fn mark_disabled(store: &FsStateStore, name: &str) -> Result<()> {
    if let Some(mut state) = store.load(name)?
        && !state.full_sync_pending
    {
        state.full_sync_pending = true;
        store.save(name, &state)?;
    }
    Ok(())
}

fn disabled_status(name: &str) -> SyncStatus {
    SyncStatus {
        disabled: true,
        ..SyncStatus::new(name)
    }
}

fn paused_status(name: &str) -> SyncStatus {
    SyncStatus {
        paused: true,
        ..SyncStatus::new(name)
    }
}

/// Wait for a signalled sync task to finish, giving up after SHUTDOWN_TIMEOUT.
/// A future that turns a panic while polling into an `Err`.
struct CatchUnwind<F>(Pin<Box<F>>);

impl<F: Future> Future for CatchUnwind<F> {
    type Output = std::result::Result<F::Output, Box<dyn Any + Send>>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match catch_unwind(AssertUnwindSafe(|| self.0.as_mut().poll(cx))) {
            Ok(Poll::Ready(output)) => Poll::Ready(Ok(output)),
            Ok(Poll::Pending) => Poll::Pending,
            Err(panic) => Poll::Ready(Err(panic)),
        }
    }
}

fn panic_message(panic: &(dyn Any + Send)) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".to_string())
}

/// Wait for a stopped sync's task. One that hangs past the timeout is
/// aborted, so it cannot save state while a new runner of the same sync or
/// a purge already works on it.
async fn join(mut runner: RunnerHandle) {
    let name = runner.name;
    match tokio::time::timeout(SHUTDOWN_TIMEOUT, &mut runner.handle).await {
        Ok(Ok(())) => info!(sync = %name, "sync stopped"),
        Ok(Err(e)) => error!(sync = %name, error = %e, "sync task failed"),
        Err(_) => {
            runner.handle.abort();
            error!(sync = %name, "sync task did not stop within timeout, aborted");
        }
    }
}

/// Map a sync's config onto the settings the runner needs.
fn runner_settings(
    sync_config: &SyncConfig,
    source_url: &str,
    output_key: String,
) -> Result<RunnerSettings> {
    Ok(RunnerSettings {
        name: sync_config.name.clone(),
        source_name: sync_config.source.clone(),
        sink_name: sync_config.target.clone(),
        source_url: source_url.to_string(),
        fields: sync_config.fields.iter().cloned().collect(),
        transforms: sync_config.compile_transforms()?,
        interval: sync_config.interval,
        time_zone: crate::ical::transform::local_tz(),
        output_key,
    })
}

/// Build a shared semaphore per calendar based on its concurrency setting.
pub fn build_semaphores(config: &Config) -> HashMap<String, Arc<Semaphore>> {
    config
        .calendars
        .iter()
        .map(|(name, cal)| {
            let permits = cal.concurrency.max(1);
            (name.clone(), Arc::new(Semaphore::new(permits)))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::status::CycleStats;

    #[tokio::test(start_paused = true)]
    async fn test_join_aborts_a_task_that_does_not_stop() {
        let held = Arc::new(());
        let task_held = held.clone();
        let handle = tokio::spawn(async move {
            let _held = task_held;
            std::future::pending::<()>().await;
        });
        let (shutdown_tx, _) = watch::channel(false);
        join(RunnerHandle {
            name: "stuck".to_string(),
            handle,
            shutdown_tx,
            wake: Arc::new(Wake::default()),
        })
        .await;
        tokio::task::yield_now().await;
        assert_eq!(Arc::strong_count(&held), 1, "the task still runs");
    }

    #[tokio::test]
    async fn test_catch_unwind_turns_a_panic_into_an_error() {
        assert_eq!(CatchUnwind(Box::pin(async { 7 })).await.unwrap(), 7);
        let panicked = CatchUnwind(Box::pin(async {
            tokio::task::yield_now().await;
            panic!("byte index 463 is out of bounds")
        }))
        .await
        .unwrap_err();
        assert_eq!(
            panic_message(panicked.as_ref()),
            "byte index 463 is out of bounds"
        );
    }

    fn sync_config(transforms: &str) -> SyncConfig {
        let mut sync: SyncConfig = toml::from_str(&format!(
            r#"
source = "work"
target = "personal"
interval = "1h"
fields = ["DTSTART", "DTEND"]
{transforms}
"#
        ))
        .unwrap();
        sync.name = "work-to-personal".to_string();
        sync
    }

    #[test]
    fn test_runner_settings_from_config() {
        let cfg = sync_config(
            r#"
[[transforms]]
field = "SUMMARY"
action = "set_field"
value = "Busy"
"#,
        );
        let settings = runner_settings(&cfg, "https://example.com/work/", "k".to_string()).unwrap();
        assert_eq!(settings.name, "work-to-personal");
        assert_eq!(settings.source_name, "work");
        assert_eq!(settings.sink_name, "personal");
        assert_eq!(settings.source_url, "https://example.com/work/");
        assert!(settings.fields.contains("DTSTART"));
        assert!(settings.fields.contains("DTEND"));
        assert_eq!(settings.transforms.len(), 1);
        assert_eq!(settings.interval, Duration::from_secs(3600));
    }

    #[test]
    fn test_runner_settings_rejects_invalid_transform() {
        let cfg = sync_config(
            r#"
[[transforms]]
field = "SUMMARY"
action = "replace_field_regex"
pattern = "("
"#,
        );
        assert!(runner_settings(&cfg, "https://example.com/work/", "k".to_string()).is_err());
    }

    use std::path::{Path, PathBuf};
    use wiremock::matchers::{body_string_contains, header, method, path_regex};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const SOURCE_ICS: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:e1\r\n\
        DTSTART:20260105T090000Z\r\nDTEND:20260105T100000Z\r\n\
        SUMMARY:Secret meeting\r\nEND:VEVENT\r\nEND:VCALENDAR";

    fn state_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("capycal-{}-coord-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// One sync from `work` to `home`, both served by `server`.
    fn config(server: &MockServer, state_dir: &Path, home_password: &str, extra: &str) -> Config {
        toml::from_str(&format!(
            r#"
[daemon]
state_dir = "{state_dir}"

[accounts.work]
url = "{uri}/"
username = "u"
password = "p"

[accounts.home]
url = "{uri}/"
username = "u"
password = {home_password}

[calendars.work]
account = "work"
url = "{uri}/work/"

[calendars.home]
account = "home"
url = "{uri}/home/"

[syncs.work-to-home]
source = "work"
target = "home"
interval = "1h"
fields = ["DTSTART", "DTEND"]
{extra}
"#,
            state_dir = state_dir.display(),
            uri = server.uri(),
        ))
        .unwrap()
    }

    const BUSY: &str = r#"
transforms = [
  { field = "SUMMARY", action = "set_field", value = "Busy" },
]
"#;

    /// An empty listing of the sink `home`, as a full sync reads it. Other
    /// listings mounted by a test take precedence.
    async fn mount_empty_sink_listing(server: &MockServer) {
        Mock::given(method("PROPFIND"))
            .and(path_regex("^/home/?$"))
            .and(header("Depth", "1"))
            .respond_with(
                ResponseTemplate::new(207)
                    .set_body_string(r#"<d:multistatus xmlns:d="DAV:"></d:multistatus>"#),
            )
            .with_priority(10)
            .mount(server)
            .await;
    }

    /// Serve one source event and accept its puts. A sync without state
    /// starts with a full sync, which also lists the (empty) sink.
    async fn mount_source_and_sink(server: &MockServer, expected_cycles: u64) {
        mount_empty_sink_listing(server).await;
        Mock::given(method("REPORT"))
            .and(path_regex("^/work/?$"))
            .and(body_string_contains("sync-collection"))
            .respond_with(ResponseTemplate::new(207).set_body_string(format!(
                r#"<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
<d:response><d:href>/work/e1.ics</d:href><d:propstat><d:prop>
<d:getetag>"v1"</d:getetag><c:calendar-data>{SOURCE_ICS}</c:calendar-data>
</d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
<d:sync-token>tok-1</d:sync-token></d:multistatus>"#
            )))
            .expect(expected_cycles)
            .mount(server)
            .await;
        Mock::given(method("PUT"))
            .and(path_regex(r"^/home/capycal-[0-9a-f-]+\.ics$"))
            .respond_with(ResponseTemplate::new(201))
            .expect(expected_cycles)
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn test_run_once_copies_filtered_event_and_saves_state() {
        let server = MockServer::start().await;
        mount_source_and_sink(&server, 1).await;
        let dir = state_dir("run-once");
        let cfg = config(&server, &dir, "\"p\"", BUSY);

        assert!(Coordinator::run_once(&cfg, false).await.unwrap());

        let requests = server.received_requests().await.unwrap();
        let put = requests
            .iter()
            .find(|r| r.method.as_str() == "PUT")
            .unwrap();
        let body = String::from_utf8_lossy(&put.body);
        assert!(body.contains("DTSTART:20260105T090000Z"), "{body}");
        assert!(body.contains("SUMMARY:Busy"), "{body}");
        assert!(body.contains("X-CAPYCAL-SYNC:work-to-home"), "{body}");
        assert!(!body.contains("Secret"), "{body}");

        let state = std::fs::read_to_string(dir.join("work-to-home.json")).unwrap();
        assert!(state.contains("tok-1"), "{state}");
        assert!(state.contains("/work/e1.ics"), "{state}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_run_once_reports_failed_sync() {
        // Unmocked requests get 404, so both sync-collection and the ctag fallback fail
        let server = MockServer::start().await;
        let dir = state_dir("run-once-fail");
        let cfg = config(&server, &dir, "\"p\"", "");

        assert!(!Coordinator::run_once(&cfg, false).await.unwrap());
        assert!(!dir.join("work-to-home.json").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_run_once_rejects_unresolvable_credential() {
        let server = MockServer::start().await;
        let dir = state_dir("run-once-cred");
        let cfg = config(
            &server,
            &dir,
            r#"{ env = "CAPYCAL_TEST_UNSET_PASSWORD_VAR" }"#,
            "",
        );

        let err = Coordinator::run_once(&cfg, false).await.unwrap_err();
        assert!(err.to_string().contains("account 'home'"), "{err}");
        assert!(server.received_requests().await.unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_start_rejects_invalid_transform() {
        let server = MockServer::start().await;
        let dir = state_dir("start-invalid");
        let cfg = config(
            &server,
            &dir,
            "\"p\"",
            "[[syncs.work-to-home.transforms]]\nfield = \"SUMMARY\"\naction = \"replace_field_regex\"\npattern = \"(\"",
        );

        assert!(Coordinator::run_once(&cfg, false).await.is_err());
        let mut coordinator = Coordinator::new();
        assert!(coordinator.start(&cfg).is_err());
        assert!(coordinator.runners.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_reload_restarts_syncs_and_shutdown_stops_them() {
        let server = MockServer::start().await;
        // Each started runner syncs once, then waits for the next poll or shutdown
        mount_source_and_sink(&server, 2).await;
        let dir = state_dir("reload");
        let cfg = config(&server, &dir, "\"p\"", "");

        let mut coordinator = Coordinator::new();
        let mut status = coordinator.status();
        coordinator.reload(&cfg).await.unwrap();
        assert_eq!(coordinator.runners.len(), 1);
        wait_for_result(&mut status, 1, 0).await;
        coordinator.reload(&cfg).await.unwrap();
        assert_eq!(coordinator.runners.len(), 1);
        wait_for_result(&mut status, 0, 1).await;
        coordinator.shutdown().await;

        server.verify().await;
        let state = std::fs::read_to_string(dir.join("work-to-home.json")).unwrap();
        assert!(state.contains("tok-1"), "{state}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_reload_runner_sees_state_from_old_runners_last_cycle() {
        let server = MockServer::start().await;
        // The old runner finishes its first cycle (saving tok-1) before the
        // reload, so the new runner must continue from tok-1.
        Mock::given(method("REPORT"))
            .and(path_regex("^/work/?$"))
            .and(body_string_contains("<d:sync-token>tok-1</d:sync-token>"))
            .respond_with(ResponseTemplate::new(207).set_body_string(
                r#"<d:multistatus xmlns:d="DAV:"><d:sync-token>tok-2</d:sync-token></d:multistatus>"#,
            ))
            .expect(1)
            .mount(&server)
            .await;
        mount_source_and_sink(&server, 1).await;
        let dir = state_dir("reload-state");
        let cfg = config(&server, &dir, "\"p\"", "");

        let mut coordinator = Coordinator::new();
        let mut status = coordinator.status();
        coordinator.start(&cfg).unwrap();
        wait_for_result(&mut status, 1, 0).await;
        coordinator.reload(&cfg).await.unwrap();
        wait_for_result(&mut status, 0, 0).await;
        coordinator.shutdown().await;

        server.verify().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Wait until the only sync's last cycle created and updated the given counts.
    async fn wait_for_result(
        status: &mut watch::Receiver<Vec<SyncStatus>>,
        created: usize,
        updated: usize,
    ) {
        let expected = Some(Ok(CycleStats {
            created,
            updated,
            ..Default::default()
        }));
        tokio::time::timeout(
            Duration::from_secs(5),
            status.wait_for(|all| all.len() == 1 && all[0].last_result == expected),
        )
        .await
        .expect("sync cycle did not finish")
        .unwrap();
    }

    /// Requests the server received with this method and Depth header.
    async fn requests(server: &MockServer, method: &str, depth: &str) -> usize {
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| {
                r.method.as_str() == method && r.headers.get("Depth").is_some_and(|d| d == depth)
            })
            .count()
    }

    #[tokio::test]
    async fn test_full_sync_compares_with_the_sink_again() {
        let server = MockServer::start().await;
        mount_source_and_sink(&server, 2).await;
        let dir = state_dir("full-sync");
        let cfg = config(&server, &dir, "\"p\"", "");

        let mut coordinator = Coordinator::new();
        let mut status = coordinator.status();
        coordinator.start(&cfg).unwrap();
        // Without state the first cycle is a full sync
        wait_for_result(&mut status, 1, 0).await;
        assert_eq!(requests(&server, "PROPFIND", "1").await, 1);
        let state = std::fs::read_to_string(dir.join("work-to-home.json")).unwrap();
        assert!(state.contains("last_full_sync"), "{state}");

        assert!(!coordinator.full_sync(Some("unknown")));
        assert!(coordinator.full_sync(Some("work-to-home")));
        // The (mocked) sink no longer lists the event, so it is put again
        wait_for_result(&mut status, 0, 1).await;
        assert_eq!(requests(&server, "PROPFIND", "1").await, 2);
        coordinator.shutdown().await;
        server.verify().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_status_and_sync_now() {
        let server = MockServer::start().await;
        // The interval is an hour, so the second cycle only runs through sync_now
        mount_source_and_sink(&server, 2).await;
        let dir = state_dir("sync-now");
        let cfg = config(&server, &dir, "\"p\"", "");

        let mut coordinator = Coordinator::new();
        let mut status = coordinator.status();
        coordinator.start(&cfg).unwrap();
        wait_for_result(&mut status, 1, 0).await;
        assert!(!coordinator.sync_now(Some("unknown")));
        assert!(coordinator.sync_now(Some("work-to-home")));
        wait_for_result(&mut status, 0, 1).await;
        assert_eq!(status.borrow()[0].name, "work-to-home");

        coordinator.shutdown().await;
        server.verify().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn is_paused(status: &watch::Receiver<Vec<SyncStatus>>) -> bool {
        let all = status.borrow();
        all.len() == 1 && all[0].paused && !all[0].running
    }

    #[tokio::test]
    async fn test_disabled_sync_gets_no_runner_and_full_syncs_when_enabled() {
        let server = MockServer::start().await;
        let dir = state_dir("disabled");
        let store = FsStateStore::new(dir.clone()).unwrap();
        store
            .save("work-to-home", &crate::state::SyncState::default())
            .unwrap();

        let mut coordinator = Coordinator::new();
        coordinator
            .start(&config(&server, &dir, "\"p\"", "enabled = false"))
            .unwrap();
        let status = coordinator.status().borrow().clone();
        assert_eq!(status.len(), 1);
        assert!(status[0].disabled);
        assert!(!coordinator.sync_now(Some("work-to-home")));
        assert!(
            store
                .load("work-to-home")
                .unwrap()
                .unwrap()
                .full_sync_pending
        );
        coordinator.shutdown().await;
    }

    #[tokio::test]
    async fn test_reload_stops_a_sync_that_gets_disabled() {
        let server = MockServer::start().await;
        let dir = state_dir("disable-on-reload");
        let mut coordinator = Coordinator::new();
        coordinator
            .start(&config(&server, &dir, "\"p\"", ""))
            .unwrap();
        assert!(coordinator.sync_now(Some("work-to-home")));

        coordinator
            .reload(&config(&server, &dir, "\"p\"", "enabled = false"))
            .await
            .unwrap();
        assert!(!coordinator.sync_now(Some("work-to-home")));
        let status = coordinator.status().borrow().clone();
        assert!(status[0].disabled && !status[0].running);
        coordinator.shutdown().await;
    }

    #[tokio::test]
    async fn test_pause_survives_reload_until_resumed() {
        let server = MockServer::start().await;
        // The first runner and the resumed one sync once each
        mount_source_and_sink(&server, 2).await;
        let dir = state_dir("pause");
        let cfg = config(&server, &dir, "\"p\"", "");

        let mut coordinator = Coordinator::new();
        let mut status = coordinator.status();
        coordinator.start(&cfg).unwrap();
        wait_for_result(&mut status, 1, 0).await;
        assert!(!coordinator.pause("unknown").await);
        assert!(coordinator.pause("work-to-home").await);
        assert!(coordinator.runners.is_empty());
        assert!(is_paused(&status));

        coordinator.reload(&cfg).await.unwrap();
        assert!(coordinator.runners.is_empty());
        assert!(is_paused(&status));
        assert!(!coordinator.sync_now(None));

        coordinator.resume(&cfg, "work-to-home").unwrap();
        assert_eq!(coordinator.runners.len(), 1);
        wait_for_result(&mut status, 0, 1).await;
        assert!(!status.borrow()[0].paused);

        // A reload without the sync forgets that it was paused
        coordinator.pause("work-to-home").await;
        let empty: Config =
            toml::from_str(&format!("[daemon]\nstate_dir = \"{}\"\n", dir.display())).unwrap();
        coordinator.reload(&empty).await.unwrap();
        assert!(status.borrow().is_empty());
        assert!(coordinator.paused.is_empty());
        coordinator.shutdown().await;
        server.verify().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_purge_deletes_the_syncs_events_and_keeps_it_paused() {
        use crate::sync::maintenance::tests::{event, expect_deletes, mount_home};
        let server = MockServer::start().await;
        mount_source_and_sink(&server, 1).await;
        mount_home(
            &server,
            &["/home/a.ics", "/home/b.ics"],
            &[
                event("/home/a.ics", "X-CAPYCAL-SYNC:work-to-home\n"),
                event("/home/b.ics", ""),
            ],
        )
        .await;
        expect_deletes(&server, &["/home/a.ics"], 1).await;
        expect_deletes(&server, &["/home/b.ics"], 0).await;
        let dir = state_dir("purge");
        // A state from before full syncs, so the first cycle does not list the sink
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("work-to-home.json"),
            r#"{"uid_mapping":{"entries":{}}}"#,
        )
        .unwrap();
        let cfg = config(&server, &dir, "\"p\"", "");

        let mut coordinator = Coordinator::new();
        let mut status = coordinator.status();
        coordinator.start(&cfg).unwrap();
        wait_for_result(&mut status, 1, 0).await;
        assert!(dir.join("work-to-home.json").exists());

        assert_eq!(
            coordinator
                .purge(&cfg, "work-to-home", maintenance::no_progress())
                .await
                .unwrap(),
            (1, 0)
        );
        assert!(coordinator.runners.is_empty());
        assert!(is_paused(&status));
        assert!(!dir.join("work-to-home.json").exists());
        coordinator.shutdown().await;
        server.verify().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_failed_purge_resumes_the_sync() {
        // Unmocked requests get 404, so listing the sink fails
        let server = MockServer::start().await;
        let dir = state_dir("purge-fail");
        let cfg = config(&server, &dir, "\"p\"", "");

        let mut coordinator = Coordinator::new();
        let status = coordinator.status();
        coordinator.start(&cfg).unwrap();
        assert!(
            coordinator
                .purge(&cfg, "work-to-home", maintenance::no_progress())
                .await
                .is_err()
        );
        assert_eq!(coordinator.runners.len(), 1);
        assert!(!status.borrow()[0].paused);
        coordinator.shutdown().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_reload_and_shutdown_cancel_a_cycle_in_backoff() {
        let server = MockServer::start().await;
        Mock::given(method("REPORT"))
            .respond_with(ResponseTemplate::new(503).insert_header("Retry-After", "30"))
            .mount(&server)
            .await;
        let dir = state_dir("reload-backoff");
        let cfg = config(&server, &dir, "\"p\"", "");

        let mut coordinator = Coordinator::new();
        let mut status = coordinator.status();
        coordinator.start(&cfg).unwrap();
        let in_backoff = async || {
            while server.received_requests().await.unwrap().is_empty() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        in_backoff().await;

        tokio::time::timeout(Duration::from_secs(5), coordinator.reload(&cfg))
            .await
            .expect("reload waited for the backoff")
            .unwrap();
        server.reset().await;
        Mock::given(method("REPORT"))
            .respond_with(ResponseTemplate::new(503).insert_header("Retry-After", "30"))
            .mount(&server)
            .await;
        in_backoff().await;
        tokio::time::timeout(Duration::from_secs(5), coordinator.shutdown())
            .await
            .expect("shutdown waited for the backoff");

        // Cancelled cycles report no result and save no state
        let all = status.borrow_and_update();
        assert_eq!(all.len(), 1);
        assert!(!all[0].running);
        assert_eq!(all[0].last_result, None);
        assert!(!dir.join("work-to-home.json").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_failed_reload_keeps_previous_syncs() {
        let server = MockServer::start().await;
        mount_source_and_sink(&server, 1).await;
        let dir = state_dir("reload-fail");
        let cfg = config(&server, &dir, "\"p\"", "");
        let bad = config(
            &server,
            &dir,
            r#"{ env = "CAPYCAL_TEST_UNSET_PASSWORD_VAR" }"#,
            "",
        );

        let mut coordinator = Coordinator::new();
        let mut status = coordinator.status();
        coordinator.start(&cfg).unwrap();
        assert!(coordinator.reload(&bad).await.is_err());
        assert_eq!(coordinator.runners.len(), 1);
        wait_for_result(&mut status, 1, 0).await;
        coordinator.shutdown().await;
        let _ = std::fs::remove_dir_all(&dir);
    }
}
