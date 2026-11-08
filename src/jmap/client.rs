use std::sync::Arc;
use std::time::Duration;

use reqwest::{Client, StatusCode, Url};
use serde_json::{Map, Value, json};
use tokio::sync::OnceCell;
use tracing::debug;

use crate::caldav::auth::ResolvedAuth;
use crate::caldav::client::{CalDavClient, DiscoveredCalendar, RetryPolicy};
use crate::error::{Error, Result};

const CORE: &str = "urn:ietf:params:jmap:core";
const CALENDARS: &str = "urn:ietf:params:jmap:calendars";

/// Ids asked for at once when the server does not name its limit.
const DEFAULT_MAX_OBJECTS: usize = 256;
/// Ids of one `CalendarEvent/query` answer.
const QUERY_LIMIT: u64 = 1000;

#[derive(Debug)]
struct Session {
    api_url: String,
    account_id: String,
    max_objects_in_get: usize,
}

/// What changed in the account's events since a state.
#[derive(Debug, Default)]
pub struct Changes {
    pub changed: Vec<String>,
    pub destroyed: Vec<String>,
    pub state: String,
}

/// A JMAP calendar: `calendar` is its id or its name, read and resolved at the
/// first use and kept, like the session.
#[derive(Clone)]
pub struct JmapClient {
    http: Client,
    url: String,
    auth: Arc<ResolvedAuth>,
    calendar: String,
    session: Arc<OnceCell<Session>>,
    calendar_id: Arc<OnceCell<String>>,
    retry: RetryPolicy,
}

impl JmapClient {
    pub fn new(url: &str, auth: ResolvedAuth, calendar: &str) -> Result<Self> {
        let http = Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .timeout(Duration::from_secs(300))
            .user_agent(concat!("capycal/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| Error::Jmap(format!("failed to create HTTP client: {e}")))?;
        Ok(Self {
            http,
            url: url.to_string(),
            auth: Arc::new(auth),
            calendar: calendar.to_string(),
            session: Arc::new(OnceCell::new()),
            calendar_id: Arc::new(OnceCell::new()),
            retry: RetryPolicy::default(),
        })
    }

    /// The calendars of the account's primary calendar account.
    pub async fn discover_calendars(
        url: &str,
        username: &str,
        password: &str,
    ) -> Result<Vec<DiscoveredCalendar>> {
        let auth = ResolvedAuth {
            username: username.to_string(),
            password: password.to_string(),
        };
        let client = Self::new(url, auth, "")?;
        let list = client.calendars().await?;
        Ok(list
            .iter()
            .filter_map(|c| {
                let id = c["id"].as_str()?;
                let rights = &c["myRights"];
                Some(DiscoveredCalendar {
                    href: id.to_string(),
                    url: url.to_string(),
                    display_name: c["name"].as_str().map(str::to_string),
                    color: c["color"].as_str().map(str::to_string),
                    writable: rights.is_null()
                        || rights["mayWriteAll"].as_bool() == Some(true)
                        || rights["mayWriteOwn"].as_bool() == Some(true),
                    jmap_id: Some(id.to_string()),
                })
            })
            .collect())
    }

    async fn session(&self) -> Result<&Session> {
        self.session.get_or_try_init(|| self.fetch_session()).await
    }

    /// The account URL when it names a path, else (or if that is no session)
    /// RFC 8620's well-known address.
    async fn fetch_session(&self) -> Result<Session> {
        let base =
            Url::parse(&self.url).map_err(|e| Error::Jmap(format!("invalid account URL: {e}")))?;
        let well_known = base
            .join("/.well-known/jmap")
            .map_err(|e| Error::Jmap(format!("invalid account URL: {e}")))?;
        let mut candidates = vec![well_known];
        if base.path() != "/" {
            candidates.insert(0, base);
        }
        for url in &candidates {
            if let Some(session) = self.session_at(url).await? {
                return Ok(session);
            }
        }
        Err(Error::Jmap(format!(
            "no JMAP session at {}, check the server URL",
            candidates[0]
        )))
    }

    /// The session at `url`, None if there is none. A server that rejects
    /// the sign-in or has no calendars is an error: another address won't help.
    async fn session_at(&self, url: &Url) -> Result<Option<Session>> {
        let response = CalDavClient::send_with_retry_with_auth(&self.auth, self.retry, || {
            self.http.get(url.clone())
        })
        .await?;
        let status = response.status();
        let moved = response.url().origin() != url.origin();
        if moved && (status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN) {
            // The password is not sent to another host
            return Err(Error::Jmap(format!(
                "{url} sends the session to {}, use that as the server URL",
                response.url().origin().ascii_serialization()
            )));
        }
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(Error::Jmap(format!(
                "the server rejected the sign-in ({status})"
            )));
        }
        if !status.is_success() {
            debug!(%url, %status, "no JMAP session here");
            return Ok(None);
        }
        // Redirects count: the API is relative to where the session really is
        let at = response.url().clone();
        let Ok(body) = response.json::<Value>().await else {
            debug!(%url, "no JMAP session here");
            return Ok(None);
        };
        parse_session(&body, &at)
    }

    /// Run one method on the account and return its answer.
    async fn call(&self, method: &str, mut args: Value) -> Result<Value> {
        let session = self.session().await?;
        args["accountId"] = json!(session.account_id);
        let body = json!({
            "using": [CORE, CALENDARS],
            "methodCalls": [[method, args, "0"]],
        });
        let response = CalDavClient::send_with_retry_with_auth(&self.auth, self.retry, || {
            self.http.post(&session.api_url).json(&body)
        })
        .await?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(Error::Jmap(format!("{method}: {status} {text}")));
        }
        let mut answer: Value = response
            .json()
            .await
            .map_err(|e| Error::Jmap(format!("{method}: invalid answer: {e}")))?;
        let first = answer["methodResponses"][0].take();
        if first[0] == "error" {
            let kind = first[1]["type"].as_str().unwrap_or("unknown");
            // Stalwart calls a state it cannot read invalid arguments
            if method.ends_with("/changes")
                && matches!(kind, "cannotCalculateChanges" | "invalidArguments")
            {
                return Err(Error::SyncTokenExpired);
            }
            let description = first[1]["description"].as_str().unwrap_or_default();
            return Err(Error::Jmap(format!("{method}: {kind} {description}")));
        }
        if first[0] != method {
            return Err(Error::Jmap(format!("{method}: no answer to the call")));
        }
        Ok(first[1].clone())
    }

