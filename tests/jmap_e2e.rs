//! End-to-end tests of the `capycal` binary against a real JMAP server.
//!
//! Ignored by default; `make test-jmap` starts a throwaway Stalwart and runs
//! them. They need `CAPYCAL_TEST_JMAP_URL`, `CAPYCAL_TEST_CALDAV_URL` (the same
//! server over CalDAV), `CAPYCAL_TEST_CALDAV_USER` and
//! `CAPYCAL_TEST_CALDAV_PASSWORD`. Every calendar they create is named
//! `capycal-test-...` and deleted at the end.

use std::path::PathBuf;
use std::process::Command;

use reqwest::{Client, Method};
use serde_json::{Value, json};

const PREFIX: &str = "capycal-test-";
const CALENDARS: &str = "urn:ietf:params:jmap:calendars";

struct Server {
    client: Client,
    jmap: String,
    dav: String,
    user: String,
    password: String,
    api: String,
    account: String,
}

impl Server {
    async fn from_env() -> Self {
        let var = |name: &str| {
            std::env::var(name)
                .unwrap_or_else(|_| panic!("{name} is not set; run `make test-jmap`"))
        };
        let (jmap, mut dav) = (var("CAPYCAL_TEST_JMAP_URL"), var("CAPYCAL_TEST_CALDAV_URL"));
        if !dav.ends_with('/') {
            dav.push('/');
        }
        let (user, password) = (
            var("CAPYCAL_TEST_CALDAV_USER"),
            var("CAPYCAL_TEST_CALDAV_PASSWORD"),
        );
        // No idle connections: `Command::output` blocks this runtime while
        // capycal runs, so a connection the server closes meanwhile goes unnoticed
        let client = Client::builder().pool_max_idle_per_host(0).build().unwrap();
        let session: Value = client
            .get(format!("{jmap}/jmap/session"))
            .basic_auth(&user, Some(&password))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        Self {
            account: session["primaryAccounts"][CALENDARS]
                .as_str()
                .unwrap()
                .to_string(),
            api: format!("{jmap}/jmap/"),
            client,
            jmap,
            dav,
            user,
            password,
        }
    }

