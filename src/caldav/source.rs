use tracing::{debug, warn};

use crate::caldav::client::{CalDavClient, EventResource};
use crate::error::{Error, Result};
use crate::ical::marker::{has_sync_marker, source_uid};
use crate::ical::{self, extract_uid};
use crate::source::{EventSink, EventSource, PutResult, SinkChanges, SinkEvent, SourceEvent};
use crate::state::SyncState;
use crate::sync::diff::EtagDiff;

/// CalDAV implementation of EventSource - wraps the existing CalDavClient fetch logic.
pub struct CalDavSource {
    client: CalDavClient,
}

impl CalDavSource {
    pub fn new(client: CalDavClient) -> Self {
        Self { client }
    }
}

impl EventSource for CalDavSource {
    async fn fetch_changes(
        &self,
        state: &mut SyncState,
    ) -> Result<(Vec<SourceEvent>, Vec<String>)> {
        // Try sync-collection REPORT (RFC 6578)
        let resp = match self
            .client
            .sync_collection(state.sync_token.as_deref())
            .await
        {
            Ok(resp) => Some(resp),
            Err(Error::SyncTokenExpired) => {
                warn!("sync token expired, falling back to full sync");
                state.sync_token = None;
                match self.client.sync_collection(None).await {
                    Ok(resp) => Some(resp),
                    Err(e) => {
                        warn!(error = %e, "sync-collection retry failed, trying ctag fallback");
                        None
                    }
                }
            }
            Err(e) => {
                debug!(error = %e, "sync-collection not available, using ctag fallback");
                None
            }
        };

        let Some(resp) = resp else {
            return self.fetch_changes_ctag(state).await;
        };

        let mut events: Vec<SourceEvent> = resp.changed.into_iter().map(source_event).collect();
        if !resp.pending_hrefs.is_empty() {
            let fetched = self.client.multiget(&resp.pending_hrefs).await?;
            events.extend(fetched.into_iter().map(source_event));
        }
        // Advance only after the whole batch was fetched, so a failed multiget is retried
        state.sync_token = Some(resp.sync_token);
        Ok((events, resp.removed))
    }
}

fn source_event(e: EventResource) -> SourceEvent {
    SourceEvent {
        uid: String::new(), // filled by caller via extract_uid
        href: e.href,
        etag: e.etag,
        calendar_data: e.calendar_data,
    }
}

impl CalDavSource {
    async fn fetch_changes_ctag(
        &self,
        state: &mut SyncState,
    ) -> Result<(Vec<SourceEvent>, Vec<String>)> {
        let props = self.client.get_calendar_props().await?;

        if let (Some(stored_ctag), Some(server_ctag)) = (&state.ctag, &props.ctag)
            && stored_ctag == server_ctag
        {
            return Ok((vec![], vec![]));
        }

        let refs = self.client.list_events().await?;

        let mut diff = EtagDiff::new(state.uid_mapping.source_etags());
        let changed_hrefs: Vec<String> = refs
            .iter()
            .filter(|r| diff.observe(&r.href, &r.etag))
            .map(|r| r.href.clone())
            .collect();

        let changed_events = if changed_hrefs.is_empty() {
            vec![]
        } else {
            self.client
                .multiget(&changed_hrefs)
                .await?
                .into_iter()
                .map(source_event)
                .collect()
        };

        let removed = diff.removed();

        if let Some(ctag) = props.ctag {
            state.ctag = Some(ctag);
        }

        Ok((changed_events, removed))
    }
}

/// CalDAV implementation of EventSink - wraps put_event/delete_event.
#[derive(Clone)]
pub struct CalDavSink {
    client: CalDavClient,
}

impl CalDavSink {
    pub fn new(client: CalDavClient) -> Self {
        Self { client }
    }
}

impl EventSink for CalDavSink {
    type Stored = String;

    async fn put_event(&self, href: &str, ical_data: &str) -> Result<PutResult> {
        let etag = self.client.put_event(href, ical_data, None).await?;
        Ok(PutResult {
            etag,
            href: href.to_string(),
        })
    }

    async fn delete_event(&self, href: &str) -> Result<()> {
        self.client.delete_event(href).await
    }

    async fn list_synced(
        &self,
        sync_name: &str,
        source_name: &str,
    ) -> Result<Vec<SinkEvent<String>>> {
        synced_events(&self.client, sync_name, source_name).await
    }

    fn is_unchanged(&self, stored: &String, ical_data: &str) -> bool {
        ical::same_content(stored, ical_data)
    }