    async fn calendars(&self) -> Result<Vec<Value>> {
        let answer = self.call("Calendar/get", json!({ "ids": null })).await?;
        Ok(answer["list"].as_array().cloned().unwrap_or_default())
    }

    /// The id of this calendar, found by id, or by a name only one has.
    pub async fn calendar_id(&self) -> Result<&str> {
        self.calendar_id
            .get_or_try_init(|| async {
                let list = self.calendars().await?;
                let id_of = |c: &Value| c["id"].as_str().map(str::to_string);
                if let Some(id) = list.iter().filter_map(id_of).find(|i| *i == self.calendar) {
                    return Ok(id);
                }
                let named: Vec<&Value> = list
                    .iter()
                    .filter(|c| c["name"].as_str() == Some(self.calendar.as_str()))
                    .collect();
                match named[..] {
                    [one] => id_of(one).ok_or_else(|| Error::Jmap("calendar without id".into())),
                    [] => Err(Error::Jmap(format!(
                        "no calendar '{}' on the server, choose it again or check jmap_calendar",
                        self.calendar
                    ))),
                    _ => Err(Error::Jmap(format!(
                        "several calendars are named '{}', use the id",
                        self.calendar
                    ))),
                }
            })
            .await
            .map(String::as_str)
    }

    /// The account's current state of events.
    pub async fn state(&self) -> Result<String> {
        let answer = self.call("CalendarEvent/get", json!({ "ids": [] })).await?;
        text_of(&answer, "state")
    }

    /// The ids of every event in this calendar.
    pub async fn event_ids(&self) -> Result<Vec<String>> {
        let calendar = self.calendar_id().await?;
        let mut ids = Vec::new();
        loop {
            let answer = self
                .call(
                    "CalendarEvent/query",
                    json!({
                        "filter": { "inCalendar": calendar },
                        "position": ids.len(),
                        "limit": QUERY_LIMIT,
                    }),
                )
                .await?;
            let page = strings(&answer["ids"]);
            if page.is_empty() {
                return Ok(ids);
            }
            ids.extend(page);
        }
    }

    /// The events with these ids; the ones gone by now are left out.
    pub async fn events(&self, ids: &[String]) -> Result<Vec<Value>> {
        let session = self.session().await?;
        let mut events = Vec::with_capacity(ids.len());
        for chunk in ids.chunks(session.max_objects_in_get.max(1)) {
            let answer = self
                .call("CalendarEvent/get", json!({ "ids": chunk }))
                .await?;
            events.extend(answer["list"].as_array().cloned().unwrap_or_default());
        }
        Ok(events)
    }

    /// Every event in this calendar.
    pub async fn all_events(&self) -> Result<Vec<Value>> {
        self.events(&self.event_ids().await?).await
    }

