use std::sync::Arc;

use tracing::{debug, error, info, warn};

use crate::caldav::client::CalDavClient;
use crate::caldav::source::synced_events;
use crate::config::{CalendarConfig, Config, SyncConfig};
use crate::error::{Error, Result};
use crate::state::StateStore;
use crate::state::store::FsStateStore;
use crate::sync::factory::caldav_client;

/// Reports how many events a purge has handled (deleted or failed) so far.
pub type Progress = Arc<dyn Fn(usize) + Send + Sync>;

/// Forget what a sync synced, keeping its events in the target (a deleted
/// sync whose copies stay).
pub fn forget_sync_state(config: &Config, sync_name: &str) -> Result<()> {
    FsStateStore::new(config.resolve_state_dir()?)?.remove(sync_name)
}

/// A `Progress` that reports nowhere.
pub fn no_progress() -> Progress {
    Arc::new(|_| {})
}

/// The source's next event from `now` on, as iCal, for the preview of a
/// sync that may not be saved yet: `sync` reads from the calendar `source`
/// is the config of. None when nothing is ahead.
pub async fn next_source_event(
    config: &Config,
    sync: &SyncConfig,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Option<String>> {
    use crate::source::EventSource;
    let source = crate::sync::factory::build_source(config, sync)?;
    let (events, _) = source
        .fetch_changes(&mut crate::state::SyncState::default())
        .await?;
    Ok(next_event(events.into_iter().map(|e| e.calendar_data), now))
}

/// The event that starts first at or after `now` (by its DTSTART, in UTC
/// or local time alike: close enough to pick an example).
fn next_event(
    events: impl Iterator<Item = String>,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<String> {
    let now = now.format("%Y%m%dT%H%M%S").to_string();
    let start = |ical: &str| {
        crate::ical::unfold(ical).lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            (crate::ical::property_name(key) == "DTSTART").then(|| value.trim().to_string())
        })
    };
    events
        .filter_map(|ical| Some((start(&ical)?, ical)))
        .filter(|(start, _)| start.as_str() >= now.as_str())
        .min_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, ical)| ical)
}

/// How many events `purge_sync` would delete from the sink of `sync_name`.
pub async fn count_synced_events(config: &Config, sync_name: &str) -> Result<usize> {
    let sync = find_sync(config, sync_name)?;
    match config.calendars[&sync.target].eventkit_calendar.clone() {
        Some(ek_name) => eventkit_count(ek_name, sync.name.clone(), sync.source.clone()).await,
        None => {
            let client = caldav_client(config, &sync.target)?;
            Ok(marked_caldav_events(&client, sync_name, &sync.source)
                .await?
                .len())
        }
    }
}

/// Delete the events `sync_name` created from its sink, then forget its
/// state. CalDAV events are found by the sync marker of exactly this sync,
/// Calendar.app events by their notes marker: the sync name, or for a legacy
/// marker the sync's source. Returns (deleted, errors); single failed
/// deletes are logged and counted, the state is removed anyway. `progress`
/// gets the number of events handled so far.
pub async fn purge_sync(
    config: &Config,
    sync_name: &str,
    progress: Progress,
) -> Result<(usize, usize)> {
    let sync = find_sync(config, sync_name)?;
    let store = FsStateStore::new(config.resolve_state_dir()?)?;
    let sink_cal = &config.calendars[&sync.target];
    info!(sync = sync_name, "purge started");
    let started = std::time::Instant::now();
    let (deleted, errors) = match sink_cal.eventkit_calendar.clone() {
        Some(ek_name) => {
            info!(
                sync = sync_name,
                target = %ek_name,
                "deleting synced events from Calendar.app"
            );
            eventkit_delete(ek_name, sync.name.clone(), sync.source.clone(), progress).await?
        }
        None => {
            let client = caldav_client(config, &sync.target)?;
            info!(
                sync = sync_name,
                target = sink_cal.url_or_err(&sync.target)?,
                "fetching events from target for purge"
            );
            let hrefs = marked_caldav_events(&client, sync_name, &sync.source).await?;
            if hrefs.is_empty() {
                info!(sync = sync_name, "no events of this sync in the target");
            } else {
                info!(
                    sync = sync_name,
                    count = hrefs.len(),
                    "deleting synced events from target"
                );
            }
            delete_concurrently(&client, hrefs, sink_cal.concurrency, progress).await
        }
    };
    let took = crate::sync::runner::took(started);
    if errors > 0 {
        warn!(sync = sync_name, %took, deleted, errors, "purge done with errors");
    } else {
        info!(sync = sync_name, %took, deleted, "purge done");
    }
    remove_states(&store, [sync_name]);
    Ok((deleted, errors))
}