    /// Through sync-collection (RFC 6578); None for a server without it.
    async fn changes_since(&self, token: Option<&str>) -> Result<Option<SinkChanges>> {
        match self.client.sync_collection_hrefs(token).await {
            Ok(resp) => Ok(Some(SinkChanges {
                hrefs: resp
                    .pending_hrefs
                    .into_iter()
                    .chain(resp.changed.into_iter().map(|e| e.href))
                    .chain(resp.removed)
                    .collect(),
                token: resp.sync_token,
            })),
            Err(Error::SyncTokenExpired) => Err(Error::SyncTokenExpired),
            Err(e) => {
                debug!(error = %e, "target has no sync-collection, changes elsewhere go unnoticed");
                Ok(None)
            }
        }
    }

    /// The sink href is chosen by capycal, so a repeated PUT replaces the same resource.
    fn cancel_safe(&self) -> bool {
        true
    }
}

/// The events of the calendar that carry the sync marker of exactly
/// `sync_name`: list the calendar, then fetch its events with a multiget.
pub(crate) async fn synced_events(
    client: &CalDavClient,
    sync_name: &str,
    source_name: &str,
) -> Result<Vec<SinkEvent<String>>> {
    let sink_error = |action| {
        move |e| Error::SinkEvents {
            action,
            source: Box::new(e),
        }
    };
    let refs = client
        .list_events()
        .await
        .map_err(sink_error("listing target events"))?;
    if refs.is_empty() {
        return Ok(Vec::new());
    }
    let hrefs: Vec<String> = refs.into_iter().map(|r| r.href).collect();
    let events = client
        .multiget(&hrefs)
        .await
        .map_err(sink_error("fetching target events"))?;
    Ok(events
        .into_iter()
        .filter(|e| has_sync_marker(&e.calendar_data, sync_name))
        .map(|e| SinkEvent {
            uid: extract_uid(&e.calendar_data),
            source_uid: source_uid(&e.calendar_data, source_name),
            href: e.href,
            stored: e.calendar_data,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::caldav::auth::ResolvedAuth;
    use crate::state::mapping::SinkRecord;
    use wiremock::matchers::{body_string_contains, header, method, path};
    use wiremock::{Mock, MockBuilder, MockServer, ResponseTemplate};

    const ICS: &str = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:e1\r\nEND:VEVENT\r\nEND:VCALENDAR";

    fn client(server: &MockServer) -> CalDavClient {
        CalDavClient::new(
            &format!("{}/cal/", server.uri()),
            ResolvedAuth {
                username: "u".to_string(),
                password: "p".to_string(),
            },
        )
        .unwrap()
    }

    fn multistatus(inner: &str) -> ResponseTemplate {
        ResponseTemplate::new(207).set_body_string(format!(
            r#"<?xml version="1.0"?>
<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav" xmlns:cs="http://calendarserver.org/ns/">
{inner}
</d:multistatus>"#
        ))
    }

    fn event(href: &str, etag: &str) -> String {
        format!(
            r#"<d:response><d:href>{href}</d:href><d:propstat><d:prop>
<d:getetag>{etag}</d:getetag><c:calendar-data>{ICS}</c:calendar-data>
</d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>"#
        )
    }

    fn etag_only(href: &str, etag: &str) -> String {
        format!(
            r#"<d:response><d:href>{href}</d:href><d:propstat><d:prop>
<d:getetag>{etag}</d:getetag></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>"#
        )
    }

    fn sync_report(token_xml: &str) -> MockBuilder {
        Mock::given(method("REPORT"))
            .and(body_string_contains("sync-collection"))
            .and(body_string_contains(token_xml))
    }

    fn multiget() -> MockBuilder {
        Mock::given(method("REPORT")).and(body_string_contains("calendar-multiget"))
    }

    fn ctag_props(ctag: &str) -> Mock {
        Mock::given(method("PROPFIND"))
            .and(path("/cal"))
            .and(header("Depth", "0"))
            .respond_with(multistatus(&format!(
                "<d:response><d:href>/cal/</d:href><d:propstat><d:prop><cs:getctag>{ctag}</cs:getctag></d:prop></d:propstat></d:response>"
            )))
    }

    fn list_events(inner: &str) -> Mock {
        Mock::given(method("PROPFIND"))
            .and(header("Depth", "1"))
            .respond_with(multistatus(inner))
    }

    fn known(state: &mut SyncState, uid: &str, href: &str, etag: &str) {
        state.uid_mapping.insert(
            uid.to_string(),
            SinkRecord {
                sink_uid: uid.to_string(),
                sink_href: format!("/sink/{uid}.ics"),
                source_href: href.to_string(),
                source_etag: etag.to_string(),
            },
        );
    }

    fn hrefs(events: &[SourceEvent]) -> Vec<&str> {
        let mut v: Vec<&str> = events.iter().map(|e| e.href.as_str()).collect();
        v.sort();
        v
    }

    #[tokio::test]
    async fn test_fetch_changes_initial_sync_collection() {
        let server = MockServer::start().await;
        sync_report("<d:sync-token/>")
            .respond_with(multistatus(&format!(
                "{}<d:sync-token>tok-1</d:sync-token>",
                event("/cal/a.ics", "\"ea\"")
            )))
            .expect(1)
            .mount(&server)
            .await;

        let mut state = SyncState::default();
        let (events, removed) = CalDavSource::new(client(&server))
            .fetch_changes(&mut state)
            .await
            .unwrap();
        assert_eq!(state.sync_token.as_deref(), Some("tok-1"));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].href, "/cal/a.ics");
        assert_eq!(events[0].etag, "\"ea\"");
        assert_eq!(events[0].calendar_data, ICS);
        assert!(removed.is_empty());
    }

    #[tokio::test]
    async fn test_fetch_changes_incremental_with_pending_and_removed() {
        let server = MockServer::start().await;
        sync_report("<d:sync-token>tok-1</d:sync-token>")
            .respond_with(multistatus(&format!(
                "{}{}<d:response><d:href>/cal/gone.ics</d:href><d:status>HTTP/1.1 404 Not Found</d:status></d:response><d:sync-token>tok-2</d:sync-token>",
                event("/cal/a.ics", "\"ea2\""),
                etag_only("/cal/b.ics", "\"eb\"")
            )))
            .expect(1)
            .mount(&server)
            .await;
        multiget()
            .and(body_string_contains("<d:href>/cal/b.ics</d:href>"))
            .respond_with(multistatus(&event("/cal/b.ics", "\"eb\"")))
            .expect(1)
            .mount(&server)
            .await;

        let mut state = SyncState {
            sync_token: Some("tok-1".to_string()),
            ..Default::default()
        };
        let (events, removed) = CalDavSource::new(client(&server))
            .fetch_changes(&mut state)
            .await
            .unwrap();
        assert_eq!(state.sync_token.as_deref(), Some("tok-2"));
        assert_eq!(hrefs(&events), vec!["/cal/a.ics", "/cal/b.ics"]);
        assert_eq!(removed, vec!["/cal/gone.ics"]);
    }

    #[tokio::test]
    async fn test_fetch_changes_multiget_error_propagates() {
        let server = MockServer::start().await;
        sync_report("<d:sync-token/>")
            .respond_with(multistatus(&format!(
                "{}<d:sync-token>t</d:sync-token>",
                etag_only("/cal/b.ics", "\"eb\"")
            )))
            .mount(&server)
            .await;
        multiget()
            .respond_with(ResponseTemplate::new(400))
            .mount(&server)
            .await;

        let mut state = SyncState::default();
        let res = CalDavSource::new(client(&server))
            .fetch_changes(&mut state)
            .await;
        assert!(res.is_err());
    }

    #[tokio::test]
    async fn test_fetch_changes_multiget_error_keeps_old_token() {
        let server = MockServer::start().await;
        sync_report("<d:sync-token>tok-1</d:sync-token>")
            .respond_with(multistatus(&format!(
                "{}<d:sync-token>tok-2</d:sync-token>",
                etag_only("/cal/b.ics", "\"eb\"")
            )))
            .mount(&server)
            .await;
        multiget()
            .respond_with(ResponseTemplate::new(400))
            .mount(&server)
            .await;

        let source = CalDavSource::new(client(&server));
        let mut state = SyncState {
            sync_token: Some("tok-1".to_string()),
            ..Default::default()
        };
        assert!(source.fetch_changes(&mut state).await.is_err());
        assert_eq!(state.sync_token.as_deref(), Some("tok-1"));

        server.reset().await;
        sync_report("<d:sync-token>tok-1</d:sync-token>")
            .respond_with(multistatus(&format!(
                "{}<d:sync-token>tok-2</d:sync-token>",
                etag_only("/cal/b.ics", "\"eb\"")
            )))
            .expect(1)
            .mount(&server)
            .await;
        multiget()
            .respond_with(multistatus(&event("/cal/b.ics", "\"eb\"")))
            .expect(1)
            .mount(&server)
            .await;

        let (events, _) = source.fetch_changes(&mut state).await.unwrap();
        assert_eq!(hrefs(&events), vec!["/cal/b.ics"]);
        assert_eq!(state.sync_token.as_deref(), Some("tok-2"));
    }

    #[tokio::test]
    async fn test_fetch_changes_expired_token_restarts_full_sync() {
        let server = MockServer::start().await;
        sync_report("<d:sync-token>stale</d:sync-token>")
            .respond_with(ResponseTemplate::new(403))
            .expect(1)
            .mount(&server)
            .await;
        sync_report("<d:sync-token/>")
            .respond_with(multistatus(&format!(
                "{}{}<d:sync-token>fresh</d:sync-token>",
                event("/cal/a.ics", "\"ea\""),
                etag_only("/cal/b.ics", "\"eb\"")
            )))
            .expect(1)
            .mount(&server)
            .await;
        multiget()
            .respond_with(multistatus(&event("/cal/b.ics", "\"eb\"")))
            .expect(1)
            .mount(&server)
            .await;

        let mut state = SyncState {
            sync_token: Some("stale".to_string()),
            ..Default::default()
        };
        let (events, removed) = CalDavSource::new(client(&server))
            .fetch_changes(&mut state)
            .await
            .unwrap();
        assert_eq!(state.sync_token.as_deref(), Some("fresh"));
        assert_eq!(hrefs(&events), vec!["/cal/a.ics", "/cal/b.ics"]);
        assert!(removed.is_empty());
    }

    #[tokio::test]
    async fn test_fetch_changes_expired_token_and_failed_retry_falls_back_to_ctag() {
        let server = MockServer::start().await;
        Mock::given(method("REPORT"))
            .and(body_string_contains("sync-collection"))
            .respond_with(ResponseTemplate::new(409))
            .expect(2)
            .mount(&server)
            .await;
        ctag_props("\"c1\"").expect(1).mount(&server).await;
        list_events(&etag_only("/cal/a.ics", "\"ea\""))
            .expect(1)
            .mount(&server)
            .await;
        multiget()
            .respond_with(multistatus(&event("/cal/a.ics", "\"ea\"")))
            .expect(1)
            .mount(&server)
            .await;

        let mut state = SyncState {
            sync_token: Some("stale".to_string()),
            ..Default::default()
        };
        let (events, _) = CalDavSource::new(client(&server))
            .fetch_changes(&mut state)
            .await
            .unwrap();
        assert_eq!(state.sync_token, None);
        assert_eq!(state.ctag.as_deref(), Some("\"c1\""));
        assert_eq!(hrefs(&events), vec!["/cal/a.ics"]);
    }

    #[tokio::test]
    async fn test_fetch_changes_ctag_unchanged_short_circuits() {
        let server = MockServer::start().await;
        Mock::given(method("REPORT"))
            .respond_with(ResponseTemplate::new(501))
            .mount(&server)
            .await;
        ctag_props("\"c1\"").expect(1).mount(&server).await;
        list_events("").expect(0).mount(&server).await;

        let mut state = SyncState {
            ctag: Some("\"c1\"".to_string()),
            ..Default::default()
        };
        known(&mut state, "u1", "/cal/a.ics", "\"ea\"");
        let (events, removed) = CalDavSource::new(client(&server))
            .fetch_changes(&mut state)
            .await
            .unwrap();
        assert!(events.is_empty() && removed.is_empty());
    }

    #[tokio::test]
    async fn test_fetch_changes_etag_diff_detects_changed_new_and_removed() {
        let server = MockServer::start().await;
        Mock::given(method("REPORT"))
            .and(body_string_contains("sync-collection"))
            .respond_with(ResponseTemplate::new(501))
            .mount(&server)
            .await;
        ctag_props("\"c2\"").mount(&server).await;
        list_events(&format!(
            "<d:response><d:href>/cal/</d:href></d:response>{}{}{}",
            etag_only("/cal/same.ics", "\"s1\""),
            etag_only("/cal/changed.ics", "\"c-new\""),
            etag_only("/cal/new.ics", "\"n1\"")
        ))
        .mount(&server)
        .await;
        multiget()
            .respond_with(multistatus(&format!(
                "{}{}",
                event("/cal/changed.ics", "\"c-new\""),
                event("/cal/new.ics", "\"n1\"")
            )))
            .expect(1)
            .mount(&server)
            .await;

        let mut state = SyncState {
            ctag: Some("\"c1\"".to_string()),
            ..Default::default()
        };
        known(&mut state, "u1", "/cal/same.ics", "\"s1\"");
        known(&mut state, "u2", "/cal/changed.ics", "\"c-old\"");
        known(&mut state, "u3", "/cal/deleted.ics", "\"d1\"");

        let (events, removed) = CalDavSource::new(client(&server))
            .fetch_changes(&mut state)
            .await
            .unwrap();
        assert_eq!(hrefs(&events), vec!["/cal/changed.ics", "/cal/new.ics"]);
        assert_eq!(removed, vec!["/cal/deleted.ics"]);
        assert_eq!(state.ctag.as_deref(), Some("\"c2\""));

        let reqs = server.received_requests().await.unwrap();
        let mg = reqs
            .iter()
            .map(|r| String::from_utf8_lossy(&r.body).to_string())
            .find(|b| b.contains("calendar-multiget"))
            .unwrap();
        assert!(!mg.contains("same.ics"), "{mg}");
    }

    #[tokio::test]
    async fn test_fetch_changes_ctag_changed_without_etag_changes_skips_multiget() {
        let server = MockServer::start().await;
        Mock::given(method("REPORT"))
            .and(body_string_contains("sync-collection"))
            .respond_with(ResponseTemplate::new(501))
            .mount(&server)
            .await;
        ctag_props("\"c2\"").mount(&server).await;
        list_events(&etag_only("/cal/a.ics", "\"ea\""))
            .mount(&server)
            .await;
        multiget()
            .respond_with(multistatus(""))
            .expect(0)
            .mount(&server)
            .await;

        let mut state = SyncState {
            ctag: Some("\"c1\"".to_string()),
            ..Default::default()
        };
        known(&mut state, "u1", "/cal/a.ics", "\"ea\"");
        let (events, removed) = CalDavSource::new(client(&server))
            .fetch_changes(&mut state)
            .await
            .unwrap();
        assert!(events.is_empty() && removed.is_empty());
        assert_eq!(state.ctag.as_deref(), Some("\"c2\""));
    }

    #[tokio::test]
    async fn test_fetch_changes_ctag_fallback_propfind_error() {
        let server = MockServer::start().await;
        Mock::given(method("REPORT"))
            .respond_with(ResponseTemplate::new(501))
            .mount(&server)
            .await;
        Mock::given(method("PROPFIND"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;

        let mut state = SyncState::default();
        let res = CalDavSource::new(client(&server))
            .fetch_changes(&mut state)
            .await;
        assert!(res.is_err());
    }

    #[tokio::test]
    async fn test_sink_lists_events_of_this_sync() {
        let server = MockServer::start().await;
        let synced = |href: &str, extra: &str| {
            format!(
                r#"<d:response><d:href>{href}</d:href><d:propstat><d:prop>
<d:getetag>"e"</d:getetag><c:calendar-data>BEGIN:VCALENDAR
BEGIN:VEVENT
UID:sink-1
{extra}END:VEVENT
END:VCALENDAR</c:calendar-data></d:prop></d:propstat></d:response>"#
            )
        };
        list_events(&format!(
            "{}{}{}",
            etag_only("/cal/a.ics", "\"e\""),
            etag_only("/cal/b.ics", "\"e\""),
            etag_only("/cal/c.ics", "\"e\"")
        ))
        .mount(&server)
        .await;
        multiget()
            .respond_with(multistatus(&format!(
                "{}{}{}",
                synced(
                    "/cal/a.ics",
                    "X-CAPYCAL-SOURCE:work/src-1\nX-CAPYCAL-SYNC:s\n"
                ),
                synced("/cal/b.ics", "X-CAPYCAL-SYNC:s\n"),
                synced(
                    "/cal/c.ics",
                    "X-CAPYCAL-SOURCE:work/src-3\nX-CAPYCAL-SYNC:other\n"
                )
            )))
            .expect(1)
            .mount(&server)
            .await;

        let events = CalDavSink::new(client(&server))
            .list_synced("s", "work")
            .await
            .unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].href, "/cal/a.ics");
        assert_eq!(events[0].uid.as_deref(), Some("sink-1"));
        assert_eq!(events[0].source_uid.as_deref(), Some("src-1"));
        assert!(events[0].stored.contains("UID:sink-1"));
        assert_eq!(events[1].href, "/cal/b.ics");
        assert_eq!(events[1].source_uid, None);
    }

    #[tokio::test]
    async fn test_sink_put_and_delete() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/cal/x.ics"))
            .respond_with(ResponseTemplate::new(201).insert_header("ETag", "\"p1\""))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/cal/x.ics"))
            .respond_with(ResponseTemplate::new(404))
            .expect(1)
            .mount(&server)
            .await;

        let sink = CalDavSink::new(client(&server));
        let res = sink.put_event("/cal/x.ics", ICS).await.unwrap();
        assert_eq!(res.href, "/cal/x.ics");
        assert_eq!(res.etag, "\"p1\"");
        sink.delete_event("/cal/x.ics").await.unwrap();
    }
}