    async fn call(&self, method: &str, mut args: Value) -> Value {
        args["accountId"] = json!(self.account);
        let mut answer: Value = self
            .client
            .post(&self.api)
            .basic_auth(&self.user, Some(&self.password))
            .json(&json!({
                "using": ["urn:ietf:params:jmap:core", CALENDARS],
                "methodCalls": [[method, args, "0"]],
            }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let first = answer["methodResponses"][0].take();
        assert_eq!(first[0], method, "{first}");
        first[1].clone()
    }

    fn dav_request(&self, method: &str, url: &str) -> reqwest::RequestBuilder {
        self.client
            .request(Method::from_bytes(method.as_bytes()).unwrap(), url)
            .basic_auth(&self.user, Some(&self.password))
    }

    fn dav_url(&self, name: &str) -> String {
        format!("{}{name}/", self.dav)
    }

    /// A calendar reachable over both protocols, named `name`; returns its
    /// JMAP id.
    async fn create_calendar(&self, name: &str) -> String {
        assert!(name.starts_with(PREFIX));
        let response = self
            .dav_request("MKCALENDAR", &self.dav_url(name))
            .header("Content-Type", "application/xml")
            .body(format!(
                r#"<?xml version="1.0"?><c:mkcalendar xmlns:c="urn:ietf:params:xml:ns:caldav" xmlns:d="DAV:"><d:set><d:prop><d:displayname>{name}</d:displayname></d:prop></d:set></c:mkcalendar>"#
            ))
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success(), "{response:?}");
        self.calendar_id(name).await.expect("the new calendar")
    }

    async fn calendar_id(&self, name: &str) -> Option<String> {
        let answer = self.call("Calendar/get", json!({ "ids": null })).await;
        answer["list"]
            .as_array()?
            .iter()
            .find(|c| c["name"] == name)
            .and_then(|c| c["id"].as_str())
            .map(str::to_string)
    }

    async fn delete_calendar(&self, name: &str) {
        assert!(name.starts_with(PREFIX));
        if let Some(id) = self.calendar_id(name).await {
            self.call(
                "Calendar/set",
                json!({ "destroy": [id], "onDestroyRemoveEvents": true }),
            )
            .await;
        }
    }

    async fn create_event(&self, calendar: &str, mut event: Value) -> String {
        event["@type"] = json!("Event");
        event["calendarIds"] = json!({ calendar: true });
        let answer = self
            .call("CalendarEvent/set", json!({ "create": { "e": event } }))
            .await;
        answer["created"]["e"]["id"]
            .as_str()
            .unwrap_or_else(|| panic!("not created: {answer}"))
            .to_string()
    }

    async fn update_event(&self, id: &str, patch: Value) {
        let answer = self
            .call("CalendarEvent/set", json!({ "update": { id: patch } }))
            .await;
        assert!(answer["updated"].get(id).is_some(), "{answer}");
    }

    async fn delete_event(&self, id: &str) {
        let answer = self
            .call("CalendarEvent/set", json!({ "destroy": [id] }))
            .await;
        assert!(answer["destroyed"][0] == id, "{answer}");
    }

    /// Every event in a calendar, ordered by title.
    async fn events(&self, calendar: &str) -> Vec<Value> {
        let answer = self
            .call(
                "CalendarEvent/query",
                json!({ "filter": { "inCalendar": calendar } }),
            )
            .await;
        let ids = answer["ids"].as_array().cloned().unwrap_or_default();
        let mut list = Vec::new();
        for chunk in ids.chunks(100) {
            let got = self
                .call("CalendarEvent/get", json!({ "ids": chunk }))
                .await;
            list.extend(got["list"].as_array().cloned().unwrap_or_default());
        }
        list.sort_by_key(|e| e["title"].as_str().unwrap_or_default().to_string());
        list
    }

    /// The account's state of events: it moves with every write.
    async fn state(&self) -> String {
        let got = self.call("CalendarEvent/get", json!({ "ids": [] })).await;
        got["state"].as_str().unwrap().to_string()
    }

    /// The iCal of every event in a calendar over CalDAV, unfolded.
    async fn dav_events(&self, name: &str) -> Vec<String> {
        let body = self
            .dav_request("PROPFIND", &self.dav_url(name))
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
        let hrefs = regex::Regex::new(r"<(?:\w+:)?href>([^<]+)</(?:\w+:)?href>").unwrap();
        let mut events = Vec::new();
        for href in hrefs.captures_iter(&body) {
            if href[1].ends_with('/') {
                continue;
            }
            let url = reqwest::Url::parse(&self.dav)
                .unwrap()
                .join(&href[1])
                .unwrap();
            let ics = self
                .dav_request("GET", url.as_str())
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

    async fn dav_put(&self, name: &str, file: &str, ics: &str) {
        let response = self
            .dav_request("PUT", &format!("{}{file}.ics", self.dav_url(name)))
            .header("Content-Type", "text/calendar; charset=utf-8")
            .body(ics.replace('\n', "\r\n"))
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success(), "PUT {file}: {response:?}");
    }
}

/// Two calendars and a config syncing the first into the second, over the
/// protocols `source_jmap` and `sink_jmap` say.
struct Setup {
    server: Server,
    source_name: String,
    sink_name: String,
    source: String,
    sink: String,
    dir: PathBuf,
}

impl Setup {
    async fn new(test: &str, source_jmap: bool, sink_jmap: bool, sync_options: &str) -> Self {
        let server = Server::from_env().await;
        let id = format!("{PREFIX}{}-{test}", std::process::id());
        let (source_name, sink_name) = (format!("{id}-source"), format!("{id}-sink"));
        server.delete_calendar(&source_name).await;
        server.delete_calendar(&sink_name).await;
        let source = server.create_calendar(&source_name).await;
        let sink = server.create_calendar(&sink_name).await;
        let dir = std::env::temp_dir().join(&id);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let calendar = |jmap: bool, name: &str, id: &str| {
            if jmap {
                format!("account = \"jmap\"\njmap_calendar = \"{id}\"\n")
            } else {
                format!("account = \"dav\"\nurl = \"{}\"\n", server.dav_url(name))
            }
        };
        std::fs::write(
            dir.join("capycal.toml"),
            format!(
                "[daemon]\nstate_dir = \"{}\"\n\n\
                 [accounts.jmap]\nurl = \"{}\"\nusername = \"{}\"\npassword = \"{}\"\nprotocol = \"jmap\"\n\n\
                 [accounts.dav]\nurl = \"{}\"\nusername = \"{}\"\npassword = \"{}\"\n\n\
                 [calendars.source]\n{}\n[calendars.sink]\n{}\n\
                 [syncs.test]\nsource = \"source\"\ntarget = \"sink\"\ninterval = \"1h\"\n{sync_options}\n",
                dir.join("state").display(),
                server.jmap,
                server.user,
                server.password,
                server.dav,
                server.user,
                server.password,
                calendar(source_jmap, &source_name, &source),
                calendar(sink_jmap, &sink_name, &sink),
            ),
        )
        .unwrap();
        Self {
            server,
            source_name,
            sink_name,
            source,
            sink,
            dir,
        }
    }

    fn capycal(&self, args: &[&str]) -> String {
        let output = Command::new(env!("CARGO_BIN_EXE_capycal"))
            .arg("--config")
            .arg(self.dir.join("capycal.toml"))
            .args(args)
            .output()
            .unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.status.success(), "capycal {args:?} failed:\n{text}");
        text
    }

    fn sync(&self) {
        self.capycal(&["--once"]);
    }

    /// A full sync must find nothing to write.
    async fn assert_full_sync_writes_nothing(&self) {
        let before = self.server.state().await;
        let log = self.capycal(&["--once", "--full-sync"]);
        assert_eq!(self.server.state().await, before, "{log}");
    }

    fn edit_config(&self, from: &str, to: &str) {
        let path = self.dir.join("capycal.toml");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains(from), "{from} not in config");
        std::fs::write(&path, text.replace(from, to)).unwrap();
    }