/// How many events `purge_calendar` would delete: all in `calendar`.
pub async fn count_calendar_events(config: &Config, calendar: &str) -> Result<usize> {
    match find_calendar(config, calendar)?.eventkit_calendar.clone() {
        Some(ek_name) => eventkit_count_all(ek_name).await,
        None => Ok(all_caldav_events(config, calendar).await?.0.len()),
    }
}

/// Delete every event in `calendar`, also ones capycal did not create, then
/// forget the state of every sync writing to it. Returns (deleted, errors);
/// `progress` gets the number of events handled so far.
pub async fn purge_calendar(
    config: &Config,
    calendar: &str,
    progress: Progress,
) -> Result<(usize, usize)> {
    let cal = find_calendar(config, calendar)?;
    let store = FsStateStore::new(config.resolve_state_dir()?)?;
    info!(calendar, "purge-all started");
    let started = std::time::Instant::now();
    let (deleted, errors) = match cal.eventkit_calendar.clone() {
        Some(ek_name) => {
            info!(calendar, eventkit = %ek_name, "purging all events from Calendar.app");
            eventkit_delete_all(ek_name, progress).await?
        }
        None => {
            let (hrefs, client) = all_caldav_events(config, calendar).await?;
            if hrefs.is_empty() {
                info!(calendar, "no events in calendar");
            } else {
                info!(calendar, count = hrefs.len(), "deleting ALL events");
            }
            delete_concurrently(&client, hrefs, cal.concurrency, progress).await
        }
    };
    let took = crate::sync::runner::took(started);
    if errors > 0 {
        warn!(calendar, %took, deleted, errors, "purge-all done with errors");
    } else {
        info!(calendar, %took, deleted, "purge-all done");
    }
    let writing = config.syncs.iter().filter(|s| s.target == calendar);
    remove_states(&store, writing.map(|s| s.name.as_str()));
    Ok((deleted, errors))
}

fn find_calendar<'a>(config: &'a Config, calendar: &str) -> Result<&'a CalendarConfig> {
    let cal = config
        .calendars
        .get(calendar)
        .ok_or_else(|| Error::Config(format!("calendar '{calendar}' not found in config")))?;
    if cal.is_ics() {
        return Err(crate::sync::factory::read_only_feed(calendar));
    }
    Ok(cal)
}

/// The hrefs of every event in a CalDAV calendar, and its client.
async fn all_caldav_events(config: &Config, calendar: &str) -> Result<(Vec<String>, CalDavClient)> {
    let client = caldav_client(config, calendar)?;
    let refs = client.list_events().await.map_err(|e| Error::SinkEvents {
        action: "listing events",
        source: Box::new(e),
    })?;
    Ok((refs.into_iter().map(|r| r.href).collect(), client))
}

#[cfg(target_os = "macos")]
async fn eventkit_count_all(calendar: String) -> Result<usize> {
    tokio::task::spawn_blocking(move || crate::eventkit::count_all_events(&calendar))
        .await
        .map_err(|e| Error::EventKit(e.to_string()))?
}

#[cfg(target_os = "macos")]
async fn eventkit_delete_all(calendar: String, progress: Progress) -> Result<(usize, usize)> {
    tokio::task::spawn_blocking(move || crate::eventkit::delete_all_events(&calendar, &*progress))
        .await
        .map_err(|e| Error::EventKit(e.to_string()))?
}

#[cfg(not(target_os = "macos"))]
async fn eventkit_count_all(_: String) -> Result<usize> {
    Err(crate::sync::factory::eventkit_unsupported())
}

#[cfg(not(target_os = "macos"))]
async fn eventkit_delete_all(_: String, _: Progress) -> Result<(usize, usize)> {
    Err(crate::sync::factory::eventkit_unsupported())
}

