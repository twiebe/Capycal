use serde_json::Value;
use tracing::warn;

use crate::error::{Error, Result};
use crate::ical::marker::{MARKER_SOURCE, MARKER_SYNC, source_uid};
use crate::jmap::client::JmapClient;
use crate::jmap::convert;
use crate::source::{EventSink, EventSource, PutResult, SinkChanges, SinkEvent, SourceEvent};
use crate::state::SyncState;
use crate::sync::diff::EtagDiff;

/// The file ending of the href the runner asks for a new copy. Ids a server
/// assigns have none (RFC 8620 section 1.2 allows no dot).
const REQUESTED_HREF: &str = ".ics";

pub struct JmapSource {
    client: JmapClient,
}

impl JmapSource {
    pub fn new(client: JmapClient) -> Self {
        Self { client }
    }

    /// The event with its id as href and a hash of its iCal as etag. None for
    /// one that cannot be read, which is logged: it must not stop the others.
    /// Its copy goes, as on a full sync, which cannot match it either.
    fn source_event(event: &Value) -> Option<SourceEvent> {
        let id = event["id"].as_str()?;
        let ical = match convert::to_ical(event) {
            Ok(ical) => ical,
            Err(e) => {
                warn!(event = id, error = %e, "skipping a JMAP event that cannot be read");
                return None;
            }
        };
        Some(SourceEvent {
            uid: event["uid"].as_str().unwrap_or_default().to_string(),
            href: id.to_string(),
            etag: uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, ical.as_bytes()).to_string(),
            calendar_data: ical,
        })
    }

    /// All events of the calendar, compared with the synced ones. An empty
    /// state reports every event.
    async fn fetch_all(&self, state: &mut SyncState) -> Result<(Vec<SourceEvent>, Vec<String>)> {
        // Before the events: a change while reading shows up next time
        let token = self.client.state().await?;
        let events = self.client.all_events().await?;
        let mut diff = EtagDiff::new(state.uid_mapping.source_etags());
        let changed = events
            .iter()
            .filter_map(Self::source_event)
            .filter(|e| diff.observe(&e.href, &e.etag))
            .collect();
        let removed = diff.removed();
        state.sync_token = Some(token);
        Ok((changed, removed))
    }
}

impl EventSource for JmapSource {
    async fn fetch_changes(
        &self,
        state: &mut SyncState,
    ) -> Result<(Vec<SourceEvent>, Vec<String>)> {
        let Some(token) = state.sync_token.clone() else {
            return self.fetch_all(state).await;
        };
        let changes = match self.client.changes(&token).await {
            Ok(changes) => changes,
            Err(Error::SyncTokenExpired) => {
                warn!("JMAP state expired, reading the whole calendar");
                state.sync_token = None;
                return self.fetch_all(state).await;
            }
            Err(e) => return Err(e),
        };
        let calendar = self.client.calendar_id().await?;
        let events = self.client.events(&changes.changed).await?;
        let mut removed = changes.destroyed;
        let mut changed = Vec::new();
        // Changes cover the whole account. One gone by now, moved to another
        // calendar or unreadable counts as removed; the runner skips unknown ones.
        for id in &changes.changed {
            let here = events.iter().find(|e| e["id"].as_str() == Some(id));
            match here
                .filter(|e| e["calendarIds"][calendar].as_bool() == Some(true))
                .and_then(Self::source_event)
            {
                Some(event) => changed.push(event),
                None => removed.push(id.clone()),
            }
        }
        // Advance only after everything was read, so a failure is retried
        state.sync_token = Some(changes.state);
        Ok((changed, removed))
    }
}

#[derive(Clone)]
pub struct JmapSink {
    client: JmapClient,
}

impl JmapSink {
    pub fn new(client: JmapClient) -> Self {
        Self { client }
    }

    /// The ids of every event in the calendar.
    pub async fn event_ids(&self) -> Result<Vec<String>> {
        self.client.event_ids().await
    }
}

impl EventSink for JmapSink {
    type Stored = Value;