    /// What changed since `state`, in any calendar of the account.
    pub async fn changes(&self, state: &str) -> Result<Changes> {
        let mut changes = Changes {
            state: state.to_string(),
            ..Changes::default()
        };
        loop {
            let answer = self
                .call(
                    "CalendarEvent/changes",
                    json!({ "sinceState": changes.state }),
                )
                .await?;
            changes.changed.extend(strings(&answer["created"]));
            changes.changed.extend(strings(&answer["updated"]));
            changes.destroyed.extend(strings(&answer["destroyed"]));
            changes.state = text_of(&answer, "newState")?;
            if answer["hasMoreChanges"].as_bool() != Some(true) {
                // A record created and updated since the state may be in both
                changes.changed.sort();
                changes.changed.dedup();
                changes.destroyed.sort();
                changes.destroyed.dedup();
                return Ok(changes);
            }
        }
    }

    /// The event with this UID in the account, in any calendar: UIDs are
    /// unique in a whole account on some servers.
    pub async fn find_uid(&self, uid: &str) -> Result<Option<String>> {
        let answer = self
            .call(
                "CalendarEvent/query",
                json!({ "filter": { "uid": uid }, "limit": 1 }),
            )
            .await?;
        Ok(strings(&answer["ids"]).into_iter().next())
    }

    /// Create an event in this calendar and return its id. Scheduling
    /// messages stay off, the JMAP default.
    pub async fn create(&self, mut event: Map<String, Value>) -> Result<(String, String)> {
        let calendar = self.calendar_id().await?;
        event.insert("calendarIds".to_string(), json!({ calendar: true }));
        let answer = self
            .call("CalendarEvent/set", json!({ "create": { "e": event } }))
            .await?;
        if let Some(created) = answer["created"].get("e") {
            return Ok((text_of(created, "id")?, stamp(created)));
        }
        Err(set_error("create", &answer["notCreated"]["e"]))
    }

    /// Change an event and put it into this calendar, false if it is gone.
    pub async fn update(&self, id: &str, mut patch: Map<String, Value>) -> Result<Option<String>> {
        let calendar = self.calendar_id().await?;
        patch.insert("calendarIds".to_string(), json!({ calendar: true }));
        let answer = self
            .call("CalendarEvent/set", json!({ "update": { id: patch } }))
            .await?;
        if let Some(updated) = answer["updated"].get(id) {
            return Ok(Some(stamp(updated)));
        }
        let failure = &answer["notUpdated"][id];
        if failure["type"] == "notFound" {
            return Ok(None);
        }
        Err(set_error("update", failure))
    }

    /// Delete an event; one already gone counts as deleted.
    pub async fn destroy(&self, id: &str) -> Result<()> {
        let answer = self
            .call("CalendarEvent/set", json!({ "destroy": [id] }))
            .await?;
        let failure = &answer["notDestroyed"][id];
        if failure.is_null() || failure["type"] == "notFound" {
            return Ok(());
        }
        Err(set_error("delete", failure))
    }
}

fn stamp(event: &Value) -> String {
    event["updated"].as_str().unwrap_or_default().to_string()
}

fn set_error(action: &str, failure: &Value) -> Error {
    let kind = failure["type"].as_str().unwrap_or("unknown");
    let description = failure["description"].as_str().unwrap_or_default();
    Error::Jmap(format!("cannot {action} the event: {kind} {description}"))
}

fn text_of(value: &Value, key: &str) -> Result<String> {
    value[key]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| Error::Jmap(format!("answer without {key}")))
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect()
}

fn parse_session(body: &Value, at: &Url) -> Result<Option<Session>> {
    let Some(api) = body["apiUrl"].as_str() else {
        return Ok(None);
    };
    if body["capabilities"].get(CALENDARS).is_none() {
        return Err(Error::Jmap(
            "this server has no calendars over JMAP, try CalDAV".to_string(),
        ));
    }
    let account_id = body["primaryAccounts"][CALENDARS].as_str().ok_or_else(|| {
        Error::Jmap("this account has no calendars over JMAP, try CalDAV".to_string())
    })?;
    Ok(Some(Session {
        api_url: same_origin(at, api)?,
        account_id: account_id.to_string(),
        max_objects_in_get: body["capabilities"][CORE]["maxObjectsInGet"]
            .as_u64()
            .map_or(DEFAULT_MAX_OBJECTS, |n| n as usize),
    }))
}