fn find_sync<'a>(config: &'a Config, sync_name: &str) -> Result<&'a SyncConfig> {
    config
        .syncs
        .iter()
        .find(|s| s.name == sync_name)
        .ok_or_else(|| Error::Config(format!("sync '{sync_name}' not found in config")))
}

/// Hrefs of the sink events that carry the marker of exactly `sync_name`.
async fn marked_caldav_events(
    client: &CalDavClient,
    sync_name: &str,
    source_name: &str,
) -> Result<Vec<String>> {
    Ok(synced_events(client, sync_name, source_name)
        .await?
        .into_iter()
        .map(|e| e.href)
        .collect())
}

/// Count the Calendar.app events carrying the notes marker of `sync`.
#[cfg(target_os = "macos")]
async fn eventkit_count(calendar: String, sync: String, source: String) -> Result<usize> {
    tokio::task::spawn_blocking(move || {
        crate::eventkit::count_events_by_marker(&calendar, &sync, &source)
    })
    .await
    .map_err(|e| Error::EventKit(e.to_string()))?
}

/// Delete the Calendar.app events carrying the notes marker of `sync`.
#[cfg(target_os = "macos")]
async fn eventkit_delete(
    calendar: String,
    sync: String,
    source: String,
    progress: Progress,
) -> Result<(usize, usize)> {
    tokio::task::spawn_blocking(move || {
        crate::eventkit::delete_events_by_marker(&calendar, &sync, &source, &*progress)
    })
    .await
    .map_err(|e| Error::EventKit(e.to_string()))?
}

#[cfg(not(target_os = "macos"))]
async fn eventkit_count(_: String, _: String, _: String) -> Result<usize> {
    Err(crate::sync::factory::eventkit_unsupported())
}

#[cfg(not(target_os = "macos"))]
async fn eventkit_delete(_: String, _: String, _: String, _: Progress) -> Result<(usize, usize)> {
    Err(crate::sync::factory::eventkit_unsupported())
}

/// Delete events with at most `concurrency` requests in flight, reporting
/// each finished one to `progress`. Returns (deleted, errors).
pub(crate) async fn delete_concurrently(
    client: &CalDavClient,
    hrefs: Vec<String>,
    concurrency: usize,
    progress: Progress,
) -> (usize, usize) {
    let semaphore = Arc::new(tokio::sync::Semaphore::new(concurrency.max(1)));
    // Counted in the tasks: while they are spawned, none is awaited yet
    let handled = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut handles = Vec::new();
    for href in hrefs {
        let permit = semaphore.clone().acquire_owned().await.unwrap();
        let client = client.clone();
        let (handled, progress) = (handled.clone(), progress.clone());
        handles.push(tokio::spawn(async move {
            let result = client.delete_event(&href).await;
            drop(permit);
            progress(handled.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1);
            (href, result)
        }));
    }

    let (mut deleted, mut errors) = (0, 0);
    for handle in handles {
        match handle.await {
            Ok((href, Ok(()))) => {
                deleted += 1;
                debug!(href = %href, "deleted");
            }
            Ok((href, Err(e))) => {
                errors += 1;
                error!(href = %href, error = %e, "failed to delete");
            }
            Err(e) => {
                errors += 1;
                error!(error = %e, "delete task panicked");
            }
        }
    }
    (deleted, errors)
}