    async fn sink_events(&self) -> Vec<Value> {
        self.server.events(&self.sink).await
    }

    async fn only_copy(&self) -> Value {
        let mut copies = self.sink_events().await;
        assert_eq!(copies.len(), 1, "{copies:#?}");
        copies.remove(0)
    }

    async fn cleanup(self) {
        self.server.delete_calendar(&self.source_name).await;
        self.server.delete_calendar(&self.sink_name).await;
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn simple(uid: &str, title: &str) -> Value {
    json!({
        "uid": uid,
        "title": title,
        "start": "2030-01-07T09:00:00",
        "timeZone": "Europe/Berlin",
        "duration": "PT1H",
        "description": "Details\nwith a line, and; signs",
        "locations": { "room": { "@type": "Location", "name": "Room 1" } },
    })
}

fn series(uid: &str) -> Value {
    json!({
        "uid": uid,
        "title": "Jour fixe",
        "start": "2030-01-07T09:00:00",
        "timeZone": "Europe/Berlin",
        "duration": "PT1H",
        "freeBusyStatus": "free",
        "recurrenceRule": { "@type": "RecurrenceRule", "frequency": "weekly", "count": 6, "byDay": [{ "@type": "NDay", "day": "mo" }] },
        "recurrenceOverrides": {
            "2030-01-14T09:00:00": { "start": "2030-01-14T11:00:00", "title": "Jour fixe (moved)" },
            "2030-01-21T09:00:00": { "excluded": true },
        },
        "alerts": { "a": { "@type": "Alert", "action": "display", "trigger": { "@type": "OffsetTrigger", "offset": "-PT15M" } } },
    })
}

const ALL_FIELDS: &str =
    "fields = [\"SUMMARY\", \"DESCRIPTION\", \"LOCATION\", \"VALARM\", \"URL\", \"CATEGORIES\"]";

#[tokio::test]
#[ignore = "needs a JMAP server, run `make test-jmap`"]
async fn test_jmap_to_jmap_creates_updates_and_deletes() {
    let setup = Setup::new("basic", true, true, ALL_FIELDS).await;
    let id = setup
        .server
        .create_event(&setup.source, simple("e", "Planning"))
        .await;
    setup.sync();

    let copy = setup.only_copy().await;
    assert_eq!(copy["title"], "Planning");
    assert_eq!(copy["description"], "Details\nwith a line, and; signs");
    assert_eq!(copy["start"], "2030-01-07T09:00:00");
    assert_eq!(copy["timeZone"], "Europe/Berlin");
    assert_eq!(copy["duration"], "PT1H");
    assert_eq!(copy["locations"]["1"]["name"], "Room 1");
    assert_eq!(copy["capycal.app:sync"], "test");
    assert_eq!(copy["capycal.app:source"], "source/e");
    assert_ne!(copy["uid"], "e");
    let copy_id = copy["id"].clone();

    // Nothing changed: no writes, in a normal cycle or a full sync
    let before = setup.server.state().await;
    setup.sync();
    assert_eq!(setup.server.state().await, before);
    setup.assert_full_sync_writes_nothing().await;

    // A change updates the same copy and clears what the source dropped
    setup
        .server
        .update_event(&id, json!({ "title": "Renamed", "description": null }))
        .await;
    setup.sync();
    let copy = setup.only_copy().await;
    assert_eq!(copy["id"], copy_id);
    assert_eq!(copy["title"], "Renamed");
    assert!(copy["description"].is_null(), "{copy:#}");

    setup.server.delete_event(&id).await;
    setup.sync();
    assert!(setup.sink_events().await.is_empty());
    setup.cleanup().await;
}

#[tokio::test]
#[ignore = "needs a JMAP server, run `make test-jmap`"]
async fn test_series_with_exceptions_survives_jmap_on_both_sides() {
    let setup = Setup::new("series", true, true, ALL_FIELDS).await;
    setup.server.create_event(&setup.source, series("s")).await;
    setup.sync();

    let copy = setup.only_copy().await;
    assert_eq!(copy["recurrenceRule"]["frequency"], "weekly");
    assert_eq!(copy["recurrenceRule"]["count"], 6);
    assert_eq!(copy["freeBusyStatus"], "free");
    assert_eq!(copy["alerts"]["1"]["trigger"]["offset"], "-PT15M");
    let overrides = &copy["recurrenceOverrides"];
    assert_eq!(overrides["2030-01-21T09:00:00"]["excluded"], true);
    let moved = &overrides["2030-01-14T09:00:00"];
    assert_eq!(moved["start"], "2030-01-14T11:00:00");
    assert_eq!(moved["title"], "Jour fixe (moved)");
    setup.assert_full_sync_writes_nothing().await;

    // A change and then the end of the series between two cycles
    let id = setup.server.events(&setup.source).await[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    setup
        .server
        .update_event(&id, json!({ "title": "Jour fixe (new)" }))
        .await;
    setup.server.delete_event(&id).await;
    setup.sync();
    assert!(setup.sink_events().await.is_empty());
    setup.cleanup().await;
}

#[tokio::test]
#[ignore = "needs a JMAP server, run `make test-jmap`"]
async fn test_events_moved_between_calendars_of_the_account() {
    let setup = Setup::new("moved", true, true, "fields = [\"SUMMARY\"]").await;
    let other_name = format!("{}-other", setup.source_name);
    setup.server.delete_calendar(&other_name).await;
    let other = setup.server.create_calendar(&other_name).await;
    let leaving = setup
        .server
        .create_event(&setup.source, simple("leaving", "Leaving"))
        .await;
    let arriving = setup
        .server
        .create_event(&other, simple("arriving", "Arriving"))
        .await;
    setup.sync();
    assert_eq!(setup.only_copy().await["title"], "Leaving");

    // Changes cover the whole account: one moved out is gone, one moved in new
    let to = |calendar: &str| json!({ "calendarIds": { calendar: true } });
    setup.server.update_event(&leaving, to(&other)).await;
    setup
        .server
        .update_event(&arriving, to(&setup.source))
        .await;
    setup.sync();
    assert_eq!(setup.only_copy().await["title"], "Arriving");
    setup.assert_full_sync_writes_nothing().await;
    setup.server.delete_calendar(&other_name).await;
    setup.cleanup().await;
}

#[tokio::test]
#[ignore = "needs a JMAP server, run `make test-jmap`"]
async fn test_all_day_utc_and_busy_blockers() {
    // Without fields: busy blockers without any details
    let setup = Setup::new("kinds", true, true, "").await;
    let all_day = json!({
        "uid": "day", "title": "Holiday", "start": "2030-02-01T00:00:00",
        "showWithoutTime": true, "duration": "P2D",
    });
    let utc = json!({
        "uid": "utc", "title": "Call", "start": "2030-02-05T10:00:00",
        "timeZone": "Etc/UTC", "duration": "PT30M",
    });
    setup.server.create_event(&setup.source, all_day).await;
    setup.server.create_event(&setup.source, utc).await;
    let cancelled = json!({
        "uid": "gone", "title": "Off", "start": "2030-02-06T10:00:00",
        "timeZone": "Etc/UTC", "status": "cancelled",
    });
    setup.server.create_event(&setup.source, cancelled).await;
    setup.sync();

    let copies = setup.sink_events().await;
    assert_eq!(copies.len(), 2, "{copies:#?}");
    let of = |source: &str| {
        copies
            .iter()
            .find(|c| c["capycal.app:source"] == format!("source/{source}"))
            .unwrap()
    };
    assert_eq!(of("day")["showWithoutTime"], true);
    assert_eq!(of("day")["start"], "2030-02-01T00:00:00");
    assert_eq!(of("day")["duration"], "P2D");
    assert_eq!(of("utc")["timeZone"], "Etc/UTC");
    assert_eq!(of("utc")["duration"], "PT30M");
    assert!(of("utc")["title"].is_null() || of("utc")["title"] == "");
    setup.assert_full_sync_writes_nothing().await;
    setup.cleanup().await;
}

#[tokio::test]
#[ignore = "needs a JMAP server, run `make test-jmap`"]
async fn test_caldav_to_jmap() {
    let setup = Setup::new("dav-source", false, true, ALL_FIELDS).await;
    let ics = "BEGIN:VCALENDAR
VERSION:2.0
PRODID:-//capycal test//EN
BEGIN:VEVENT
UID:d1
DTSTAMP:20240101T000000Z
DTSTART;TZID=Europe/Berlin:20300107T090000
DTEND;TZID=Europe/Berlin:20300107T100000
SUMMARY:Standup
RRULE:FREQ=WEEKLY;COUNT=4
EXDATE;TZID=Europe/Berlin:20300114T090000
URL:https://example.org/page
CATEGORIES:One,Two
BEGIN:VALARM
ACTION:DISPLAY
TRIGGER:-PT10M
DESCRIPTION:Soon
END:VALARM
END:VEVENT
BEGIN:VEVENT
UID:d1
DTSTAMP:20240101T000000Z
RECURRENCE-ID;TZID=Europe/Berlin:20300121T090000
DTSTART;TZID=Europe/Berlin:20300121T120000
DTEND;TZID=Europe/Berlin:20300121T130000
SUMMARY:Standup (moved)
END:VEVENT
END:VCALENDAR
";
    setup.server.dav_put(&setup.source_name, "d1", ics).await;
    setup.sync();
    let copy = setup.only_copy().await;
    assert_eq!(copy["title"], "Standup");
    assert_eq!(copy["timeZone"], "Europe/Berlin");
    assert_eq!(copy["recurrenceRule"]["count"], 4);
    assert_eq!(copy["links"]["1"]["href"], "https://example.org/page");
    assert_eq!(copy["keywords"], json!({ "One": true, "Two": true }));
    assert_eq!(
        copy["recurrenceOverrides"]["2030-01-14T09:00:00"]["excluded"],
        true
    );
    assert_eq!(
        copy["recurrenceOverrides"]["2030-01-21T09:00:00"]["title"],
        "Standup (moved)"
    );
    setup.assert_full_sync_writes_nothing().await;
    setup.cleanup().await;
}

#[tokio::test]
#[ignore = "needs a JMAP server, run `make test-jmap`"]
async fn test_jmap_to_caldav() {
    let setup = Setup::new("dav-sink", true, false, ALL_FIELDS).await;
    setup.server.create_event(&setup.source, series("s")).await;
    setup.sync();

    let copies = setup.server.dav_events(&setup.sink_name).await;
    assert_eq!(copies.len(), 1, "{copies:#?}");
    let copy = &copies[0];
    for expected in [
        "X-CAPYCAL-SOURCE:source/s",
        "X-CAPYCAL-SYNC:test",
        "DTSTART;TZID=Europe/Berlin:20300107T090000",
        "RRULE:FREQ=WEEKLY;COUNT=6;BYDAY=MO",
        "EXDATE;TZID=Europe/Berlin:20300121T090000",
        "RECURRENCE-ID;TZID=Europe/Berlin:20300114T090000",
        "SUMMARY:Jour fixe (moved)",
        "TRIGGER:-PT15M",
        "TRANSP:TRANSPARENT",
    ] {
        assert!(copy.contains(expected), "{expected} missing in {copy}");
    }
    setup.assert_full_sync_writes_nothing().await;
    setup.cleanup().await;
}

#[tokio::test]
#[ignore = "needs a JMAP server, run `make test-jmap`"]
async fn test_copies_written_over_caldav_are_taken_over_without_duplicates() {
    let setup = Setup::new("takeover", true, false, "fields = [\"SUMMARY\"]").await;
    setup
        .server
        .create_event(&setup.source, simple("e", "Planning"))
        .await;
    setup.sync();
    assert_eq!(setup.server.dav_events(&setup.sink_name).await.len(), 1);

    let dav = format!(
        "account = \"dav\"\nurl = \"{}\"\n",
        setup.server.dav_url(&setup.sink_name)
    );
    let jmap = format!(
        "account = \"jmap\"\njmap_calendar = \"{}\"\n",
        setup.sink_name
    );
    setup.edit_config(&dav, &jmap);
    setup.sync();

    let copy = setup.only_copy().await;
    assert_eq!(copy["title"], "Planning");
    assert_eq!(copy["capycal.app:sync"], "test");
    setup.cleanup().await;
}

#[tokio::test]
#[ignore = "needs a JMAP server, run `make test-jmap`"]
async fn test_changes_in_the_target_are_repaired() {
    let setup = Setup::new("elsewhere", true, true, "fields = [\"SUMMARY\"]").await;
    setup
        .server
        .create_event(&setup.source, simple("e", "Planning"))
        .await;
    setup.sync();
    let copy = setup.only_copy().await;
    let id = copy["id"].as_str().unwrap();

    setup
        .server
        .update_event(id, json!({ "title": "Edited by hand" }))
        .await;
    setup.sync();
    assert_eq!(setup.only_copy().await["title"], "Planning");

    let id = setup.only_copy().await["id"].as_str().unwrap().to_string();
    setup.server.delete_event(&id).await;
    setup.sync();
    assert_eq!(setup.only_copy().await["title"], "Planning");
    setup.cleanup().await;
}

#[tokio::test]
#[ignore = "needs a JMAP server, run `make test-jmap`"]
async fn test_lost_state_finds_the_copies_again() {
    let setup = Setup::new("state", true, true, "fields = [\"SUMMARY\"]").await;
    setup
        .server
        .create_event(&setup.source, simple("e", "Planning"))
        .await;
    setup.sync();
    let id = setup.only_copy().await["id"].clone();

    std::fs::remove_dir_all(setup.dir.join("state")).unwrap();
    setup.sync();
    assert_eq!(setup.only_copy().await["id"], id);
    setup.cleanup().await;
}

#[tokio::test]
#[ignore = "needs a JMAP server, run `make test-jmap`"]
async fn test_opposite_syncs_do_not_copy_copies_back() {
    let setup = Setup::new("two-way", true, true, "fields = [\"SUMMARY\"]").await;
    setup.edit_config(
        "[syncs.test]",
        "[syncs.back]\nsource = \"sink\"\ntarget = \"source\"\ninterval = \"1h\"\nfields = [\"SUMMARY\"]\n\n[syncs.test]",
    );
    setup
        .server
        .create_event(&setup.source, simple("a", "From source"))
        .await;
    setup
        .server
        .create_event(&setup.sink, simple("b", "From sink"))
        .await;
    setup.sync();
    setup.sync();
    assert_eq!(setup.server.events(&setup.source).await.len(), 2);
    assert_eq!(setup.server.events(&setup.sink).await.len(), 2);
    setup.cleanup().await;
}

#[tokio::test]
#[ignore = "needs a JMAP server, run `make test-jmap`"]
async fn test_purge_removes_only_the_copies_and_purge_all_everything() {
    let setup = Setup::new("purge", true, true, "fields = [\"SUMMARY\"]").await;
    setup
        .server
        .create_event(&setup.source, simple("e", "Planning"))
        .await;
    setup
        .server
        .create_event(&setup.sink, simple("own", "Not a copy"))
        .await;
    setup.sync();
    assert_eq!(setup.sink_events().await.len(), 2);

    setup.capycal(&["--purge", "test"]);
    let left = setup.sink_events().await;
    assert_eq!(left.len(), 1, "{left:#?}");
    assert_eq!(left[0]["title"], "Not a copy");
    assert_eq!(setup.server.events(&setup.source).await.len(), 1);

    setup.capycal(&["--purge-all", "sink", "--yes"]);
    assert!(setup.sink_events().await.is_empty());
    setup.cleanup().await;
}

#[tokio::test]
#[ignore = "needs a JMAP server, run `make test-jmap`"]
async fn test_calendar_named_instead_of_id() {
    let setup = Setup::new("by-name", true, true, "fields = [\"SUMMARY\"]").await;
    setup.edit_config(
        &format!("jmap_calendar = \"{}\"", setup.sink),
        &format!("jmap_calendar = \"{}\"", setup.sink_name),
    );
    setup
        .server
        .create_event(&setup.source, simple("e", "Planning"))
        .await;
    setup.sync();
    assert_eq!(setup.only_copy().await["title"], "Planning");
    setup.cleanup().await;
}

#[tokio::test]
#[ignore = "needs a JMAP server, run `make test-jmap`"]
async fn test_participants_cancelled_occurrences_and_odd_time_zones() {
    let setup = Setup::new(
        "details",
        false,
        true,
        "fields = [\"SUMMARY\", \"ORGANIZER\", \"ATTENDEE\"]",
    )
    .await;
    let ics = "BEGIN:VCALENDAR
VERSION:2.0
PRODID:-//capycal test//EN
BEGIN:VTIMEZONE
TZID:Custom Zone
BEGIN:STANDARD
DTSTART:19700101T000000
TZOFFSETFROM:+0300
TZOFFSETTO:+0300
END:STANDARD
END:VTIMEZONE
BEGIN:VEVENT
UID:team
DTSTAMP:20240101T000000Z
DTSTART;TZID=W. Europe Standard Time:20300107T090000
DTEND;TZID=W. Europe Standard Time:20300107T100000
SUMMARY:Team
ORGANIZER;CN=Boss:mailto:boss@example.org
ATTENDEE;CN=Ann;PARTSTAT=ACCEPTED;ROLE=REQ-PARTICIPANT:mailto:ann@example.org
ATTENDEE;ROLE=OPT-PARTICIPANT:mailto:ben@example.org
RRULE:FREQ=DAILY;COUNT=3
END:VEVENT
BEGIN:VEVENT
UID:team
DTSTAMP:20240101T000000Z
RECURRENCE-ID;TZID=W. Europe Standard Time:20300108T090000
DTSTART;TZID=W. Europe Standard Time:20300108T090000
DTEND;TZID=W. Europe Standard Time:20300108T100000
SUMMARY:Team
STATUS:CANCELLED
END:VEVENT
END:VCALENDAR
";
    setup.server.dav_put(&setup.source_name, "team", ics).await;
    let custom = "BEGIN:VCALENDAR
VERSION:2.0
PRODID:-//capycal test//EN
BEGIN:VTIMEZONE
TZID:Custom Zone
BEGIN:STANDARD
DTSTART:19700101T000000
TZOFFSETFROM:+0300
TZOFFSETTO:+0300
END:STANDARD
END:VTIMEZONE
BEGIN:VEVENT
UID:custom
DTSTAMP:20240101T000000Z
DTSTART;TZID=Custom Zone:20300109T120000
DTEND;TZID=Custom Zone:20300109T130000
SUMMARY:Zone
END:VEVENT
END:VCALENDAR
";
    setup
        .server
        .dav_put(&setup.source_name, "custom", custom)
        .await;
    setup.sync();

    let copies = setup.sink_events().await;
    assert_eq!(copies.len(), 2, "{copies:#?}");
    let team = copies.iter().find(|c| c["title"] == "Team").unwrap();
    assert_eq!(team["timeZone"], "Europe/Berlin");
    let people = team["participants"].as_object().unwrap();
    assert_eq!(people.len(), 3, "{team:#}");
    let person = |address: &str| {
        people
            .values()
            .find(|p| p["calendarAddress"] == address)
            .unwrap_or_else(|| panic!("{address} missing in {team:#}"))
    };
    assert_eq!(person("mailto:boss@example.org")["roles"]["owner"], true);
    assert_eq!(
        person("mailto:ann@example.org")["participationStatus"],
        "accepted"
    );
    assert_eq!(person("mailto:ben@example.org")["roles"]["optional"], true);
    assert_eq!(
        team["recurrenceOverrides"]["2030-01-08T09:00:00"]["status"],
        "cancelled"
    );

    // A zone only its VTIMEZONE knows arrives as UTC
    let zone = copies.iter().find(|c| c["title"] == "Zone").unwrap();
    assert_eq!(zone["start"], "2030-01-09T09:00:00");
    assert_eq!(zone["timeZone"], "Etc/UTC");
    assert_eq!(zone["duration"], "PT1H");
    setup.assert_full_sync_writes_nothing().await;
    setup.cleanup().await;
}

#[tokio::test]
#[ignore = "needs a JMAP server, run `make test-jmap`"]
async fn test_more_events_than_one_request_may_hold() {
    let setup = Setup::new("many", true, true, "fields = [\"SUMMARY\"]").await;
    let create: serde_json::Map<String, Value> = (0..520)
        .map(|i| {
            let mut event = simple(&format!("e{i}"), &format!("Event {i}"));
            event["@type"] = json!("Event");
            event["calendarIds"] = json!({ setup.source.as_str(): true });
            (format!("e{i}"), event)
        })
        .collect();
    let mut chunks = create.into_iter().peekable();
    while chunks.peek().is_some() {
        let chunk: serde_json::Map<String, Value> = chunks.by_ref().take(250).collect();
        setup
            .server
            .call("CalendarEvent/set", json!({ "create": chunk }))
            .await;
    }
    setup.sync();
    assert_eq!(setup.sink_events().await.len(), 520);
    setup.assert_full_sync_writes_nothing().await;
    setup.capycal(&["--purge", "test"]);
    assert!(setup.sink_events().await.is_empty());
    setup.cleanup().await;
}
