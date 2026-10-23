//! End-to-end tests of the `capycal` binary against a real CalDAV server.
//!
//! Ignored by default; `make test-caldav` starts a throwaway Radicale server
//! and runs them. They need `CAPYCAL_TEST_CALDAV_URL` (a URL under which
//! calendars may be created, e.g. `http://127.0.0.1:5232/test/`) and accept
//! `CAPYCAL_TEST_CALDAV_USER` / `CAPYCAL_TEST_CALDAV_PASSWORD` (default
//! `test` / `test`). Every calendar they create is named `capycal-test-...`
//! and deleted at the end.

use std::path::PathBuf;
use std::process::Command;

use regex::Regex;
use reqwest::{Client, Method};

const PREFIX: &str = "capycal-test-";

struct Server {
    client: Client,
    base: String,
    user: String,
    password: String,
}

impl Server {
    fn from_env() -> Self {
        let mut base = std::env::var("CAPYCAL_TEST_CALDAV_URL")
            .expect("CAPYCAL_TEST_CALDAV_URL is not set; run `make test-caldav`");
        if !base.ends_with('/') {
            base.push('/');
        }
        Self {
            client: Client::new(),
            base,
            user: std::env::var("CAPYCAL_TEST_CALDAV_USER").unwrap_or_else(|_| "test".into()),
            password: std::env::var("CAPYCAL_TEST_CALDAV_PASSWORD")
                .unwrap_or_else(|_| "test".into()),
        }
    }

    fn url(&self, calendar: &str) -> String {
        format!("{}{calendar}/", self.base)
    }

    async fn request(&self, method: &str, url: &str) -> reqwest::RequestBuilder {
        self.client
            .request(Method::from_bytes(method.as_bytes()).unwrap(), url)
            .basic_auth(&self.user, Some(&self.password))
    }

    async fn create_calendar(&self, calendar: &str) {
        assert!(calendar.starts_with(PREFIX));
        let response = self
            .request("MKCALENDAR", &self.url(calendar))
            .await
            .send()
            .await
            .unwrap();
        assert!(
            response.status().is_success(),
            "MKCALENDAR {calendar}: {response:?}"
        );
    }

    async fn delete_calendar(&self, calendar: &str) {
        assert!(calendar.starts_with(PREFIX));
        let _ = self
            .request("DELETE", &self.url(calendar))
            .await
            .send()
            .await;
    }

    async fn put(&self, calendar: &str, name: &str, ics: &str) {
        let response = self
            .request("PUT", &format!("{}{name}.ics", self.url(calendar)))
            .await
            .header("Content-Type", "text/calendar; charset=utf-8")
            .body(ics.replace('\n', "\r\n"))
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success(), "PUT {name}: {response:?}");
    }

    async fn delete(&self, calendar: &str, name: &str) {
        let response = self
            .request("DELETE", &format!("{}{name}.ics", self.url(calendar)))
            .await
            .send()
            .await
            .unwrap();
        assert!(
            response.status().is_success(),
            "DELETE {name}: {response:?}"
        );
    }

    /// The iCal of every event in `calendar`, unfolded.
    async fn events(&self, calendar: &str) -> Vec<String> {
        let body = self
            .request("PROPFIND", &self.url(calendar))
            .await
            .header("Depth", "1")
            .body(
                r#"<?xml version="1.0"?><propfind xmlns="DAV:"><prop><getetag/></prop></propfind>"#,
            )
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        let hrefs = Regex::new(r"<(?:\w+:)?href>([^<]+\.ics)</(?:\w+:)?href>").unwrap();
        let mut events = Vec::new();
        for href in hrefs.captures_iter(&body) {
            let url = reqwest::Url::parse(&self.base)
                .unwrap()
                .join(&href[1])
                .unwrap();
            let ics = self
                .request("GET", url.as_str())
                .await
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap();
            events.push(ics.replace("\r\n ", "").replace("\r\n", "\n"));
        }
        events
    }
}

/// Two calendars and a config syncing the first into the second.
struct Setup {
    server: Server,
    source: String,
    sink: String,
    dir: PathBuf,
}

impl Setup {
    async fn new(test: &str, sync_options: &str) -> Self {
        let server = Server::from_env();
        let id = format!("{PREFIX}{}-{test}", std::process::id());
        let (source, sink) = (format!("{id}-source"), format!("{id}-sink"));
        server.delete_calendar(&source).await;
        server.delete_calendar(&sink).await;
        server.create_calendar(&source).await;
        server.create_calendar(&sink).await;
        let dir = std::env::temp_dir().join(&id);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let calendar =
            |name: &str| format!("account = \"radicale\"\nurl = \"{}\"\n", server.url(name));
        std::fs::write(
            dir.join("capycal.toml"),
            format!(
                "[daemon]\nstate_dir = \"{}\"\n\n\
                 [accounts.radicale]\nurl = \"{}\"\nusername = \"{}\"\npassword = \"{}\"\n\n\
                 [calendars.source]\n{}\n[calendars.sink]\n{}\n\
                 [syncs.test]\nsource = \"source\"\ntarget = \"sink\"\ninterval = \"1h\"\n{sync_options}\n",
                dir.join("state").display(),
                server.base,
                server.user,
                server.password,
                calendar(&source),
                calendar(&sink),
            ),
        )
        .unwrap();
        Self {
            server,
            source,
            sink,
            dir,
        }
    }