/// Forget the state of the given syncs, logging failures without aborting.
pub(crate) fn remove_states<'a>(
    store: &impl StateStore,
    sync_names: impl IntoIterator<Item = &'a str>,
) {
    for name in sync_names {
        match store.remove(name) {
            Ok(()) => info!(sync = name, "removed sync state"),
            Err(e) => error!(error = %e, sync = name, "failed to remove sync state"),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::caldav::auth::ResolvedAuth;
    use crate::cli::tests::{caldav_config, state_dir};
    use wiremock::matchers::{body_string_contains, header, method, path, path_regex};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn test_next_event_is_the_first_one_ahead() {
        let event = |start: &str, title: &str| {
            format!(
                "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:{title}\r\n{start}\r\nSUMMARY:{title}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
            )
        };
        let now = chrono::DateTime::parse_from_rfc3339("2026-10-17T12:00:00Z")
            .unwrap()
            .to_utc();
        let events = [
            event("DTSTART:20261016T090000Z", "past"),
            event("DTSTART;TZID=Europe/Berlin:20261020T090000", "later"),
            event("DTSTART;VALUE=DATE:20261018", "next"),
        ];
        let next = next_event(events.clone().into_iter(), now).unwrap();
        assert!(next.contains("SUMMARY:next"), "{next}");
        assert_eq!(next_event(events[..1].iter().cloned(), now), None);
    }

    fn multistatus(inner: &str) -> ResponseTemplate {
        ResponseTemplate::new(207).set_body_string(format!(
            r#"<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">{inner}</d:multistatus>"#
        ))
    }

    fn event_ref(href: &str) -> String {
        format!(
            "<d:response><d:href>{href}</d:href><d:propstat><d:prop>\
             <d:getetag>\"e\"</d:getetag></d:prop></d:propstat></d:response>"
        )
    }

    /// Multiget response entry for `href` with `extra` property lines.
    pub(crate) fn event(href: &str, extra: &str) -> String {
        format!(
            "<d:response><d:href>{href}</d:href><d:propstat><d:prop><d:getetag>\"e\"</d:getetag>\
             <c:calendar-data>BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:x\n{extra}END:VEVENT\nEND:VCALENDAR\
             </c:calendar-data></d:prop></d:propstat></d:response>"
        )
    }

    /// Serve `hrefs` from the `home` calendar listing, with `events` as their multiget data.
    pub(crate) async fn mount_home(server: &MockServer, hrefs: &[&str], events: &[String]) {
        Mock::given(method("PROPFIND"))
            .and(path_regex("^/home/?$"))
            .and(header("Depth", "1"))
            .respond_with(multistatus(
                &hrefs.iter().map(|h| event_ref(h)).collect::<String>(),
            ))
            .mount(server)
            .await;
        Mock::given(method("REPORT"))
            .and(body_string_contains("calendar-multiget"))
            .respond_with(multistatus(&events.concat()))
            .mount(server)
            .await;
    }

    pub(crate) async fn expect_deletes(server: &MockServer, hrefs: &[&str], times: u64) {
        for href in hrefs {
            Mock::given(method("DELETE"))
                .and(path(*href))
                .respond_with(ResponseTemplate::new(204))
                .expect(times)
                .mount(server)
                .await;
        }
    }

    /// Sink `home` with events of this sync, of another sync, of a sync
    /// whose name starts with this one, and one not created by capycal.
    async fn mount_mixed_home(server: &MockServer) {
        mount_home(
            server,
            &[
                "/home/a.ics",
                "/home/b.ics",
                "/home/c.ics",
                "/home/d.ics",
                "/home/e.ics",
            ],
            &[
                event("/home/a.ics", "X-CAPYCAL-SYNC:work-to-home\n"),
                event("/home/b.ics", ""),
                event("/home/c.ics", "X-CAPYCAL-SYNC:home-to-work\n"),
                event("/home/d.ics", "X-CAPYCAL-SYNC:work-to-home-old\n"),
                event("/home/e.ics", "X-CAPYCAL-SYNC:work-to-home\n"),
            ],
        )
        .await;
    }

    #[tokio::test]
    async fn test_count_synced_events_counts_exact_marker() {
        let server = MockServer::start().await;
        mount_mixed_home(&server).await;
        let dir = state_dir("maint-count");
        let cfg = caldav_config(&server.uri(), &dir);

        assert_eq!(count_synced_events(&cfg, "work-to-home").await.unwrap(), 2);
        // Counting deletes nothing
        assert!(
            server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .all(|r| r.method.as_str() != "DELETE")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_purge_sync_deletes_exact_marker_and_forgets_state() {
        let server = MockServer::start().await;
        mount_mixed_home(&server).await;
        expect_deletes(&server, &["/home/a.ics", "/home/e.ics"], 1).await;
        expect_deletes(&server, &["/home/b.ics", "/home/c.ics", "/home/d.ics"], 0).await;
        let dir = state_dir("maint-purge");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("work-to-home.json"), "{}").unwrap();
        std::fs::write(dir.join("home-to-work.json"), "{}").unwrap();
        let cfg = caldav_config(&server.uri(), &dir);

        assert_eq!(
            purge_sync(&cfg, "work-to-home", no_progress())
                .await
                .unwrap(),
            (2, 0)
        );
        server.verify().await;
        assert!(!dir.join("work-to-home.json").exists());
        assert!(dir.join("home-to-work.json").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_purge_calendar_deletes_every_event_and_forgets_its_syncs() {
        let server = MockServer::start().await;
        mount_mixed_home(&server).await;
        let all = [
            "/home/a.ics",
            "/home/b.ics",
            "/home/c.ics",
            "/home/d.ics",
            "/home/e.ics",
        ];
        expect_deletes(&server, &all, 1).await;
        let dir = state_dir("maint-purge-calendar");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("work-to-home.json"), "{}").unwrap();
        std::fs::write(dir.join("home-to-work.json"), "{}").unwrap();
        let cfg = caldav_config(&server.uri(), &dir);

        assert_eq!(count_calendar_events(&cfg, "home").await.unwrap(), 5);
        assert_eq!(
            purge_calendar(&cfg, "home", no_progress()).await.unwrap(),
            (5, 0)
        );
        server.verify().await;
        // Only the sync writing into home
        assert!(!dir.join("work-to-home.json").exists());
        assert!(dir.join("home-to-work.json").exists());
        let err = count_calendar_events(&cfg, "nope").await.unwrap_err();
        assert!(
            err.to_string().contains("calendar 'nope' not found"),
            "{err}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_count_and_purge_report_unknown_sync_and_listing_error() {
        let server = MockServer::start().await;
        let dir = state_dir("maint-errors");
        let cfg = caldav_config(&server.uri(), &dir);

        let err = count_synced_events(&cfg, "nope").await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "configuration error: sync 'nope' not found in config"
        );
        let err = count_synced_events(&cfg, "work-to-home").await.unwrap_err();
        assert!(
            err.to_string().starts_with("error listing target events: "),
            "{err}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(not(target_os = "macos"))]
    #[tokio::test]
    async fn test_eventkit_sink_unsupported_off_macos() {
        let dir = state_dir("maint-ek");
        let mut cfg = caldav_config("http://localhost", &dir);
        cfg.calendars.get_mut("home").unwrap().eventkit_calendar = Some("Home".to_string());

        let err = count_synced_events(&cfg, "work-to-home").await.unwrap_err();
        assert!(err.to_string().contains("only supported on macOS"), "{err}");
        assert!(
            purge_sync(&cfg, "work-to-home", no_progress())
                .await
                .is_err()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_delete_concurrently_counts_results() {
        let server = MockServer::start().await;
        for (p, status) in [
            ("/cal/a.ics", 204),
            ("/cal/b.ics", 404),
            ("/cal/c.ics", 403),
        ] {
            Mock::given(method("DELETE"))
                .and(path(p))
                .respond_with(ResponseTemplate::new(status))
                .expect(1)
                .mount(&server)
                .await;
        }
        let auth = ResolvedAuth {
            username: "u".to_string(),
            password: "p".to_string(),
        };
        let client = CalDavClient::new(&format!("{}/cal/", server.uri()), auth).unwrap();
        let hrefs = ["/cal/a.ics", "/cal/b.ics", "/cal/c.ics"].map(String::from);

        let reported = Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = reported.clone();
        let progress: Progress = Arc::new(move |done| log.lock().unwrap().push(done));
        let (deleted, errors) = delete_concurrently(&client, hrefs.to_vec(), 2, progress).await;
        assert_eq!((deleted, errors), (2, 1));
        // The tasks run concurrently and may report out of order
        let mut reported = reported.lock().unwrap().clone();
        reported.sort();
        assert_eq!(reported, [1, 2, 3]);
    }

    #[test]
    fn test_forget_sync_state_removes_only_that_state() {
        let dir = std::env::temp_dir().join(format!("capycal-{}-forget", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let config: Config =
            toml::from_str(&format!("[daemon]\nstate_dir = \"{}\"\n", dir.display())).unwrap();
        let store = FsStateStore::new(dir.clone()).unwrap();
        store
            .save("a", &crate::state::SyncState::default())
            .unwrap();
        store
            .save("b", &crate::state::SyncState::default())
            .unwrap();
        forget_sync_state(&config, "a").unwrap();
        forget_sync_state(&config, "a").unwrap();
        assert!(store.load("a").unwrap().is_none());
        assert!(store.load("b").unwrap().is_some());
    }
}
