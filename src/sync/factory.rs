use crate::caldav::auth::ResolvedAuth;
use crate::caldav::client::CalDavClient;
use crate::caldav::source::{CalDavSink, CalDavSource};
use crate::config::{CalendarConfig, Config, SyncConfig};
use crate::error::{Error, Result};
use crate::source::{EventSink, EventSource, PutResult, SinkChanges, SinkEvent, SourceEvent};
use crate::state::SyncState;

/// Any supported event source. Add new backends here and in `build_source`.
pub enum AnySource {
    CalDav(CalDavSource),
    #[cfg(target_os = "macos")]
    EventKit(crate::eventkit::EventKitClient),
    Ics(crate::ics::IcsSource),
}

/// Any supported event sink. Add new backends here and in `build_sink`.
#[derive(Clone)]
pub enum AnySink {
    CalDav(CalDavSink),
    #[cfg(target_os = "macos")]
    EventKit(crate::eventkit::EventKitClient),
}

/// What an `AnySink` keeps of a stored event, per backend.
pub enum AnyStored {
    CalDav(String),
    #[cfg(target_os = "macos")]
    EventKit(crate::eventkit::convert::StoredEvent),
}

impl EventSource for AnySource {
    async fn fetch_changes(
        &self,
        state: &mut SyncState,
    ) -> Result<(Vec<SourceEvent>, Vec<String>)> {
        match self {
            Self::CalDav(s) => s.fetch_changes(state).await,
            #[cfg(target_os = "macos")]
            Self::EventKit(s) => s.fetch_changes(state).await,
            Self::Ics(s) => s.fetch_changes(state).await,
        }
    }
}

impl EventSink for AnySink {
    type Stored = AnyStored;

    async fn put_event(&self, href: &str, ical_data: &str) -> Result<PutResult> {
        match self {
            Self::CalDav(s) => s.put_event(href, ical_data).await,
            #[cfg(target_os = "macos")]
            Self::EventKit(s) => s.put_event(href, ical_data).await,
        }
    }

    async fn delete_event(&self, href: &str) -> Result<()> {
        match self {
            Self::CalDav(s) => s.delete_event(href).await,
            #[cfg(target_os = "macos")]
            Self::EventKit(s) => s.delete_event(href).await,
        }
    }

    async fn list_synced(
        &self,
        sync_name: &str,
        source_name: &str,
    ) -> Result<Vec<SinkEvent<AnyStored>>> {
        fn wrap<S>(events: Vec<SinkEvent<S>>, f: fn(S) -> AnyStored) -> Vec<SinkEvent<AnyStored>> {
            events
                .into_iter()
                .map(|e| SinkEvent {
                    href: e.href,
                    uid: e.uid,
                    source_uid: e.source_uid,
                    stored: f(e.stored),
                })
                .collect()
        }
        Ok(match self {
            Self::CalDav(s) => wrap(
                s.list_synced(sync_name, source_name).await?,
                AnyStored::CalDav,
            ),
            #[cfg(target_os = "macos")]
            Self::EventKit(s) => wrap(
                s.list_synced(sync_name, source_name).await?,
                AnyStored::EventKit,
            ),
        })
    }

    fn is_unchanged(&self, stored: &AnyStored, ical_data: &str) -> bool {
        match (self, stored) {
            (Self::CalDav(s), AnyStored::CalDav(stored)) => s.is_unchanged(stored, ical_data),
            #[cfg(target_os = "macos")]
            (Self::EventKit(s), AnyStored::EventKit(stored)) => s.is_unchanged(stored, ical_data),
            #[cfg(target_os = "macos")]
            _ => false,
        }
    }

    async fn changes_since(&self, token: Option<&str>) -> Result<Option<SinkChanges>> {
        match self {
            Self::CalDav(s) => s.changes_since(token).await,
            #[cfg(target_os = "macos")]
            Self::EventKit(s) => s.changes_since(token).await,
        }
    }

    fn cancel_safe(&self) -> bool {
        match self {
            Self::CalDav(s) => s.cancel_safe(),
            #[cfg(target_os = "macos")]
            Self::EventKit(s) => s.cancel_safe(),
        }
    }
}

/// Build the source adapter for a sync from its source calendar config.
pub fn build_source(config: &Config, sync: &SyncConfig) -> Result<AnySource> {
    let cal = calendar(config, &sync.source)?;
    if let Some(feed) = &cal.ics_url {
        let url = feed.resolve(
            &format!("calendar '{}'", sync.source),
            config.config_dir.as_deref(),
        )?;
        return Ok(AnySource::Ics(crate::ics::IcsSource::new(
            &sync.source,
            &url,
        )?));
    }
    if cal.is_eventkit() {
        #[cfg(target_os = "macos")]
        return Ok(AnySource::EventKit(crate::eventkit::EventKitClient::new(
            cal.eventkit_calendar.as_deref().unwrap(),
            sync.days_past,
            sync.days_future,
            sync.all_events(cal),
        )?));
        #[cfg(not(target_os = "macos"))]
        return Err(eventkit_unsupported());
    }
    Ok(AnySource::CalDav(CalDavSource::new(caldav_client(
        config,
        &sync.source,
    )?)))
}