    fn capycal_output(&self, args: &[&str]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_capycal"))
            .arg("--config")
            .arg(self.dir.join("capycal.toml"))
            .args(args)
            .output()
            .unwrap()
    }

    fn capycal(&self, args: &[&str]) {
        let output = self.capycal_output(args);
        assert!(
            output.status.success(),
            "capycal {args:?} failed:\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn sync(&self) {
        self.capycal(&["--once"]);
    }

    /// Replace `from` with `to` in the config file.
    fn edit_config(&self, from: &str, to: &str) {
        let path = self.dir.join("capycal.toml");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains(from), "{from} not in config");
        std::fs::write(&path, text.replace(from, to)).unwrap();
    }

    fn status(&self) -> String {
        String::from_utf8(self.capycal_output(&["--status"]).stdout).unwrap()
    }

    async fn put(&self, name: &str, ics: &str) {
        self.server.put(&self.source, name, ics).await;
    }

    async fn delete(&self, name: &str) {
        self.server.delete(&self.source, name).await;
    }

    async fn sink_events(&self) -> Vec<String> {
        self.server.events(&self.sink).await
    }

    /// The one sink event copied from the source event `uid`.
    async fn copy_of(&self, uid: &str) -> String {
        let marker = format!("X-CAPYCAL-SOURCE:source/{uid}\n");
        let copies: Vec<String> = self
            .sink_events()
            .await
            .into_iter()
            .filter(|e| e.contains(&marker))
            .collect();
        assert_eq!(copies.len(), 1, "copies of {uid}: {copies:#?}");
        copies.into_iter().next().unwrap()
    }

