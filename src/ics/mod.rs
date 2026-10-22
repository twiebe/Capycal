//! Read-only iCalendar feeds (`ics_url`): the whole calendar in one GET,
//! split into one source event per UID.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use reqwest::{Client, StatusCode, header};
use tracing::debug;

use crate::error::{Error, Result};
use crate::ical;
use crate::source::{EventSource, SourceEvent};
use crate::state::SyncState;
use crate::sync::diff::EtagDiff;

/// Prefixes of the HTTP validator kept in `SyncState::ctag`.
const ETAG: &str = "etag:";
const MODIFIED: &str = "modified:";

pub struct IcsSource {
    client: Client,
    url: String,
    name: String,
}

impl IcsSource {
    /// `name` is the calendar's key, for logs: the address of a private feed
    /// is a secret and never logged.
    pub fn new(name: &str, url: &str) -> Result<Self> {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .timeout(Duration::from_secs(300))
            .user_agent(concat!("capycal/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| Error::Feed(format!("failed to create HTTP client: {e}")))?;
        Ok(Self {
            client,
            url: https_url(url),
            name: name.to_string(),
        })
    }
}

/// `webcal://` and `webcals://` are what calendar sites link to; both are
/// plain HTTPS.
fn https_url(url: &str) -> String {
    match url
        .strip_prefix("webcals://")
        .or_else(|| url.strip_prefix("webcal://"))
    {
        Some(rest) => format!("https://{rest}"),
        None => url.to_string(),
    }
}

impl EventSource for IcsSource {
    /// Fetches the feed unless the server says it did not change since the
    /// validator in `state.ctag`, and compares each event's content hash
    /// with the synced one. An empty state fetches it in any case.
    async fn fetch_changes(
        &self,
        state: &mut SyncState,
    ) -> Result<(Vec<SourceEvent>, Vec<String>)> {
        let mut request = self.client.get(&self.url);
        match state.ctag.as_deref() {
            Some(c) if c.starts_with(ETAG) => {
                request = request.header(header::IF_NONE_MATCH, &c[ETAG.len()..]);
            }
            Some(c) if c.starts_with(MODIFIED) => {
                request = request.header(header::IF_MODIFIED_SINCE, &c[MODIFIED.len()..]);
            }
            _ => {}
        }
        // Without the URL, which reqwest's errors would carry
        let response = request.send().await.map_err(|e| e.without_url())?;
        if response.status() == StatusCode::NOT_MODIFIED {
            debug!(calendar = %self.name, "feed not modified");
            return Ok((vec![], vec![]));
        }
        if !response.status().is_success() {
            return Err(Error::Feed(format!(
                "calendar '{}': the feed answered {}",
                self.name,
                response.status()
            )));
        }
        let validator = |name, prefix: &str| {
            response
                .headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(|v| format!("{prefix}{v}"))
        };
        let cursor =
            validator(header::ETAG, ETAG).or_else(|| validator(header::LAST_MODIFIED, MODIFIED));
        let body = response.text().await.map_err(|e| e.without_url())?;
        let events =
            split_feed(&body).map_err(|e| Error::Feed(format!("calendar '{}': {e}", self.name)))?;

        let known = state.uid_mapping.source_etags();
        let events = keep_uids(events, &known);
        let mut diff = EtagDiff::new(known);
        let total = events.len();
        let changed: Vec<SourceEvent> = events
            .into_iter()
            .filter(|e| diff.observe(&e.href, &e.etag))
            .collect();
        let removed = diff.removed();
        debug!(
            calendar = %self.name,
            total,
            changed = changed.len(),
            removed = removed.len(),
            "feed read"
        );
        state.ctag = cursor;
        Ok((changed, removed))
    }
}

/// Some feeds are generated on each request with new random UIDs. An event
/// with a new UID takes the UID of a synced event that is gone from the
/// feed and had the same content, so it counts as unchanged instead of a
/// new copy plus a removal.
fn keep_uids(mut events: Vec<SourceEvent>, known: &HashMap<&str, &str>) -> Vec<SourceEvent> {
    let present: HashSet<&str> = events.iter().map(|e| e.href.as_str()).collect();
    let mut gone: Vec<(&str, &str)> = known
        .iter()
        .filter(|(href, _)| !present.contains(*href))
        .map(|(&href, &etag)| (href, etag))
        .collect();
    gone.sort_unstable();
    let mut vacant: HashMap<&str, Vec<&str>> = HashMap::new();
    for (href, etag) in gone.into_iter().rev() {
        vacant.entry(etag).or_default().push(href);
    }
    for event in &mut events {
        if known.contains_key(event.href.as_str()) {
            continue;
        }
        let Some(old) = vacant.get_mut(event.etag.as_str()).and_then(Vec::pop) else {
            continue;
        };
        event.calendar_data = event
            .calendar_data
            .split("\r\n")
            .map(|line| match line.strip_prefix("UID:") {
                Some(uid) if uid.trim() == event.uid => format!("UID:{old}"),
                _ => line.to_string(),
            })
            .collect::<Vec<_>>()
            .join("\r\n");
        event.uid = old.to_string();
        event.href = old.to_string();
    }
    events
}

/// One `SourceEvent` per UID of an iCalendar feed: the VEVENTs with that
/// UID (a series and its changed occurrences) and the VTIMEZONEs they
/// name. The href is the UID; the etag a hash of the content without
/// `DTSTAMP`, which feeds often set to the time of the request, and without
/// the UIDs, for `keep_uids`.
fn split_feed(body: &str) -> std::result::Result<Vec<SourceEvent>, String> {
    let text = ical::unfold(body);
    if !text.lines().any(|line| line.trim() == "BEGIN:VCALENDAR") {
        return Err("the address does not return an iCalendar file".to_string());
    }

    let mut zones: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut events: Vec<(&str, Vec<&str>)> = Vec::new();
    let mut by_uid: HashMap<&str, usize> = HashMap::new();
    let mut without_uid = 0usize;

    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        if line != "BEGIN:VEVENT" && line != "BEGIN:VTIMEZONE" {
            continue;
        }
        let mut block = vec![line];
        let mut depth = 0usize;
        let mut id = None;
        for line in lines.by_ref() {
            block.push(line);
            if line.starts_with("BEGIN:") {
                depth += 1;
            } else if line.starts_with("END:") {
                if depth == 0 {
                    break;
                }
                depth -= 1;
            } else if depth == 0 && id.is_none() {
                id = line
                    .strip_prefix("UID:")
                    .or_else(|| line.strip_prefix("TZID:"))
                    .map(str::trim);
            }
        }
        match (line, id) {
            ("BEGIN:VTIMEZONE", Some(tzid)) => {
                zones.insert(tzid, block);
            }
            ("BEGIN:VEVENT", Some(uid)) => match by_uid.get(uid) {
                Some(&i) => events[i].1.extend(block),
                None => {
                    by_uid.insert(uid, events.len());
                    events.push((uid, block));
                }
            },
            ("BEGIN:VEVENT", None) => without_uid += 1,
            _ => {}
        }
    }
    if without_uid > 0 {
        debug!(count = without_uid, "skipped feed events without UID");
    }

    Ok(events
        .into_iter()
        .map(|(uid, vevents)| {
            let prodid = format!("PRODID:{}", ical::PRODID);
            let mut out = vec!["BEGIN:VCALENDAR", "VERSION:2.0", prodid.as_str()];
            let mut named: Vec<&str> = vevents
                .iter()
                .filter_map(|l| ical::extract_tzid(l))
                .collect();
            named.sort_unstable();
            named.dedup();
            for tzid in named {
                if let Some(zone) = zones.get(tzid) {
                    out.extend(zone);
                }
            }
            out.extend(&vevents);
            out.push("END:VCALENDAR");
            let hashed: Vec<&str> = out
                .iter()
                .filter(|l| !matches!(ical::property_name(l), "DTSTAMP" | "UID"))
                .copied()
                .collect();
            let etag = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, hashed.join("\n").as_bytes());
            SourceEvent {
                uid: uid.to_string(),
                href: uid.to_string(),
                etag: etag.to_string(),
                calendar_data: out.join("\r\n") + "\r\n",
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::mapping::SinkRecord;
    use wiremock::matchers::{header, headers, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const FEED: &str = "BEGIN:VCALENDAR\r\n\
        VERSION:2.0\r\n\
        PRODID:-//Google Inc//Google Calendar 70.9054//EN\r\n\
        X-WR-CALNAME:Work\r\n\
        BEGIN:VTIMEZONE\r\n\
        TZID:Europe/Berlin\r\n\
        BEGIN:STANDARD\r\n\
        TZOFFSETFROM:+0200\r\n\
        TZOFFSETTO:+0100\r\n\
        DTSTART:19701025T030000\r\n\
        END:STANDARD\r\n\
        END:VTIMEZONE\r\n\
        BEGIN:VTIMEZONE\r\n\
        TZID:America/New_York\r\n\
        END:VTIMEZONE\r\n\
        BEGIN:VEVENT\r\n\
        DTSTART;TZID=Europe/Berlin:20260105T090000\r\n\
        DTEND;TZID=Europe/Berlin:20260105T093000\r\n\
        RRULE:FREQ=WEEKLY\r\n\
        DTSTAMP:20261018T120000Z\r\n\
        UID:standup@google.com\r\n\
        SUMMARY:Stand-up with a long title that the server folded across t\r\n \
        wo lines\r\n\
        BEGIN:VALARM\r\n\
        ACTION:DISPLAY\r\n\
        UID:alarm-1\r\n\
        TRIGGER:-PT10M\r\n\
        END:VALARM\r\n\
        END:VEVENT\r\n\
        BEGIN:VEVENT\r\n\
        DTSTART:20260110T100000Z\r\n\
        DTEND:20260110T110000Z\r\n\
        DTSTAMP:20261018T120000Z\r\n\
        UID:dentist@google.com\r\n\
        SUMMARY:Dentist\r\n\
        END:VEVENT\r\n\
        BEGIN:VEVENT\r\n\
        DTSTART;TZID=Europe/Berlin:20260112T100000\r\n\
        RECURRENCE-ID;TZID=Europe/Berlin:20260112T090000\r\n\
        DTSTAMP:20261018T120000Z\r\n\
        UID:standup@google.com\r\n\
        SUMMARY:Stand-up (moved)\r\n\
        END:VEVENT\r\n\
        BEGIN:VEVENT\r\n\
        DTSTART:20260111T100000Z\r\n\
        SUMMARY:No UID\r\n\
        END:VEVENT\r\n\
        END:VCALENDAR\r\n";

    #[test]
    fn test_split_feed_groups_by_uid_with_their_timezones() {
        let events = split_feed(FEED).unwrap();
        assert_eq!(events.len(), 2);
        let standup = &events[0];
        assert_eq!(standup.href, "standup@google.com");
        assert_eq!(standup.uid, "standup@google.com");
        let data = &standup.calendar_data;
        assert!(data.starts_with("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//capycal//EN\r\n"));
        assert!(data.ends_with("END:VCALENDAR\r\n"));
        assert!(data.contains("TZID:Europe/Berlin\r\nBEGIN:STANDARD"));
        assert!(!data.contains("America/New_York"));
        assert!(!data.contains("X-WR-CALNAME"));
        assert!(data.contains("across two lines"));
        assert!(data.contains("SUMMARY:Stand-up (moved)"));
        assert_eq!(data.matches("BEGIN:VEVENT").count(), 2);
        assert!(data.contains("UID:alarm-1"), "the alarm stays in its event");
        assert_eq!(
            ical::extract_uid(data).as_deref(),
            Some("standup@google.com")
        );

        let dentist = &events[1].calendar_data;
        assert!(!dentist.contains("VTIMEZONE"));
        assert!(dentist.contains("SUMMARY:Dentist"));
    }

    #[test]
    fn test_etag_ignores_dtstamp_but_follows_content() {
        let before = split_feed(FEED).unwrap();
        let restamped =
            split_feed(&FEED.replace("DTSTAMP:20261018T120000Z", "DTSTAMP:20261019T080000Z"))
                .unwrap();
        assert_eq!(before[0].etag, restamped[0].etag);
        let edited = split_feed(&FEED.replace("SUMMARY:Dentist", "SUMMARY:Doctor")).unwrap();
        assert_eq!(before[0].etag, edited[0].etag);
        assert_ne!(before[1].etag, edited[1].etag);
    }

    #[test]
    fn test_split_feed_rejects_other_content() {
        let err = split_feed("<!DOCTYPE html><html>Sign in</html>").unwrap_err();
        assert!(err.contains("not return an iCalendar file"), "{err}");
        assert!(
            split_feed("BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn test_webcal_is_https() {
        assert_eq!(
            https_url("webcal://example.com/a.ics"),
            "https://example.com/a.ics"
        );
        assert_eq!(
            https_url("webcals://example.com/a.ics"),
            "https://example.com/a.ics"
        );
        assert_eq!(
            https_url("http://example.com/a.ics"),
            "http://example.com/a.ics"
        );
    }

    fn synced(state: &mut SyncState, event: &SourceEvent) {
        state.uid_mapping.insert(
            event.uid.clone(),
            SinkRecord {
                sink_uid: event.uid.clone(),
                sink_href: format!("/target/{}.ics", event.uid),
                source_href: event.href.clone(),
                source_etag: event.etag.clone(),
            },
        );
    }

    const REGENERATED: &str = "BEGIN:VCALENDAR\r\n\
        BEGIN:VEVENT\r\n\
        UID:@1\r\n\
        DTSTART:20260103T050000Z\r\n\
        SUMMARY:Paper\r\n\
        END:VEVENT\r\n\
        BEGIN:VEVENT\r\n\
        UID:@2\r\n\
        DTSTART:20260110T050000Z\r\n\
        SUMMARY:Glass\r\n\
        END:VEVENT\r\n\
        BEGIN:VEVENT\r\n\
        UID:@3\r\n\
        DTSTART:20260110T050000Z\r\n\
        SUMMARY:Glass\r\n\
        END:VEVENT\r\n\
        END:VCALENDAR\r\n";

    fn regenerated(prefix: &str) -> Vec<SourceEvent> {
        split_feed(&REGENERATED.replace('@', prefix)).unwrap()
    }

    #[test]
    fn test_new_uids_for_the_same_events_keep_the_synced_uids() {
        let mut state = SyncState::default();
        for event in &regenerated("a") {
            synced(&mut state, event);
        }
        let known = state.uid_mapping.source_etags();

        let events = keep_uids(regenerated("b"), &known);
        let mut hrefs: Vec<&str> = events.iter().map(|e| e.href.as_str()).collect();
        hrefs.sort_unstable();
        assert_eq!(hrefs, ["a1", "a2", "a3"]);
        for event in &events {
            assert_eq!(event.uid, event.href);
            assert_eq!(
                ical::extract_uid(&event.calendar_data).as_deref(),
                Some(event.uid.as_str())
            );
        }
        let mut diff = EtagDiff::new(known.clone());
        assert!(!events.iter().any(|e| diff.observe(&e.href, &e.etag)));
        assert!(diff.removed().is_empty());

        // A changed event is new, its old copy goes
        let moved = REGENERATED.replace("SUMMARY:Paper", "SUMMARY:Bio");
        let events = keep_uids(split_feed(&moved.replace('@', "c")).unwrap(), &known);
        let mut diff = EtagDiff::new(known);
        let changed: Vec<&str> = events
            .iter()
            .filter(|e| diff.observe(&e.href, &e.etag))
            .map(|e| e.href.as_str())
            .collect();
        assert_eq!(changed, ["c1"]);
        assert_eq!(diff.removed(), ["a1"]);
    }

    #[tokio::test]
    async fn test_fetch_reports_changed_and_removed_and_keeps_the_etag() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/basic.ics"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("ETag", "\"v2\"")
                    .set_body_string(FEED.replace("SUMMARY:Dentist", "SUMMARY:Doctor")),
            )
            .mount(&server)
            .await;
        let source = IcsSource::new("google", &format!("{}/basic.ics", server.uri())).unwrap();

        let events = split_feed(FEED).unwrap();
        let mut state = SyncState::default();
        synced(&mut state, &events[0]);
        synced(&mut state, &events[1]);
        synced(
            &mut state,
            &SourceEvent {
                uid: "gone@google.com".into(),
                href: "gone@google.com".into(),
                etag: "x".into(),
                calendar_data: String::new(),
            },
        );

        let (changed, removed) = source.fetch_changes(&mut state).await.unwrap();
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].href, "dentist@google.com");
        assert_eq!(removed, vec!["gone@google.com"]);
        assert_eq!(state.ctag.as_deref(), Some("etag:\"v2\""));
    }

    #[tokio::test]
    async fn test_fetch_sends_the_validator_and_takes_not_modified_as_no_change() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(header("If-None-Match", "\"v1\""))
            .respond_with(ResponseTemplate::new(304))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            // wiremock splits header values at commas
            .and(headers(
                "If-Modified-Since",
                vec!["Sun", "18 Oct 2026 12:00:00 GMT"],
            ))
            .respond_with(ResponseTemplate::new(304))
            .expect(1)
            .mount(&server)
            .await;
        let source = IcsSource::new("google", &format!("{}/basic.ics", server.uri())).unwrap();

        for ctag in ["etag:\"v1\"", "modified:Sun, 18 Oct 2026 12:00:00 GMT"] {
            let mut state = SyncState {
                ctag: Some(ctag.to_string()),
                ..Default::default()
            };
            let (changed, removed) = source.fetch_changes(&mut state).await.unwrap();
            assert!(changed.is_empty() && removed.is_empty());
            assert_eq!(state.ctag.as_deref(), Some(ctag));
        }
    }

    #[tokio::test]
    async fn test_fetch_error_names_the_calendar_but_not_the_address() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let url = format!("{}/private-secret123/basic.ics", server.uri());
        let source = IcsSource::new("google", &url).unwrap();
        let err = source
            .fetch_changes(&mut SyncState::default())
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("calendar 'google'") && err.contains("404"),
            "{err}"
        );
        assert!(!err.contains("secret123"), "{err}");

        let source =
            IcsSource::new("google", "http://127.0.0.1:1/private-secret123/basic.ics").unwrap();
        let err = source
            .fetch_changes(&mut SyncState::default())
            .await
            .unwrap_err()
            .to_string();
        assert!(!err.contains("secret123"), "{err}");
    }
}