    /// The server assigns ids, so a requested href first looks for an event
    /// with the copy's UID: a put whose result was never recorded left one.
    async fn put_event(&self, href: &str, ical_data: &str) -> Result<PutResult> {
        let event = convert::from_ical(ical_data)?;
        let id = if href.ends_with(REQUESTED_HREF) {
            let uid = event["uid"].as_str().unwrap_or_default();
            self.client.find_uid(uid).await?
        } else {
            Some(href.to_string())
        };
        if let Some(id) = id
            && let Some(etag) = self
                .client
                .update(&id, convert::update_patch(&event))
                .await?
        {
            return Ok(PutResult { etag, href: id });
        }
        // New, or deleted in the target since
        let (href, etag) = self.client.create(event).await?;
        Ok(PutResult { etag, href })
    }

    async fn delete_event(&self, href: &str) -> Result<()> {
        self.client.destroy(href).await
    }

    async fn list_synced(
        &self,
        sync_name: &str,
        source_name: &str,
    ) -> Result<Vec<SinkEvent<Value>>> {
        let events = self
            .client
            .all_events()
            .await
            .map_err(|e| Error::SinkEvents {
                action: "fetching target events",
                source: Box::new(e),
            })?;
        Ok(events
            .into_iter()
            .filter(|e| convert::marker(e, MARKER_SYNC).as_deref() == Some(sync_name))
            .filter_map(|event| {
                let line = convert::marker(&event, MARKER_SOURCE)
                    .map(|value| format!("{MARKER_SOURCE}:{value}"));
                Some(SinkEvent {
                    href: event["id"].as_str()?.to_string(),
                    uid: event["uid"].as_str().map(str::to_string),
                    source_uid: line.and_then(|l| source_uid(&l, source_name)),
                    stored: event,
                })
            })
            .collect())
    }

    fn is_unchanged(&self, stored: &Value, ical_data: &str) -> bool {
        convert::from_ical(ical_data).is_ok_and(|want| convert::same_event(stored, &want))
    }

    /// Through `CalendarEvent/changes`, which covers the whole account; the
    /// runner only looks at the ids of its copies.
    async fn changes_since(&self, token: Option<&str>) -> Result<Option<SinkChanges>> {
        let Some(token) = token else {
            let token = self.client.state().await?;
            return Ok(Some(SinkChanges {
                hrefs: Vec::new(),
                token,
            }));
        };
        let changes = self.client.changes(token).await?;
        Ok(Some(SinkChanges {
            hrefs: changes
                .changed
                .into_iter()
                .chain(changes.destroyed)
                .collect(),
            token: changes.state,
        }))
    }

    /// A repeated put finds the copy by its UID.
    fn cancel_safe(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::caldav::auth::ResolvedAuth;
    use crate::state::mapping::SinkRecord;
    use serde_json::json;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockBuilder, MockServer, ResponseTemplate};

    const ICS: &str = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:copy-1\r\n\
        DTSTART:20261110T090000Z\r\nSUMMARY:T\r\nX-CAPYCAL-SOURCE:work/src-1\r\n\
        X-CAPYCAL-SYNC:s\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