    async fn cleanup(self) {
        self.server.delete_calendar(&self.source).await;
        self.server.delete_calendar(&self.sink).await;
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn event(uid: &str, extra: &str) -> String {
    format!(
        "BEGIN:VCALENDAR\nVERSION:2.0\nPRODID:-//capycal test//EN\nBEGIN:VEVENT\nUID:{uid}\n\
         DTSTAMP:20240101T000000Z\nDTSTART:20300107T090000Z\nDTEND:20300107T100000Z\n\
         SUMMARY:Secret {uid}\nLOCATION:Room 1\n{extra}END:VEVENT\nEND:VCALENDAR\n"
    )
}

const SERIES: &str = "BEGIN:VCALENDAR
VERSION:2.0
PRODID:-//capycal test//EN
BEGIN:VTIMEZONE
TZID:Europe/Berlin
BEGIN:STANDARD
DTSTART:19701025T030000
RRULE:FREQ=YEARLY;BYMONTH=10;BYDAY=-1SU
TZOFFSETFROM:+0200
TZOFFSETTO:+0100
END:STANDARD
BEGIN:DAYLIGHT
DTSTART:19700329T020000
RRULE:FREQ=YEARLY;BYMONTH=3;BYDAY=-1SU
TZOFFSETFROM:+0100
TZOFFSETTO:+0200
END:DAYLIGHT
END:VTIMEZONE
BEGIN:VEVENT
UID:series
DTSTAMP:20240101T000000Z
DTSTART;TZID=Europe/Berlin:20300107T090000
DTEND;TZID=Europe/Berlin:20300107T100000
RRULE:FREQ=WEEKLY;COUNT=6
EXDATE;TZID=Europe/Berlin:20300121T090000
RDATE;TZID=Europe/Berlin:20300301T090000
SUMMARY:Jour fixe
END:VEVENT
BEGIN:VEVENT
UID:series
DTSTAMP:20240101T000000Z
RECURRENCE-ID;TZID=Europe/Berlin:20300114T090000
DTSTART;TZID=Europe/Berlin:20300114T110000
DTEND;TZID=Europe/Berlin:20300114T120000
SUMMARY:Jour fixe (moved)
END:VEVENT
BEGIN:VEVENT
UID:series
DTSTAMP:20240101T000000Z
RECURRENCE-ID;TZID=Europe/Berlin:20300128T090000
DTSTART;TZID=Europe/Berlin:20300128T090000
DTEND;TZID=Europe/Berlin:20300128T100000
STATUS:CANCELLED
END:VEVENT
END:VCALENDAR
";

fn assert_contains(text: &str, expected: &[&str]) {
    for line in expected {
        assert!(text.contains(line), "missing {line:?} in:\n{text}");
    }
}

#[tokio::test]
#[ignore = "needs a CalDAV server, run `make test-caldav`"]
async fn test_busy_blocker_keeps_structure_and_drops_content() {
    let setup = Setup::new("blocker", "").await;
    setup
        .put(
            "free",
            &event("free", "TRANSP:TRANSPARENT\nSTATUS:TENTATIVE\n"),
        )
        .await;
    setup.sync();
    let copy = setup.copy_of("free").await;
    assert_contains(
        &copy,
        &[
            "DTSTART:20300107T090000Z",
            "DTEND:20300107T100000Z",
            "TRANSP:TRANSPARENT",
            "STATUS:TENTATIVE",
            "X-CAPYCAL-SYNC:test",
        ],
    );
    assert!(
        !copy.contains("Secret") && !copy.contains("Room 1"),
        "{copy}"
    );
    setup.cleanup().await;
}

#[tokio::test]
#[ignore = "needs a CalDAV server, run `make test-caldav`"]
async fn test_changes_update_the_same_copy() {
    let setup = Setup::new("update", "fields = [\"SUMMARY\"]").await;
    setup.put("e", &event("e", "")).await;
    setup.sync();
    assert_contains(&setup.copy_of("e").await, &["SUMMARY:Secret e"]);
    setup
        .put("e", &event("e", "").replace("Secret e", "Renamed"))
        .await;
    setup.sync();
    assert_contains(&setup.copy_of("e").await, &["SUMMARY:Renamed"]);
    assert_eq!(setup.sink_events().await.len(), 1);
    setup.cleanup().await;
}

#[tokio::test]
#[ignore = "needs a CalDAV server, run `make test-caldav`"]
async fn test_series_with_exceptions_is_copied_whole() {
    let setup = Setup::new("series", "fields = [\"SUMMARY\"]").await;
    setup.put("series", SERIES).await;
    setup.sync();
    let copy = setup.copy_of("series").await;
    assert_contains(
        &copy,
        &[
            "RRULE:FREQ=WEEKLY;COUNT=6",
            "EXDATE;TZID=Europe/Berlin:20300121T090000",
            "RDATE;TZID=Europe/Berlin:20300301T090000",
            "RECURRENCE-ID;TZID=Europe/Berlin:20300114T090000",
            "DTSTART;TZID=Europe/Berlin:20300114T110000",
            "SUMMARY:Jour fixe (moved)",
            "RECURRENCE-ID;TZID=Europe/Berlin:20300128T090000",
            "STATUS:CANCELLED",
            "BEGIN:VTIMEZONE",
        ],
    );
    assert_eq!(copy.matches("BEGIN:VEVENT").count(), 3, "{copy}");
    setup.cleanup().await;
}

#[tokio::test]
#[ignore = "needs a CalDAV server, run `make test-caldav`"]
async fn test_buffer_moves_the_dates_of_a_series() {
    let setup = Setup::new(
        "buffer",
        "transforms = [ { action = \"add_buffer\", pre_event = \"30m\", post_event = \"15m\" } ]",
    )
    .await;
    setup.put("series", SERIES).await;
    setup.sync();
    assert_contains(
        &setup.copy_of("series").await,
        &[
            "DTSTART;TZID=Europe/Berlin:20300107T083000",
            "DTEND;TZID=Europe/Berlin:20300107T101500",
            "EXDATE;TZID=Europe/Berlin:20300121T083000",
            "RDATE;TZID=Europe/Berlin:20300301T083000",
            "RECURRENCE-ID;TZID=Europe/Berlin:20300114T083000",
            "DTSTART;TZID=Europe/Berlin:20300114T103000",
            "RECURRENCE-ID;TZID=Europe/Berlin:20300128T083000",
        ],
    );
    setup.cleanup().await;
}

#[tokio::test]
#[ignore = "needs a CalDAV server, run `make test-caldav`"]
async fn test_tasks_are_not_copied() {
    let setup = Setup::new("tasks", "").await;
    setup.put("e", &event("e", "")).await;
    setup
        .put(
            "t",
            "BEGIN:VCALENDAR\nVERSION:2.0\nPRODID:-//capycal test//EN\nBEGIN:VTODO\nUID:t\n\
             DTSTAMP:20240101T000000Z\nSUMMARY:Secret task\nEND:VTODO\nEND:VCALENDAR\n",
        )
        .await;
    setup.sync();
    let events = setup.sink_events().await;
    assert_eq!(events.len(), 1, "{events:#?}");
    setup.copy_of("e").await;
    setup.cleanup().await;
}

#[tokio::test]
#[ignore = "needs a CalDAV server, run `make test-caldav`"]
async fn test_losing_most_source_events_removes_them_one_cycle_later() {
    let setup = Setup::new("mass", "").await;
    let names = ["a", "b", "c", "d", "e", "f"];
    for name in names {
        setup.put(name, &event(name, "")).await;
    }
    setup.sync();
    for name in &names[..4] {
        setup.delete(name).await;
    }
    // Each --once is a new process: the wait is kept in the state
    let output = setup.capycal_output(&["--once"]);
    let log = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{log}");
    assert!(log.contains("removing them in the next cycle"), "{log}");
    assert_eq!(setup.sink_events().await.len(), 6);
    setup.sync();
    assert_eq!(setup.sink_events().await.len(), 2);
    setup.cleanup().await;
}

#[tokio::test]
#[ignore = "needs a CalDAV server, run `make test-caldav`"]
async fn test_cancelled_event_is_removed() {
    let setup = Setup::new("cancel", "").await;
    setup.put("a", &event("a", "")).await;
    setup.put("b", &event("b", "")).await;
    setup.sync();
    assert_eq!(setup.sink_events().await.len(), 2);
    setup.put("a", &event("a", "STATUS:CANCELLED\n")).await;
    setup.sync();
    let events = setup.sink_events().await;
    assert_eq!(events.len(), 1, "{events:#?}");
    setup.copy_of("b").await;
    setup.cleanup().await;
}

#[tokio::test]
#[ignore = "needs a CalDAV server, run `make test-caldav`"]
async fn test_skip_by_regex_drops_matching_events() {
    let setup = Setup::new(
        "skip-regex",
        "transforms = [{ action = \"skip\", when = [{ field = \"SUMMARY\", matches = \"(?i)secret b\" }] }]",
    )
    .await;
    setup.put("a", &event("a", "")).await;
    setup.put("b", &event("b", "")).await;
    setup.sync();
    let events = setup.sink_events().await;
    assert_eq!(events.len(), 1, "{events:#?}");
    // A blocker without SUMMARY, although the pattern looked at it
    assert!(!setup.copy_of("a").await.contains("Secret"));
    // Renaming the copied event so the pattern matches removes its copy
    setup
        .put("a", &event("a", "").replace("Secret a", "Secret b"))
        .await;
    setup.sync();
    assert!(setup.sink_events().await.is_empty());
    setup.cleanup().await;
}

#[tokio::test]
#[ignore = "needs a CalDAV server, run `make test-caldav`"]
async fn test_replace_and_skip_by_field() {
    let setup = Setup::new(
        "replace-skip",
        "fields = [\"SUMMARY\"]\ntransforms = [\n\
         { field = \"SUMMARY\", action = \"replace_field\", search = \"Secret\", replacement = \"Busy\" },\n\
         { action = \"skip\", when = [{ field = \"LOCATION\", contains = \"Room 2\" }] },\n]",
    )
    .await;
    setup.put("a", &event("a", "")).await;
    setup
        .put("b", &event("b", "").replace("Room 1", "Room 2"))
        .await;
    setup.sync();
    assert_eq!(setup.sink_events().await.len(), 1);
    let copy = setup.copy_of("a").await;
    assert_contains(&copy, &["SUMMARY:Busy a"]);
    assert!(!copy.contains("Room"), "{copy}");
    setup.cleanup().await;
}

#[tokio::test]
#[ignore = "needs a CalDAV server, run `make test-caldav`"]
async fn test_full_sync_rebuilds_lost_state_without_duplicates() {
    let setup = Setup::new("fullsync", "").await;
    setup.put("a", &event("a", "")).await;
    setup.put("series", SERIES).await;
    setup.sync();
    assert_eq!(setup.sink_events().await.len(), 2);
    std::fs::remove_dir_all(setup.dir.join("state")).unwrap();
    setup.sync();
    assert_eq!(setup.sink_events().await.len(), 2);
    setup.capycal(&["--full-sync", "--once"]);
    assert_eq!(setup.sink_events().await.len(), 2);
    setup.cleanup().await;
}

#[tokio::test]
#[ignore = "needs a CalDAV server, run `make test-caldav`"]
async fn test_purge_removes_only_synced_events() {
    let setup = Setup::new("purge", "").await;
    setup.put("a", &event("a", "")).await;
    setup.put("series", SERIES).await;
    setup.sync();
    setup
        .server
        .put(&setup.sink, "own", &event("own", ""))
        .await;
    assert_eq!(setup.sink_events().await.len(), 3);
    setup.capycal(&["--purge", "test"]);
    let events = setup.sink_events().await;
    assert_eq!(events.len(), 1, "{events:#?}");
    assert!(events[0].contains("UID:own"));
    setup.cleanup().await;
}

/// A second sync `back` from the sink into the source.
const BACK: &str = "\n[syncs.back]\nsource = \"sink\"\ntarget = \"source\"\ninterval = \"1h\"\n";

/// Where each event of `calendar` comes from, sorted: its UID for an own
/// event, the `X-CAPYCAL-SOURCE` value for a copy.
async fn origins(server: &Server, calendar: &str) -> Vec<String> {
    let source = Regex::new(r"(?m)^X-CAPYCAL-SOURCE:(.+)$").unwrap();
    let uid = Regex::new(r"(?m)^UID:(.+)$").unwrap();
    let mut origins: Vec<String> = server
        .events(calendar)
        .await
        .iter()
        .map(|e| {
            source
                .captures(e)
                .or_else(|| uid.captures(e))
                .map(|c| c[1].to_string())
                .unwrap()
        })
        .collect();
    origins.sort();
    origins
}

#[tokio::test]
#[ignore = "needs a CalDAV server, run `make test-caldav`"]
async fn test_two_way_sync_copies_each_side_once() {
    let setup = Setup::new("twoway", BACK).await;
    let (source, sink) = (&setup.source, &setup.sink);
    setup.put("a", &event("a", "")).await;
    setup.server.put(sink, "b", &event("b", "")).await;
    for _ in 0..3 {
        setup.sync();
    }
    assert_eq!(origins(&setup.server, source).await, ["a", "sink/b"]);
    assert_eq!(origins(&setup.server, sink).await, ["b", "source/a"]);

    setup
        .put("a", &event("a", "").replace("T090000Z", "T080000Z"))
        .await;
    setup.server.delete(sink, "b").await;
    for _ in 0..2 {
        setup.sync();
    }
    assert_eq!(origins(&setup.server, source).await, ["a"]);
    assert_eq!(origins(&setup.server, sink).await, ["source/a"]);
    assert_contains(&setup.copy_of("a").await, &["DTSTART:20300107T080000Z"]);

    setup.capycal(&["--full-sync", "--once"]);
    assert_eq!(origins(&setup.server, source).await, ["a"]);
    assert_eq!(origins(&setup.server, sink).await, ["source/a"]);
    setup.cleanup().await;
}

#[tokio::test]
#[ignore = "needs a CalDAV server, run `make test-caldav`"]
async fn test_purge_of_one_direction_keeps_the_other() {
    // `other` writes into the same target as `test`
    let other = "\n[syncs.other]\nsource = \"source\"\ntarget = \"sink\"\ninterval = \"1h\"\n";
    let setup = Setup::new("twowaypurge", &format!("{BACK}{other}")).await;
    let (source, sink) = (&setup.source, &setup.sink);
    setup.put("a", &event("a", "")).await;
    setup.server.put(sink, "b", &event("b", "")).await;
    setup.sync();
    assert_eq!(
        origins(&setup.server, sink).await,
        ["b", "source/a", "source/a"]
    );

    setup.capycal(&["--purge", "test"]);
    let sink_events = setup.server.events(sink).await;
    assert_eq!(sink_events.len(), 2, "{sink_events:#?}");
    assert!(sink_events.iter().any(|e| e.contains("UID:b\n")));
    assert!(
        sink_events
            .iter()
            .any(|e| e.contains("X-CAPYCAL-SYNC:other\n"))
    );
    assert_eq!(origins(&setup.server, source).await, ["a", "sink/b"]);
    let state = setup.dir.join("state");
    assert!(!state.join("test.json").exists());
    assert!(state.join("back.json").exists());
    assert!(state.join("other.json").exists());
    setup.cleanup().await;
}

#[tokio::test]
#[ignore = "needs a CalDAV server, run `make test-caldav`"]
async fn test_full_sync_runs_once_a_day() {
    let setup = Setup::new("daily", "").await;
    let full_sync = |output: std::process::Output| {
        let log = String::from_utf8_lossy(&output.stdout).to_string();
        assert!(output.status.success(), "{log}");
        log.contains("full sync started")
    };
    assert!(full_sync(setup.capycal_output(&["--once"])));
    assert!(!full_sync(setup.capycal_output(&["--once"])));

    // The last full sync was yesterday
    let path = setup.dir.join("state").join("test.json");
    let mut state: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let yesterday = chrono::Utc::now() - chrono::TimeDelta::days(1);
    state["last_full_sync"] = serde_json::json!(yesterday.to_rfc3339());
    std::fs::write(&path, state.to_string()).unwrap();
    let output = setup.capycal_output(&["--once"]);
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("\"daily\""),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(full_sync(output));
    assert!(!full_sync(setup.capycal_output(&["--once"])));
    setup.cleanup().await;
}

#[tokio::test]
#[ignore]
async fn test_disabled_sync_is_skipped_and_full_syncs_when_enabled() {
    let setup = Setup::new("disabled", "enabled = true").await;
    setup.sync();
    setup.put("e1.ics", &event("e1", "")).await;
    setup.edit_config("enabled = true", "enabled = false");
    setup.sync();
    assert!(setup.sink_events().await.is_empty());
    let status = setup.status();
    assert!(status.contains("disabled"), "{status}");
    assert!(status.contains("full sync pending"), "{status}");

    setup.edit_config("enabled = false", "enabled = true");
    setup.sync();
    setup.copy_of("e1").await;
    assert!(!setup.status().contains("full sync pending"));
    setup.cleanup().await;
}

#[tokio::test]
#[ignore]
async fn test_changed_transform_rewrites_existing_copies() {
    let setup = Setup::new(
        "transform-change",
        "fields = [\"SUMMARY\"]\ntransforms = [{ field = \"SUMMARY\", action = \"set_field\", value = \"Busy\" }]",
    )
    .await;
    setup.put("e1.ics", &event("e1", "")).await;
    setup.sync();
    assert!(setup.copy_of("e1").await.contains("SUMMARY:Busy"));

    setup.edit_config("value = \"Busy\"", "value = \"Blocked\"");
    setup.sync();
    let copy = setup.copy_of("e1").await;
    assert!(copy.contains("SUMMARY:Blocked"), "{copy}");
    setup.cleanup().await;
}

#[tokio::test]
#[ignore = "needs a CalDAV server, run `make test-caldav`"]
async fn test_second_device_purge_is_repaired_by_a_full_sync() {
    let setup = Setup::new("two-devices", "").await;
    setup.put("a", &event("a", "")).await;
    setup.put("series", SERIES).await;
    setup.sync();
    setup.sync();
    assert_eq!(setup.sink_events().await.len(), 2);

    // The same config with its own state, like a second computer
    let state = setup.dir.join("state").display().to_string();
    let config = std::fs::read_to_string(setup.dir.join("capycal.toml")).unwrap();
    let other = setup.dir.join("other.toml");
    std::fs::write(&other, config.replace(&state, &format!("{state}-other"))).unwrap();
    let on_other = |args: &[&str]| {
        let output = Command::new(env!("CARGO_BIN_EXE_capycal"))
            .arg("--config")
            .arg(&other)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    };
    on_other(&["--once"]);
    assert_eq!(setup.sink_events().await.len(), 2, "no duplicates");
    on_other(&["--purge", "test"]);
    assert!(setup.sink_events().await.is_empty());

    // The source did not change, only the target
    setup.sync();
    assert_eq!(setup.sink_events().await.len(), 2);
    setup.copy_of("a").await;
    setup.cleanup().await;
}

#[tokio::test]
#[ignore = "needs a CalDAV server, run `make test-caldav`"]
async fn test_working_hours_window_wraps_and_keeps_all_day_events() {
    let setup = Setup::new(
        "working-hours",
        "transforms = [{ action = \"skip\", when = [{ days = [\"mon\", \"tue\", \"wed\", \"thu\", \"fri\"], start_time = \"18:00\", end_time = \"07:59\" }] }]",
    )
    .await;
    // Monday, 7 January 2030, in floating local time
    let at = |uid: &str, start: &str| {
        event(uid, "")
            .replace(
                "DTSTART:20300107T090000Z",
                &format!("DTSTART:20300107T{start}"),
            )
            .replace("DTEND:20300107T100000Z", "DURATION:PT1H")
    };
    setup.put("early", &at("early", "060000")).await;
    setup.put("noon", &at("noon", "120000")).await;
    setup.put("evening", &at("evening", "190000")).await;
    let all_day = event("allday", "")
        .replace("DTSTART:20300107T090000Z", "DTSTART;VALUE=DATE:20300107")
        .replace("DTEND:20300107T100000Z", "DTEND;VALUE=DATE:20300108");
    setup.put("allday", &all_day).await;
    setup.sync();
    assert_eq!(setup.sink_events().await.len(), 2);
    setup.copy_of("noon").await;
    setup.copy_of("allday").await;
    setup.cleanup().await;
}

#[tokio::test]
#[ignore = "needs a CalDAV server, run `make test-caldav`"]
async fn test_feed_source_follows_changes_and_removals() {
    let setup = Setup::new("feed", "fields = [\"SUMMARY\"]").await;
    // Radicale answers a GET of the collection with the whole calendar,
    // like a calendar's secret iCal address
    let url = setup.server.url(&setup.source);
    let feed = url.replacen(
        "://",
        &format!("://{}:{}@", setup.server.user, setup.server.password),
        1,
    );
    setup.edit_config(
        &format!("[calendars.source]\naccount = \"radicale\"\nurl = \"{url}\"\n"),
        &format!("[calendars.source]\nics_url = \"{feed}\"\n"),
    );
    setup.put("a", &event("a", "")).await;
    setup.put("series", SERIES).await;
    setup.sync();
    assert_eq!(setup.sink_events().await.len(), 2);
    assert_contains(&setup.copy_of("a").await, &["SUMMARY:Secret a"]);
    let series = setup.copy_of("series").await;
    assert_contains(
        &series,
        &[
            "TZID:Europe/Berlin",
            "SUMMARY:Jour fixe (moved)",
            "RECURRENCE-ID;TZID=Europe/Berlin:20300128T090000",
        ],
    );

    setup
        .put("a", &event("a", "").replace("Secret a", "Renamed a"))
        .await;
    setup.delete("series").await;
    setup.sync();
    assert_eq!(setup.sink_events().await.len(), 1);
    assert_contains(&setup.copy_of("a").await, &["SUMMARY:Renamed a"]);

    // A full sync finds the copy unchanged and keeps it
    setup.capycal(&["--once", "--full-sync"]);
    assert_eq!(setup.sink_events().await.len(), 1);
    setup.cleanup().await;
}

#[tokio::test]
#[ignore]
async fn test_feed_with_new_uids_on_each_request_keeps_its_copies() {
    let setup = Setup::new("feed-uids", "fields = [\"SUMMARY\"]").await;
    let url = setup.server.url(&setup.source);
    let feed = url.replacen(
        "://",
        &format!("://{}:{}@", setup.server.user, setup.server.password),
        1,
    );
    setup.edit_config(
        &format!("[calendars.source]\naccount = \"radicale\"\nurl = \"{url}\"\n"),
        &format!("[calendars.source]\nics_url = \"{feed}\"\n"),
    );
    // A feed generated on each request: same events, new random UIDs
    let pickup = |uid: &str, day: usize| {
        event(uid, "")
            .replace(&format!("Secret {uid}"), "Paper")
            .replace("20300107T", &format!("203001{:02}T", day + 10))
    };
    for day in 0..5 {
        setup
            .put(&format!("a{day}"), &pickup(&format!("a{day}"), day))
            .await;
    }
    setup.sync();
    let before = setup.sink_events().await;
    assert_eq!(before.len(), 5);

    for day in 0..5 {
        setup.delete(&format!("a{day}")).await;
        setup
            .put(&format!("b{day}"), &pickup(&format!("b{day}"), day))
            .await;
    }
    setup.sync();
    let mut after = setup.sink_events().await;
    let mut before = before;
    before.sort();
    after.sort();
    assert_eq!(after, before, "the copies stay as they were");
    for day in 0..5 {
        setup.copy_of(&format!("a{day}")).await;
    }
    setup.cleanup().await;
}

#[tokio::test]
#[ignore = "needs a CalDAV server, run `make test-caldav`"]
async fn test_added_skip_removes_existing_copies() {
    let setup = Setup::new("added-skip", "fields = [\"SUMMARY\"]").await;
    setup.put("e1.ics", &event("e1", "")).await;
    setup.put("e2.ics", &event("e2", "")).await;
    setup.sync();
    assert_eq!(setup.sink_events().await.len(), 2);

    setup.edit_config(
        "fields = [\"SUMMARY\"]",
        "fields = [\"SUMMARY\"]\ntransforms = [{ action = \"skip\", when = [{ field = \"UID\", contains = \"e1\" }] }]",
    );
    setup.sync();
    let events = setup.sink_events().await;
    assert_eq!(events.len(), 1, "{events:?}");
    setup.cleanup().await;
}

#[tokio::test]
#[ignore = "needs a CalDAV server, run `make test-caldav`"]
async fn test_skip_empty_drops_events_without_the_property() {
    let setup = Setup::new(
        "skip-missing",
        "transforms = [{ action = \"skip\", when = [{ field = \"LOCATION\", empty = true }] }]",
    )
    .await;
    setup.put("e1.ics", &event("e1", "")).await;
    let without = event("e2", "").replace("LOCATION:Room 1\n", "");
    setup.put("e2.ics", &without).await;
    setup.sync();
    let events = setup.sink_events().await;
    assert_eq!(events.len(), 1, "{events:?}");
    assert!(
        events[0].contains("X-CAPYCAL-SOURCE:source/e1"),
        "{events:?}"
    );
    setup.cleanup().await;
}

#[tokio::test]
#[ignore = "needs a CalDAV server, run `make test-caldav`"]
async fn test_conditions_pick_the_events_a_transform_changes() {
    let setup = Setup::new(
        "conditions",
        "transforms = [\n\
         { field = \"SUMMARY\", action = \"set_field\", value = \"At a place\", when = [{ field = \"LOCATION\", empty = false }] },\n\
         { action = \"add_buffer\", pre_event = \"30m\", when = [{ field = \"LOCATION\", contains = \"Room\" }] },\n\
         { action = \"skip\", when = [{ field = \"SUMMARY\", contains = \"At a place\" }, { field = \"LOCATION\", contains = \"Room 2\" }] },\n]",
    )
    .await;
    setup.put("a", &event("a", "")).await;
    setup
        .put("b", &event("b", "").replace("Room 1", "Room 2"))
        .await;
    setup
        .put("c", &event("c", "").replace("LOCATION:Room 1\n", ""))
        .await;
    setup.sync();
    assert_eq!(setup.sink_events().await.len(), 2);
    let at_room = setup.copy_of("a").await;
    assert_contains(
        &at_room,
        &["SUMMARY:At a place", "DTSTART:20300107T083000Z"],
    );
    // No condition matched: no title, not even the source's, and no buffer
    let elsewhere = setup.copy_of("c").await;
    assert!(!elsewhere.contains("SUMMARY"), "{elsewhere}");
    assert_contains(&elsewhere, &["DTSTART:20300107T090000Z"]);
    setup.cleanup().await;
}

#[tokio::test]
#[ignore = "needs a CalDAV server, run `make test-caldav`"]
async fn test_any_group_needs_one_of_its_conditions() {
    let setup = Setup::new(
        "any",
        "transforms = [\n\
         { action = \"skip\", when = [\n\
           { any = [{ field = \"LOCATION\", contains = \"Room 2\" }, { field = \"LOCATION\", contains = \"Room 3\" }] },\n\
           { field = \"SUMMARY\", empty = false },\n\
         ] },\n]",
    )
    .await;
    setup.put("a", &event("a", "")).await;
    setup
        .put("b", &event("b", "").replace("Room 1", "Room 2"))
        .await;
    setup
        .put("c", &event("c", "").replace("Room 1", "Room 3"))
        .await;
    setup.sync();
    // Room 2 or Room 3 is enough to leave the event out
    let events = setup.sink_events().await;
    assert_eq!(events.len(), 1, "{events:?}");
    assert!(
        events[0].contains("X-CAPYCAL-SOURCE:source/a"),
        "{events:?}"
    );
    setup.cleanup().await;
}

/// An event at 09:00 UTC `days` from today.
fn event_in(uid: &str, days: i64, extra: &str) -> String {
    let day = chrono::Utc::now() + chrono::TimeDelta::days(days);
    let start = day.format("%Y%m%dT090000Z");
    let end = day.format("%Y%m%dT100000Z");
    format!(
        "BEGIN:VCALENDAR\nVERSION:2.0\nPRODID:-//capycal test//EN\nBEGIN:VEVENT\nUID:{uid}\n\
         DTSTAMP:20240101T000000Z\nDTSTART:{start}\nDTEND:{end}\nSUMMARY:{uid}\n{extra}\
         END:VEVENT\nEND:VCALENDAR\n"
    )
}

#[tokio::test]
#[ignore = "needs a CalDAV server, run `make test-caldav`"]
async fn test_window_limits_a_caldav_source() {
    let setup = Setup::new(
        "window",
        "all_events = false\ndays_past = 1\ndays_future = 30",
    )
    .await;
    setup.put("soon.ics", &event_in("soon", 1, "")).await;
    setup.put("later.ics", &event_in("later", 60, "")).await;
    setup.put("past.ics", &event_in("past", -10, "")).await;
    let series = event_in("series", -365, "RRULE:FREQ=WEEKLY\n");
    setup.put("series.ics", &series).await;
    // A series that ended long ago, one occurrence moved to the day after tomorrow
    let day = |days: i64| chrono::Utc::now() + chrono::TimeDelta::days(days);
    let (first, until, moved) = (day(-300), day(-200), day(2));
    let ended = format!(
        "BEGIN:VCALENDAR\nVERSION:2.0\nPRODID:-//capycal test//EN\n\
         BEGIN:VEVENT\nUID:moved\nDTSTAMP:20240101T000000Z\nDTSTART:{}\nDTEND:{}\n\
         RRULE:FREQ=WEEKLY;UNTIL={}\nSUMMARY:moved\nEND:VEVENT\n\
         BEGIN:VEVENT\nUID:moved\nDTSTAMP:20240101T000000Z\nRECURRENCE-ID:{}\n\
         DTSTART:{}\nDTEND:{}\nSUMMARY:moved\nEND:VEVENT\nEND:VCALENDAR\n",
        first.format("%Y%m%dT090000Z"),
        first.format("%Y%m%dT100000Z"),
        until.format("%Y%m%dT235959Z"),
        first.format("%Y%m%dT090000Z"),
        moved.format("%Y%m%dT090000Z"),
        moved.format("%Y%m%dT100000Z"),
    );
    setup.put("moved.ics", &ended).await;
    setup.sync();
    let copied = origins(&setup.server, &setup.sink).await;
    assert_eq!(copied, ["source/moved", "source/series", "source/soon"]);

    // A wider window copies the later event with the next (full) sync
    setup.edit_config("days_future = 30", "days_future = 90");
    setup.sync();
    let copied = origins(&setup.server, &setup.sink).await;
    assert_eq!(
        copied,
        [
            "source/later",
            "source/moved",
            "source/series",
            "source/soon"
        ]
    );
    setup.cleanup().await;
}