/// The API address of a session, on the origin the session came from: behind
/// a proxy or a port mapping servers name an address of their own that cannot
/// be reached, and the password only goes to the server of the account.
fn same_origin(session: &Url, api: &str) -> Result<String> {
    let mut url = session
        .join(api)
        .map_err(|e| Error::Jmap(format!("invalid apiUrl: {e}")))?;
    if url.origin() != session.origin() {
        debug!("the session names another address for its API, staying on the session's");
        let moved = url.set_scheme(session.scheme()).is_ok()
            && url.set_host(session.host_str()).is_ok()
            && url.set_port(session.port()).is_ok();
        if !moved {
            return Err(Error::Jmap("invalid apiUrl".to_string()));
        }
    }
    Ok(url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    pub(crate) fn session_json(server: &MockServer) -> Value {
        json!({
            "apiUrl": format!("{}/jmap/", server.uri()),
            "capabilities": {
                CORE: { "maxObjectsInGet": 2 },
                CALENDARS: {},
            },
            "primaryAccounts": { CALENDARS: "acc" },
        })
    }

    fn client(server: &MockServer, calendar: &str) -> JmapClient {
        JmapClient::new(
            &server.uri(),
            ResolvedAuth {
                username: "u".into(),
                password: "p".into(),
            },
            calendar,
        )
        .unwrap()
    }

    async fn mount_session(server: &MockServer) {
        Mock::given(method("GET"))
            .and(path("/.well-known/jmap"))
            .respond_with(ResponseTemplate::new(200).set_body_json(session_json(server)))
            .mount(server)
            .await;
    }

    fn answer(method: &str, args: Value) -> ResponseTemplate {
        ResponseTemplate::new(200)
            .set_body_json(json!({ "methodResponses": [[method, args, "0"]] }))
    }

    async fn mount_calendars(server: &MockServer) {
        Mock::given(method("POST"))
            .and(body_string_contains("Calendar/get"))
            .respond_with(answer(
                "Calendar/get",
                json!({ "list": [
                    { "id": "c1", "name": "Work", "myRights": { "mayWriteAll": true } },
                    { "id": "c2", "name": "Twin" },
                    { "id": "c3", "name": "Twin", "myRights": { "mayWriteAll": false, "mayWriteOwn": false } },
                ] }),
            ))
            .mount(server)
            .await;
    }

    #[test]
    fn test_api_stays_on_the_origin_of_the_session() {
        let session = Url::parse("http://127.0.0.1:5399/jmap/session").unwrap();
        assert_eq!(
            same_origin(&session, "https://22d33f8e1e3b/jmap/").unwrap(),
            "http://127.0.0.1:5399/jmap/"
        );
        assert_eq!(
            same_origin(&session, "/api/").unwrap(),
            "http://127.0.0.1:5399/api/"
        );
        let session = Url::parse("https://mail.example.org/s").unwrap();
        assert_eq!(
            same_origin(&session, "https://mail.example.org/jmap/").unwrap(),
            "https://mail.example.org/jmap/"
        );
    }

    #[tokio::test]
    async fn test_session_from_the_well_known_address_or_the_given_path() {
        let server = MockServer::start().await;
        mount_session(&server).await;
        mount_calendars(&server).await;
        assert_eq!(client(&server, "c1").calendar_id().await.unwrap(), "c1");

        // A path that is no session falls back to the well-known address
        let at_path = JmapClient::new(
            &format!("{}/dav/", server.uri()),
            ResolvedAuth {
                username: "u".into(),
                password: "p".into(),
            },
            "Work",
        )
        .unwrap();
        assert_eq!(at_path.calendar_id().await.unwrap(), "c1");
    }

    #[tokio::test]
    async fn test_session_errors() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        let err = client(&server, "x").state().await.unwrap_err().to_string();
        assert!(err.contains("rejected the sign-in"), "{err}");

        let server = MockServer::start().await;
        let mut body = session_json(&server);
        body["capabilities"]
            .as_object_mut()
            .unwrap()
            .remove(CALENDARS);
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;
        let err = client(&server, "x").state().await.unwrap_err().to_string();
        assert!(err.contains("no calendars over JMAP"), "{err}");

        // A redirect to another host arrives there without the password
        let (server, other) = (MockServer::start().await, MockServer::start().await);
        Mock::given(method("GET"))
            .and(path("/.well-known/jmap"))
            .respond_with(
                ResponseTemplate::new(307)
                    .insert_header("Location", format!("{}/session", other.uri())),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&other)
            .await;
        let err = client(&server, "x").state().await.unwrap_err().to_string();
        assert!(
            err.contains(&format!("sends the session to {}", other.uri())),
            "{err}"
        );
    }

    #[tokio::test]
    async fn test_calendar_by_name_or_id() {
        let server = MockServer::start().await;
        mount_session(&server).await;
        mount_calendars(&server).await;
        assert_eq!(client(&server, "Work").calendar_id().await.unwrap(), "c1");
        assert_eq!(client(&server, "c3").calendar_id().await.unwrap(), "c3");
        let err = client(&server, "Twin").calendar_id().await.unwrap_err();
        assert!(err.to_string().contains("use the id"), "{err}");
        let err = client(&server, "None").calendar_id().await.unwrap_err();
        assert!(err.to_string().contains("no calendar 'None'"), "{err}");
    }

    #[tokio::test]
    async fn test_discover_lists_calendars_with_rights() {
        let server = MockServer::start().await;
        mount_session(&server).await;
        mount_calendars(&server).await;
        let found = JmapClient::discover_calendars(&server.uri(), "u", "p")
            .await
            .unwrap();
        assert_eq!(found.len(), 3);
        assert_eq!(found[0].jmap_id.as_deref(), Some("c1"));
        assert_eq!(found[0].display_name.as_deref(), Some("Work"));
        assert!(found[0].writable && found[1].writable);
        assert!(!found[2].writable);
    }

    #[tokio::test]
    async fn test_events_respect_the_server_limit_and_query_pages() {
        let server = MockServer::start().await;
        mount_session(&server).await;
        mount_calendars(&server).await;
        Mock::given(method("POST"))
            .and(body_string_contains("CalendarEvent/query"))
            .and(body_string_contains("\"position\":0"))
            .respond_with(answer(
                "CalendarEvent/query",
                json!({ "ids": ["a", "b", "c"] }),
            ))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_string_contains("CalendarEvent/query"))
            .and(body_string_contains("\"position\":3"))
            .respond_with(answer("CalendarEvent/query", json!({ "ids": [] })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_string_contains("CalendarEvent/get"))
            .respond_with(answer(
                "CalendarEvent/get",
                json!({ "list": [{ "id": "x" }], "state": "s" }),
            ))
            .expect(2)
            .mount(&server)
            .await;
        let c = client(&server, "c1");
        // Three ids at two per request
        assert_eq!(c.all_events().await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn test_changes_follow_has_more_and_report_expiry() {
        let server = MockServer::start().await;
        mount_session(&server).await;
        Mock::given(method("POST"))
            .and(body_string_contains("\"sinceState\":\"s1\""))
            .respond_with(answer(
                "CalendarEvent/changes",
                json!({ "created": ["a"], "updated": [], "destroyed": ["d"], "newState": "s2", "hasMoreChanges": true }),
            ))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_string_contains("\"sinceState\":\"s2\""))
            .respond_with(answer(
                "CalendarEvent/changes",
                json!({ "created": [], "updated": ["b"], "destroyed": [], "newState": "s3", "hasMoreChanges": false }),
            ))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_string_contains("\"sinceState\":\"old\""))
            .respond_with(answer("error", json!({ "type": "cannotCalculateChanges" })))
            .mount(&server)
            .await;
        let c = client(&server, "c1");
        let changes = c.changes("s1").await.unwrap();
        assert_eq!(changes.changed, ["a", "b"]);
        assert_eq!(changes.destroyed, ["d"]);
        assert_eq!(changes.state, "s3");
        assert!(matches!(
            c.changes("old").await,
            Err(Error::SyncTokenExpired)
        ));
    }

    #[tokio::test]
    async fn test_set_errors_and_missing_events() {
        let server = MockServer::start().await;
        mount_session(&server).await;
        mount_calendars(&server).await;
        Mock::given(method("POST"))
            .and(body_string_contains("\"create\""))
            .respond_with(answer(
                "CalendarEvent/set",
                json!({ "notCreated": { "e": { "type": "invalidProperties", "description": "bad uid" } } }),
            ))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_string_contains("\"update\""))
            .respond_with(answer(
                "CalendarEvent/set",
                json!({ "notUpdated": { "gone": { "type": "notFound" } } }),
            ))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_string_contains("\"destroy\""))
            .respond_with(answer(
                "CalendarEvent/set",
                json!({ "notDestroyed": { "gone": { "type": "notFound" }, "held": { "type": "forbidden" } } }),
            ))
            .mount(&server)
            .await;
        let c = client(&server, "c1");
        let err = c.create(Map::new()).await.unwrap_err().to_string();
        assert!(err.contains("invalidProperties bad uid"), "{err}");
        assert_eq!(c.update("gone", Map::new()).await.unwrap(), None);
        c.destroy("gone").await.unwrap();
        assert!(c.destroy("held").await.is_err());
    }
}