    async fn server() -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/.well-known/jmap"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "apiUrl": format!("{}/jmap/", server.uri()),
                "capabilities": {
                    "urn:ietf:params:jmap:core": {},
                    "urn:ietf:params:jmap:calendars": {},
                },
                "primaryAccounts": { "urn:ietf:params:jmap:calendars": "acc" },
            })))
            .mount(&server)
            .await;
        post("Calendar/get")
            .respond_with(answer(
                "Calendar/get",
                json!({ "list": [{ "id": "cal", "name": "Work" }] }),
            ))
            .mount(&server)
            .await;
        server
    }

    fn client(server: &MockServer) -> JmapClient {
        JmapClient::new(
            &server.uri(),
            ResolvedAuth {
                username: "u".into(),
                password: "p".into(),
            },
            "cal",
        )
        .unwrap()
    }

    fn post(text: &str) -> MockBuilder {
        Mock::given(method("POST")).and(body_string_contains(text))
    }

    fn answer(method: &str, args: serde_json::Value) -> ResponseTemplate {
        ResponseTemplate::new(200)
            .set_body_json(json!({ "methodResponses": [[method, args, "0"]] }))
    }

    fn event(id: &str, calendar: &str) -> serde_json::Value {
        json!({
            "id": id, "uid": format!("uid-{id}"), "calendarIds": { calendar: true },
            "start": "2026-11-10T09:00:00", "timeZone": "Etc/UTC", "title": "T",
        })
    }

    fn known(state: &mut SyncState, uid: &str, href: &str, etag: &str) {
        state.uid_mapping.insert(
            uid.to_string(),
            SinkRecord {
                sink_uid: uid.to_string(),
                sink_href: format!("{uid}.ics"),
                source_href: href.to_string(),
                source_etag: etag.to_string(),
            },
        );
    }

    #[tokio::test]
    async fn test_first_fetch_reads_all_and_keeps_the_state_before_reading() {
        let server = server().await;
        post("\"ids\":[]")
            .respond_with(answer(
                "CalendarEvent/get",
                json!({ "list": [], "state": "s1" }),
            ))
            .mount(&server)
            .await;
        post("CalendarEvent/query")
            .and(body_string_contains("\"position\":0"))
            .respond_with(answer("CalendarEvent/query", json!({ "ids": ["a"] })))
            .mount(&server)
            .await;
        post("CalendarEvent/query")
            .and(body_string_contains("\"position\":1"))
            .respond_with(answer("CalendarEvent/query", json!({ "ids": [] })))
            .mount(&server)
            .await;
        post("\"ids\":[\"a\"]")
            .respond_with(answer(
                "CalendarEvent/get",
                json!({ "list": [event("a", "cal")], "state": "s2" }),
            ))
            .mount(&server)
            .await;

        let mut state = SyncState::default();
        let (events, removed) = JmapSource::new(client(&server))
            .fetch_changes(&mut state)
            .await
            .unwrap();
        assert_eq!(state.sync_token.as_deref(), Some("s1"));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].href, "a");
        assert_eq!(events[0].uid, "uid-a");
        assert!(events[0].calendar_data.contains("UID:uid-a\r\n"));
        assert!(removed.is_empty());
    }

    #[tokio::test]
    async fn test_incremental_fetch_covers_other_calendars_and_removals() {
        let server = server().await;
        post("CalendarEvent/changes")
            .respond_with(answer(
                "CalendarEvent/changes",
                json!({
                    "created": ["new", "elsewhere"], "updated": ["moved", "gone", "new", "broken"],
                    "destroyed": ["deleted"], "newState": "s2", "hasMoreChanges": false,
                }),
            ))
            .mount(&server)
            .await;
        post("CalendarEvent/get")
            .respond_with(answer(
                "CalendarEvent/get",
                json!({
                    "list": [
                        event("new", "cal"), event("elsewhere", "other"), event("moved", "other"),
                        { "id": "broken", "uid": "uid-b", "calendarIds": { "cal": true } },
                    ],
                    "notFound": ["gone"], "state": "s2",
                }),
            ))
            .mount(&server)
            .await;
        let mut state = SyncState {
            sync_token: Some("s1".to_string()),
            ..Default::default()
        };
        let (events, removed) = JmapSource::new(client(&server))
            .fetch_changes(&mut state)
            .await
            .unwrap();
        assert_eq!(state.sync_token.as_deref(), Some("s2"));
        // Created and updated since s1: one event, not two puts
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].href, "new");
        // An unreadable one loses its copy, as on a full sync
        assert_eq!(removed, ["deleted", "broken", "elsewhere", "gone", "moved"]);
    }

    #[tokio::test]
    async fn test_failed_read_keeps_the_old_state() {
        let server = server().await;
        post("CalendarEvent/changes")
            .respond_with(answer(
                "CalendarEvent/changes",
                json!({ "created": ["a"], "updated": [], "destroyed": [], "newState": "s2" }),
            ))
            .mount(&server)
            .await;
        post("CalendarEvent/get")
            .respond_with(ResponseTemplate::new(400).set_body_string("no"))
            .mount(&server)
            .await;
        let mut state = SyncState {
            sync_token: Some("s1".to_string()),
            ..Default::default()
        };
        let source = JmapSource::new(client(&server));
        assert!(source.fetch_changes(&mut state).await.is_err());
        assert_eq!(state.sync_token.as_deref(), Some("s1"));
    }

    #[tokio::test]
    async fn test_expired_state_compares_the_whole_calendar() {
        let server = server().await;
        post("CalendarEvent/changes")
            .respond_with(answer("error", json!({ "type": "cannotCalculateChanges" })))
            .mount(&server)
            .await;
        post("\"ids\":[]")
            .respond_with(answer(
                "CalendarEvent/get",
                json!({ "list": [], "state": "fresh" }),
            ))
            .mount(&server)
            .await;
        post("CalendarEvent/query")
            .and(body_string_contains("\"position\":0"))
            .respond_with(answer("CalendarEvent/query", json!({ "ids": ["a", "b"] })))
            .mount(&server)
            .await;
        post("CalendarEvent/query")
            .and(body_string_contains("\"position\":2"))
            .respond_with(answer("CalendarEvent/query", json!({ "ids": [] })))
            .mount(&server)
            .await;
        post("\"ids\":[\"a\",\"b\"]")
            .respond_with(answer(
                "CalendarEvent/get",
                json!({ "list": [event("a", "cal"), event("b", "cal")] }),
            ))
            .mount(&server)
            .await;

        let unchanged = JmapSource::source_event(&event("a", "cal")).unwrap();
        let mut state = SyncState {
            sync_token: Some("old".to_string()),
            ..Default::default()
        };
        known(&mut state, "uid-a", "a", &unchanged.etag);
        known(&mut state, "uid-gone", "gone", "e");
        let (events, removed) = JmapSource::new(client(&server))
            .fetch_changes(&mut state)
            .await
            .unwrap();
        assert_eq!(state.sync_token.as_deref(), Some("fresh"));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].href, "b");
        assert_eq!(removed, ["gone"]);
    }

    #[test]
    fn test_unreadable_source_event_is_skipped() {
        assert!(JmapSource::source_event(&json!({ "id": "a", "uid": "u" })).is_none());
        assert!(JmapSource::source_event(&json!({ "uid": "u" })).is_none());
    }

    #[tokio::test]
    async fn test_put_of_a_new_copy_creates_it_in_the_calendar() {
        let server = server().await;
        post("\"uid\":\"copy-1\"")
            .and(body_string_contains("CalendarEvent/query"))
            .respond_with(answer("CalendarEvent/query", json!({ "ids": [] })))
            .expect(1)
            .mount(&server)
            .await;
        post("\"create\"")
            .and(body_string_contains("\"calendarIds\":{\"cal\":true}"))
            .and(body_string_contains("\"capycal.app:sync\":\"s\""))
            .respond_with(answer(
                "CalendarEvent/set",
                json!({ "created": { "e": { "id": "new-id", "updated": "t" } } }),
            ))
            .expect(1)
            .mount(&server)
            .await;
        let res = JmapSink::new(client(&server))
            .put_event("copy-1.ics", ICS)
            .await
            .unwrap();
        assert_eq!(res.href, "new-id");
        assert_eq!(res.etag, "t");
    }

    #[tokio::test]
    async fn test_repeated_put_of_a_requested_href_updates_the_same_event() {
        let server = server().await;
        post("CalendarEvent/query")
            .respond_with(answer("CalendarEvent/query", json!({ "ids": ["old-id"] })))
            .mount(&server)
            .await;
        post("\"update\"")
            .and(body_string_contains("old-id"))
            .and(body_string_contains("\"recurrenceRule\":null"))
            // Found in another calendar of the account, it moves here
            .and(body_string_contains("\"calendarIds\":{\"cal\":true}"))
            .respond_with(answer(
                "CalendarEvent/set",
                json!({ "updated": { "old-id": null } }),
            ))
            .expect(1)
            .mount(&server)
            .await;
        let sink = JmapSink::new(client(&server));
        assert!(sink.cancel_safe());
        let res = sink.put_event("copy-1.ics", ICS).await.unwrap();
        assert_eq!(res.href, "old-id");
    }

    #[tokio::test]
    async fn test_put_recreates_a_copy_deleted_in_the_target() {
        let server = server().await;
        post("\"update\"")
            .respond_with(answer(
                "CalendarEvent/set",
                json!({ "notUpdated": { "old-id": { "type": "notFound" } } }),
            ))
            .expect(1)
            .mount(&server)
            .await;
        post("\"create\"")
            .respond_with(answer(
                "CalendarEvent/set",
                json!({ "created": { "e": { "id": "new-id" } } }),
            ))
            .expect(1)
            .mount(&server)
            .await;
        let res = JmapSink::new(client(&server))
            .put_event("old-id", ICS)
            .await
            .unwrap();
        assert_eq!(res.href, "new-id");
    }

    #[tokio::test]
    async fn test_list_synced_finds_the_markers_of_this_sync() {
        let server = server().await;
        post("CalendarEvent/query")
            .and(body_string_contains("\"position\":0"))
            .respond_with(answer(
                "CalendarEvent/query",
                json!({ "ids": ["a", "b", "c", "d"] }),
            ))
            .mount(&server)
            .await;
        post("CalendarEvent/query")
            .and(body_string_contains("\"position\":4"))
            .respond_with(answer("CalendarEvent/query", json!({ "ids": [] })))
            .mount(&server)
            .await;
        post("CalendarEvent/get")
            .respond_with(answer(
                "CalendarEvent/get",
                json!({ "list": [
                    { "id": "a", "uid": "ua", "capycal.app:sync": "s", "capycal.app:source": "work/src/1" },
                    { "id": "b", "uid": "ub", "capycal.app:sync": "other", "capycal.app:source": "work/x" },
                    { "id": "c", "uid": "uc", "iCalendar": { "properties": [
                        ["x-capycal-source", {}, "unknown", "work/src-3"],
                        ["x-capycal-sync", {}, "unknown", "s"],
                    ] } },
                    { "id": "d", "uid": "ud" },
                ] }),
            ))
            .mount(&server)
            .await;
        let events = JmapSink::new(client(&server))
            .list_synced("s", "work")
            .await
            .unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].href, "a");
        assert_eq!(events[0].uid.as_deref(), Some("ua"));
        assert_eq!(events[0].source_uid.as_deref(), Some("src/1"));
        assert_eq!(events[1].href, "c");
        assert_eq!(events[1].source_uid.as_deref(), Some("src-3"));
    }

    #[tokio::test]
    async fn test_changes_since_without_a_token_only_reads_the_state() {
        let server = server().await;
        post("\"ids\":[]")
            .respond_with(answer(
                "CalendarEvent/get",
                json!({ "list": [], "state": "s1" }),
            ))
            .mount(&server)
            .await;
        post("CalendarEvent/changes")
            .respond_with(answer(
                "CalendarEvent/changes",
                json!({ "created": ["a"], "updated": ["b"], "destroyed": ["c"], "newState": "s2" }),
            ))
            .mount(&server)
            .await;
        let sink = JmapSink::new(client(&server));
        let first = sink.changes_since(None).await.unwrap().unwrap();
        assert_eq!(first.token, "s1");
        assert!(first.hrefs.is_empty());
        let next = sink.changes_since(Some("s1")).await.unwrap().unwrap();
        assert_eq!(next.token, "s2");
        assert_eq!(next.hrefs, ["a", "b", "c"]);
    }
}