/// Build the sink adapter for the calendar named `cal_name`.
pub fn build_sink(config: &Config, cal_name: &str) -> Result<AnySink> {
    let cal = calendar(config, cal_name)?;
    if cal.is_ics() {
        return Err(read_only_feed(cal_name));
    }
    if cal.is_eventkit() {
        // The query range options only affect fetch_changes, never put/delete
        #[cfg(target_os = "macos")]
        return Ok(AnySink::EventKit(crate::eventkit::EventKitClient::new(
            cal.eventkit_calendar.as_deref().unwrap(),
            0,
            0,
            false,
        )?));
        #[cfg(not(target_os = "macos"))]
        return Err(eventkit_unsupported());
    }
    Ok(AnySink::CalDav(CalDavSink::new(caldav_client(
        config, cal_name,
    )?)))
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn eventkit_unsupported() -> Error {
    Error::Config("eventkit_calendar is only supported on macOS".to_string())
}

pub(crate) fn read_only_feed(cal_name: &str) -> Error {
    Error::Config(format!(
        "calendar '{cal_name}' is a read-only feed (ics_url); Capycal never writes to it"
    ))
}

fn calendar<'a>(config: &'a Config, name: &str) -> Result<&'a CalendarConfig> {
    config
        .calendars
        .get(name)
        .ok_or_else(|| Error::Config(format!("unknown calendar '{name}'")))
}

/// Build a CalDAV client for the calendar named `cal_name`, resolving the
/// credentials of its account.
pub fn caldav_client(config: &Config, cal_name: &str) -> Result<CalDavClient> {
    let cal = calendar(config, cal_name)?;
    let auth = resolve_calendar_auth(config, cal, cal_name)?;
    CalDavClient::new(cal.url_or_err(cal_name)?, auth)
}

fn resolve_calendar_auth(
    config: &Config,
    cal: &CalendarConfig,
    cal_name: &str,
) -> Result<ResolvedAuth> {
    let name = cal
        .account
        .as_deref()
        .ok_or_else(|| Error::Config(format!("calendar '{cal_name}': requires 'account'")))?;
    let account = config.accounts.get(name).ok_or_else(|| {
        Error::Config(format!(
            "calendar '{cal_name}' references unknown account '{name}'"
        ))
    })?;
    let password = account
        .password
        .resolve(&format!("account '{name}'"), config.config_dir.as_deref())?;
    Ok(ResolvedAuth {
        username: account.username.clone(),
        password,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn config(url: &str) -> Config {
        toml::from_str(&format!(
            r#"
[daemon]

[accounts.acc]
url = "{url}"
username = "u"
password = "p"

[calendars.work]
account = "acc"
url = "{url}"

[calendars.mac]
eventkit_calendar = "Home"

[calendars.noaccount]
account = "missing"
url = "{url}"

[syncs.work-to-mac]
source = "work"
target = "mac"
interval = "1h"
fields = ["DTSTART"]

[syncs.mac-to-work]
source = "mac"
target = "work"
interval = "1h"
fields = ["DTSTART"]
"#
        ))
        .unwrap()
    }

    #[test]
    fn test_build_caldav_source_and_sink() {
        let cfg = config("https://example.com/cal/");
        assert!(matches!(
            build_source(&cfg, &cfg.syncs[0]),
            Ok(AnySource::CalDav(_))
        ));
        assert!(matches!(build_sink(&cfg, "work"), Ok(AnySink::CalDav(_))));
    }

    #[test]
    fn test_build_feed_source_but_no_sink() {
        let mut cfg = config("https://example.com/cal/");
        cfg.calendars.insert(
            "feed".to_string(),
            toml::from_str("ics_url = \"webcal://example.com/basic.ics\"").unwrap(),
        );
        let mut sync = cfg.syncs[0].clone();
        sync.source = "feed".to_string();
        assert!(matches!(build_source(&cfg, &sync), Ok(AnySource::Ics(_))));
        let err = build_sink(&cfg, "feed").err().unwrap().to_string();
        assert!(err.contains("read-only feed"), "{err}");
    }

    #[test]
    fn test_build_caldav_unknown_account() {
        let cfg = config("https://example.com/cal/");
        let err = build_sink(&cfg, "noaccount").err().unwrap();
        assert!(
            err.to_string().contains("unknown account 'missing'"),
            "{err}"
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn test_build_eventkit_unsupported_off_macos() {
        let cfg = config("https://example.com/cal/");
        for err in [
            build_source(&cfg, &cfg.syncs[1]).err().unwrap(),
            build_sink(&cfg, "mac").err().unwrap(),
        ] {
            assert!(
                matches!(&err, Error::Config(m) if m == "eventkit_calendar is only supported on macOS")
            );
        }
    }

    #[tokio::test]
    async fn test_any_sink_delegates_to_caldav() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/cal/e1.ics"))
            .and(header("Authorization", "Basic dTpw"))
            .respond_with(ResponseTemplate::new(201).insert_header("ETag", "\"e1\""))
            .expect(1)
            .mount(&server)
            .await;
        let cfg = config(&format!("{}/cal/", server.uri()));
        let sink = build_sink(&cfg, "work").unwrap();
        let res = sink
            .put_event("/cal/e1.ics", "BEGIN:VCALENDAR\r\nEND:VCALENDAR")
            .await
            .unwrap();
        assert_eq!(res.href, "/cal/e1.ics");
    }
}
