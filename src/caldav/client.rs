use std::time::Duration;

use reqwest::{Client, RequestBuilder};
use tracing::{debug, warn};

use crate::caldav::auth::ResolvedAuth;
use crate::error::{Error, Result};

/// Requests one sync-collection may take while the server truncates it.
const MAX_SYNC_PAGES: usize = 1000;

#[derive(Clone)]
pub struct CalDavClient {
    client: Client,
    base_url: String,
    auth: ResolvedAuth,
    retry: RetryPolicy,
}

#[derive(Debug, Clone, Copy)]
struct RetryPolicy {
    max_retries: u32,
    base_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 5,
            base_delay: Duration::from_secs(1),
        }
    }
}

impl RetryPolicy {
    /// Exponential backoff: base, 2x, 4x, 8x, then capped at 16x.
    fn backoff(&self, attempt: u32) -> Duration {
        self.base_delay * (1u32 << attempt.min(4))
    }
}

#[derive(Debug, Clone)]
pub struct EventResource {
    pub href: String,
    pub etag: String,
    pub calendar_data: String,
}

#[derive(Debug, Clone)]
pub struct EventRef {
    pub href: String,
    pub etag: String,
}

#[derive(Debug)]
pub struct SyncCollectionResponse {
    pub changed: Vec<EventResource>,
    /// Hrefs returned as changed (status 200) but without calendar-data in the response.
    /// The caller should multiget these to retrieve their content.
    pub pending_hrefs: Vec<String>,
    pub removed: Vec<String>,
    pub sync_token: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredCalendar {
    pub href: String,
    pub url: String,
    pub display_name: Option<String>,
    pub color: Option<String>,
    /// The current user may write events (from DAV:current-user-privilege-set;
    /// true when the server does not say).
    pub writable: bool,
}

#[derive(Debug)]
pub struct CalendarProps {
    pub ctag: Option<String>,
    #[allow(dead_code)]
    pub display_name: Option<String>,
}

impl CalDavClient {
    pub fn new(base_url: &str, auth: ResolvedAuth) -> Result<Self> {
        let client = Client::builder()
            .connect_timeout(std::time::Duration::from_secs(30))
            .timeout(std::time::Duration::from_secs(300))
            .build()
            .map_err(|e| Error::CalDav(format!("failed to create HTTP client: {e}")))?;
        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            auth,
            retry: RetryPolicy::default(),
        })
    }

    fn url(&self, path: &str) -> String {
        if path.starts_with("http://") || path.starts_with("https://") {
            return path.to_string();
        }
        if path.starts_with('/') {
            // Absolute path - combine with base URL origin
            if let Some(origin) = self.base_url.find("://")
                && let Some(slash) = self.base_url[origin + 3..].find('/')
            {
                let base = &self.base_url[..origin + 3 + slash];
                return format!("{base}{path}");
            }
            return format!("{}{path}", self.base_url);
        }
        format!("{}/{path}", self.base_url)
    }

    /// Send a request with retry and exponential backoff.
    /// `build_request` is called on each attempt to produce a fresh RequestBuilder.
    async fn send_with_retry(
        &self,
        build_request: impl Fn() -> RequestBuilder,
    ) -> Result<reqwest::Response> {
        Self::send_with_retry_with_auth(&self.auth, self.retry, build_request).await
    }

    /// Reusable retry logic with a given auth method.
    async fn send_with_retry_with_auth(
        auth: &ResolvedAuth,
        retry: RetryPolicy,
        build_request: impl Fn() -> RequestBuilder,
    ) -> Result<reqwest::Response> {
        let mut attempt = 0;

        loop {
            // Apply auth to the request
            let req = build_request();
            let req = req.basic_auth(&auth.username, Some(&auth.password));
            let resp = req.send().await;

            match resp {
                Ok(response) => {
                    let status = response.status().as_u16();
                    let retryable = matches!(status, 408 | 429 | 500 | 502 | 503 | 504);

                    if !retryable || attempt >= retry.max_retries {
                        return Ok(response);
                    }

                    // Respect Retry-After header
                    let retry_after = response
                        .headers()
                        .get("Retry-After")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.parse::<u64>().ok())
                        .map(Duration::from_secs);

                    let backoff = retry_after.unwrap_or_else(|| retry.backoff(attempt));

                    warn!(
                        status,
                        attempt = attempt + 1,
                        retry_in_secs = backoff.as_secs(),
                        "retryable HTTP error, backing off"
                    );
                    tokio::time::sleep(backoff).await;
                    attempt += 1;
                }
                Err(e) if e.is_timeout() || e.is_connect() => {
                    if attempt >= retry.max_retries {
                        return Err(e.into());
                    }
                    let backoff = retry.backoff(attempt);
                    warn!(
                        error = %e,
                        attempt = attempt + 1,
                        retry_in_secs = backoff.as_secs(),
                        "network error, backing off"
                    );
                    tokio::time::sleep(backoff).await;
                    attempt += 1;
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    pub async fn get_calendar_props(&self) -> Result<CalendarProps> {
        let body = r#"<?xml version="1.0" encoding="UTF-8"?>
<d:propfind xmlns:d="DAV:" xmlns:cs="http://calendarserver.org/ns/" xmlns:c="urn:ietf:params:xml:ns:caldav">
  <d:prop>
    <d:displayname/>
    <cs:getctag/>
  </d:prop>
</d:propfind>"#;

        let base_url = self.base_url.clone();
        let resp = self
            .send_with_retry(|| {
                self.client
                    .request(reqwest::Method::from_bytes(b"PROPFIND").unwrap(), &base_url)
                    .header("Depth", "0")
                    .header("Content-Type", "application/xml; charset=utf-8")
                    .body(body)
            })
            .await?;

        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() && status.as_u16() != 207 {
            return Err(Error::CalDav(format!(
                "PROPFIND failed with status {status}: {text}"
            )));
        }

        let ctag = extract_xml_text(&text, "getctag");
        let display_name = extract_xml_text(&text, "displayname");

        debug!(ctag = ?ctag, display_name = ?display_name, "got calendar props");
        Ok(CalendarProps { ctag, display_name })
    }

    pub async fn list_events(&self) -> Result<Vec<EventRef>> {
        let body = r#"<?xml version="1.0" encoding="UTF-8"?>
<d:propfind xmlns:d="DAV:">
  <d:prop>
    <d:getetag/>
  </d:prop>
</d:propfind>"#;

        let base_url = self.base_url.clone();
        let resp = self
            .send_with_retry(|| {
                self.client
                    .request(reqwest::Method::from_bytes(b"PROPFIND").unwrap(), &base_url)
                    .header("Depth", "1")
                    .header("Content-Type", "application/xml; charset=utf-8")
                    .body(body)
            })
            .await?;

        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() && status.as_u16() != 207 {
            return Err(Error::CalDav(format!(
                "PROPFIND (list) failed with status {status}"
            )));
        }

        ensure_multistatus(&text, "PROPFIND (list)")?;
        let events = parse_multistatus_refs(&text, &self.base_url);
        debug!(count = events.len(), "listed events");
        Ok(events)
    }

    pub async fn multiget(&self, hrefs: &[String]) -> Result<Vec<EventResource>> {
        if hrefs.is_empty() {
            return Ok(vec![]);
        }

        let href_xml: String = hrefs
            .iter()
            .map(|h| format!("    <d:href>{}</d:href>", xml_escape(h)))
            .collect::<Vec<_>>()
            .join("\n");

        let body = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<c:calendar-multiget xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
  <d:prop>
    <d:getetag/>
    <c:calendar-data/>
  </d:prop>
{href_xml}
</c:calendar-multiget>"#
        );

        let base_url = self.base_url.clone();
        let resp = self
            .send_with_retry(|| {
                self.client
                    .request(reqwest::Method::from_bytes(b"REPORT").unwrap(), &base_url)
                    .header("Depth", "1")
                    .header("Content-Type", "application/xml; charset=utf-8")
                    .body(body.clone())
            })
            .await?;

        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() && status.as_u16() != 207 {
            return Err(Error::CalDav(format!(
                "calendar-multiget failed with status {status}"
            )));
        }

        ensure_multistatus(&text, "calendar-multiget")?;
        Ok(parse_multistatus_events(&text))
    }

    /// Changes since `sync_token` (all events without one). A server may
    /// truncate the answer (RFC 6578 section 3.6, status 507 on the
    /// collection); the remaining pages are fetched with the returned token.
    pub async fn sync_collection(
        &self,
        sync_token: Option<&str>,
    ) -> Result<SyncCollectionResponse> {
        self.sync_collection_with(sync_token, true).await
    }

    /// Like `sync_collection`, but only the hrefs and etags: every changed
    /// href is in `pending_hrefs`.
    pub async fn sync_collection_hrefs(
        &self,
        sync_token: Option<&str>,
    ) -> Result<SyncCollectionResponse> {
        self.sync_collection_with(sync_token, false).await
    }

    async fn sync_collection_with(
        &self,
        sync_token: Option<&str>,
        data: bool,
    ) -> Result<SyncCollectionResponse> {
        let (mut all, mut truncated) = self.sync_collection_page(sync_token, data).await?;
        let mut pages = 1;
        while truncated {
            if pages == MAX_SYNC_PAGES {
                return Err(Error::CalDav(format!(
                    "sync-collection still truncated after {MAX_SYNC_PAGES} requests"
                )));
            }
            let (page, more) = self
                .sync_collection_page(Some(&all.sync_token), data)
                .await?;
            debug!(
                page = pages,
                "sync-collection response was truncated, continuing"
            );
            all.changed.extend(page.changed);
            all.pending_hrefs.extend(page.pending_hrefs);
            all.removed.extend(page.removed);
            all.sync_token = page.sync_token;
            truncated = more;
            pages += 1;
        }
        Ok(all)
    }

    /// One sync-collection request, and whether the server truncated it.
    async fn sync_collection_page(
        &self,
        sync_token: Option<&str>,
        data: bool,
    ) -> Result<(SyncCollectionResponse, bool)> {
        let token_xml = match sync_token {
            Some(token) => format!("  <d:sync-token>{}</d:sync-token>", xml_escape(token)),
            None => "  <d:sync-token/>".to_string(),
        };
        let data_xml = if data { "\n    <c:calendar-data/>" } else { "" };

        let body = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<d:sync-collection xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
{token_xml}
  <d:sync-level>1</d:sync-level>
  <d:prop>
    <d:getetag/>{data_xml}
  </d:prop>
</d:sync-collection>"#
        );

        let base_url = self.base_url.clone();
        let resp = self
            .send_with_retry(|| {
                self.client
                    .request(reqwest::Method::from_bytes(b"REPORT").unwrap(), &base_url)
                    .header("Content-Type", "application/xml; charset=utf-8")
                    .body(body.clone())
            })
            .await?;

        let status = resp.status();
        let text = resp.text().await?;

        // 403 or 409 typically means expired/invalid sync token
        if status.as_u16() == 403 || status.as_u16() == 409 {
            return Err(Error::SyncTokenExpired);
        }

        if !status.is_success() && status.as_u16() != 207 {
            return Err(Error::CalDav(format!(
                "sync-collection REPORT failed with status {status}"
            )));
        }
        ensure_multistatus(&text, "sync-collection REPORT")?;

        // The top-level sync-token is outside every response element, after
        // them (most servers) or before them (Radicale)
        let mut outside_responses = String::new();
        let mut from = 0;
        for (start, end) in response_spans(&text) {
            outside_responses.push_str(&text[from..start]);
            from = end;
        }
        outside_responses.push_str(&text[from..]);
        let new_sync_token = extract_xml_text(&outside_responses, "sync-token").unwrap_or_default();
        let (changed, pending_hrefs, removed, truncated) = parse_sync_response(&text);

        debug!(
            changed = changed.len(),
            pending = pending_hrefs.len(),
            removed = removed.len(),
            sync_token = %new_sync_token,
            "sync-collection response"
        );

        let response = SyncCollectionResponse {
            changed,
            pending_hrefs,
            removed,
            sync_token: new_sync_token,
        };
        Ok((response, truncated))
    }

    /// Put an event to the calendar.
    /// - `etag: Some(etag)` - update existing resource (If-Match)
    /// - `etag: None` - unconditional put (no precondition headers)
    pub async fn put_event(
        &self,
        href: &str,
        ical_data: &str,
        etag: Option<&str>,
    ) -> Result<String> {
        let url = self.url(href);
        let etag_owned = etag.map(|s| s.to_string());
        let ical_owned = ical_data.to_string();
        let resp = self
            .send_with_retry(|| {
                let mut req = self
                    .client
                    .put(&url)
                    .header("Content-Type", "text/calendar; charset=utf-8")
                    .body(ical_owned.clone());
                if let Some(ref etag) = etag_owned {
                    req = req.header("If-Match", etag.as_str());
                }
                req
            })
            .await?;
        let status = resp.status();
        let new_etag = resp
            .headers()
            .get("ETag")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(Error::CalDav(format!(
                "PUT {href} failed with status {status}: {text}"
            )));
        }

        debug!(href, etag = %new_etag, "put event");
        Ok(new_etag)
    }

    /// Discover all calendars for the given credentials.
    /// Follows the CalDAV discovery flow (RFC 6764):
    /// 1. /.well-known/caldav redirect
    /// 2. PROPFIND current-user-principal
    /// 3. PROPFIND calendar-home-set
    /// 4. PROPFIND calendar-home to list calendars
    pub async fn discover_calendars(
        base_url: &str,
        username: &str,
        password: &str,
    ) -> Result<Vec<DiscoveredCalendar>> {
        let client = Client::builder()
            .connect_timeout(std::time::Duration::from_secs(30))
            .timeout(std::time::Duration::from_secs(60))
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
            .map_err(|e| Error::CalDav(format!("failed to create HTTP client: {e}")))?;

        let base = base_url.trim_end_matches('/');

        // Step 1: Try /.well-known/caldav to find the DAV root
        let dav_root = Self::try_well_known(&client, base, username, password)
            .await
            .unwrap_or_else(|_| base.to_string());

        // Step 2: Find current-user-principal
        let principal = Self::find_principal(&client, &dav_root, username, password).await?;
        let principal_url = Self::resolve_url(base, &principal);

        // Step 3: Find calendar-home-set
        let home = Self::find_calendar_home(&client, &principal_url, username, password).await?;
        let home_url = Self::resolve_url(base, &home);

        // Step 4: List calendars in the home
        Self::list_calendars(&client, &home_url, username, password).await
    }

    async fn try_well_known(
        client: &Client,
        base: &str,
        username: &str,
        password: &str,
    ) -> Result<String> {
        let url = format!("{base}/.well-known/caldav");
        let resp = Self::send_with_retry_with_auth(&ResolvedAuth { username: username.to_string(), password: password.to_string() }, RetryPolicy::default(), || {
            client
                .request(reqwest::Method::from_bytes(b"PROPFIND").unwrap(), &url)
                .header("Depth", "0")
                .header("Content-Type", "application/xml; charset=utf-8")
                .body(r#"<?xml version="1.0"?><d:propfind xmlns:d="DAV:"><d:prop><d:resourcetype/></d:prop></d:propfind>"#)
        })
        .await?;

        // A followed redirect counts even if the target rejects this PROPFIND body
        let redirected = !resp.url().path().ends_with("/.well-known/caldav");
        let status = resp.status();
        if !redirected && !status.is_success() && !status.is_redirection() {
            return Err(Error::CalDav(format!(
                "well-known lookup failed with status {status}"
            )));
        }

        // The server may redirect (301/302) or return 207 directly
        // reqwest follows redirects, so the final URL is our DAV root
        let final_url = resp.url().to_string();
        let final_url = final_url.trim_end_matches('/').to_string();
        Ok(final_url)
    }

    async fn find_principal(
        client: &Client,
        dav_root: &str,
        username: &str,
        password: &str,
    ) -> Result<String> {
        let body = r#"<?xml version="1.0" encoding="UTF-8"?>
<d:propfind xmlns:d="DAV:">
  <d:prop>
    <d:current-user-principal/>
  </d:prop>
</d:propfind>"#;

        let dav_root_owned = dav_root.to_string();
        let resp = Self::send_with_retry_with_auth(
            &ResolvedAuth {
                username: username.to_string(),
                password: password.to_string(),
            },
            RetryPolicy::default(),
            || {
                client
                    .request(
                        reqwest::Method::from_bytes(b"PROPFIND").unwrap(),
                        &dav_root_owned,
                    )
                    .header("Depth", "0")
                    .header("Content-Type", "application/xml; charset=utf-8")
                    .body(body)
            },
        )
        .await?;

        let text = resp.text().await?;
        extract_xml_text(&text, "current-user-principal")
            .and_then(|p| extract_xml_text(&p, "href"))
            .ok_or_else(|| {
                Error::CalDav(
                    "could not find current-user-principal - check URL and credentials".to_string(),
                )
            })
    }

    async fn find_calendar_home(
        client: &Client,
        principal_url: &str,
        username: &str,
        password: &str,
    ) -> Result<String> {
        let body = r#"<?xml version="1.0" encoding="UTF-8"?>
<d:propfind xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
  <d:prop>
    <c:calendar-home-set/>
  </d:prop>
</d:propfind>"#;

        let principal_owned = principal_url.to_string();
        let resp = Self::send_with_retry_with_auth(
            &ResolvedAuth {
                username: username.to_string(),
                password: password.to_string(),
            },
            RetryPolicy::default(),
            || {
                client
                    .request(
                        reqwest::Method::from_bytes(b"PROPFIND").unwrap(),
                        &principal_owned,
                    )
                    .header("Depth", "0")
                    .header("Content-Type", "application/xml; charset=utf-8")
                    .body(body)
            },
        )
        .await?;

        let text = resp.text().await?;
        // calendar-home-set contains an href
        // Look for <href> inside <calendar-home-set>
        let home_set = extract_xml_text(&text, "calendar-home-set").ok_or_else(|| {
            Error::CalDav("could not find calendar-home-set on principal".to_string())
        })?;
        // The home-set content contains <d:href>...</d:href>
        extract_xml_text(&home_set, "href")
            .or(Some(home_set))
            .ok_or_else(|| Error::CalDav("empty calendar-home-set".to_string()))
    }

    async fn list_calendars(
        client: &Client,
        home_url: &str,
        username: &str,
        password: &str,
    ) -> Result<Vec<DiscoveredCalendar>> {
        let body = r#"<?xml version="1.0" encoding="UTF-8"?>
<d:propfind xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav" xmlns:cs="http://calendarserver.org/ns/" xmlns:ic="http://apple.com/ns/ical/">
  <d:prop>
    <d:displayname/>
    <d:resourcetype/>
    <cs:getctag/>
    <ic:calendar-color/>
    <d:current-user-privilege-set/>
    <c:supported-calendar-component-set/>
  </d:prop>
</d:propfind>"#;

        let home_owned = home_url.to_string();
        let resp = Self::send_with_retry_with_auth(
            &ResolvedAuth {
                username: username.to_string(),
                password: password.to_string(),
            },
            RetryPolicy::default(),
            || {
                client
                    .request(
                        reqwest::Method::from_bytes(b"PROPFIND").unwrap(),
                        &home_owned,
                    )
                    .header("Depth", "1")
                    .header("Content-Type", "application/xml; charset=utf-8")
                    .body(body)
            },
        )
        .await?;

        let text = resp.text().await?;
        let mut calendars = Vec::new();

        for block in split_response_blocks(&text) {
            let resource_type = extract_xml_text(&block, "resourcetype").unwrap_or_default();
            if !contains_element(&resource_type, "calendar") {
                continue;
            }

            // Skip if this is just the home collection (no ctag typically)
            let href = match extract_xml_text(&block, "href") {
                Some(h) => h,
                None => continue,
            };

            // Skip the home itself
            let home_path = home_url
                .find("://")
                .and_then(|i| home_url[i + 3..].find('/'))
                .map(|i| {
                    let start = home_url.find("://").unwrap() + 3 + i;
                    &home_url[start..]
                })
                .unwrap_or("");
            if href.trim_end_matches('/') == home_path.trim_end_matches('/') {
                continue;
            }

            // Task lists and journals hold no events
            if let Some(components) = extract_xml_text(&block, "supported-calendar-component-set")
                && !components.contains("VEVENT")
            {
                continue;
            }
            let writable = extract_xml_text(&block, "current-user-privilege-set").is_none_or(|p| {
                ["write", "write-content", "bind", "all"]
                    .iter()
                    .any(|privilege| contains_element(&p, privilege))
            });
            let display_name = extract_xml_text(&block, "displayname");
            let color = extract_xml_text(&block, "calendar-color");

            calendars.push(DiscoveredCalendar {
                href: href.clone(),
                display_name,
                color,
                writable,
                url: Self::resolve_url(home_url, &href),
            });
        }

        Ok(calendars)
    }

    fn resolve_url(base: &str, href: &str) -> String {
        if href.starts_with("http://") || href.starts_with("https://") {
            return href.to_string();
        }
        if href.starts_with('/') {
            if let Some(origin) = base.find("://")
                && let Some(slash) = base[origin + 3..].find('/')
            {
                let origin_end = origin + 3 + slash;
                return format!("{}{href}", &base[..origin_end]);
            }
            return format!("{base}{href}");
        }
        format!("{}/{href}", base.trim_end_matches('/'))
    }

    pub async fn delete_event(&self, href: &str) -> Result<()> {
        let url = self.url(href);
        let resp = self.send_with_retry(|| self.client.delete(&url)).await?;

        let status = resp.status();
        if !status.is_success() && status.as_u16() != 404 {
            let text = resp.text().await.unwrap_or_default();
            return Err(Error::CalDav(format!(
                "DELETE {href} failed with status {status}: {text}"
            )));
        }

        debug!(href, "deleted event");
        Ok(())
    }
}

// XML text extraction helpers.
// Operates on the full XML string (not line-by-line) to handle multiline content
// like calendar-data.

fn extract_xml_text(xml: &str, tag_local_name: &str) -> Option<String> {
    // Find opening tag matching the local name (with any namespace prefix)
    let mut search_pos = 0;
    while search_pos < xml.len() {
        let start = find_tag_content_start(xml, tag_local_name, search_pos)?;
        if let Some(end) = find_closing_tag(xml, tag_local_name, start) {
            let content = xml[start..end].trim();
            if !content.is_empty() {
                return Some(xml_unescape(strip_cdata(content)));
            }
        }
        search_pos = start;
    }
    None
}

fn contains_element(xml: &str, tag_local_name: &str) -> bool {
    xml.split('<').skip(1).any(|tag| {
        let name = tag
            .split(|c: char| c.is_whitespace() || c == '/' || c == '>')
            .next()
            .unwrap_or("");
        name.rsplit(':').next() == Some(tag_local_name)
    })
}

/// Strip CDATA wrapper if present: <![CDATA[...]]> -> ...
fn strip_cdata(s: &str) -> &str {
    let s = s.trim();
    if let Some(inner) = s.strip_prefix("<![CDATA[")
        && let Some(inner) = inner.strip_suffix("]]>")
    {
        return inner;
    }
    s
}

fn find_tag_content_start(xml: &str, tag_name: &str, from: usize) -> Option<usize> {
    find_open_tag(xml, tag_name, from).map(|(_, content)| content)
}

/// The position of the next opening tag with local name `tag_name` (any
/// namespace prefix, not self-closing) and of its content.
fn find_open_tag(xml: &str, tag_name: &str, from: usize) -> Option<(usize, usize)> {
    let mut search_pos = from;
    while let Some(lt) = xml[search_pos..].find('<') {
        let pos = search_pos + lt;
        let after_lt = &xml[pos + 1..];

        // Skip closing tags and comments
        if after_lt.starts_with('/') || after_lt.starts_with('!') || after_lt.starts_with('?') {
            search_pos = pos + 1;
            continue;
        }

        if let Some(gt) = after_lt.find('>') {
            let tag_content = &after_lt[..gt];
            // Self-closing tag - skip
            if tag_content.ends_with('/') {
                search_pos = pos + 1 + gt + 1;
                continue;
            }
            let tag_only = tag_content.split_whitespace().next().unwrap_or("");
            let local = tag_only
                .rfind(':')
                .map(|i| &tag_only[i + 1..])
                .unwrap_or(tag_only);

            if local == tag_name {
                return Some((pos, pos + 1 + gt + 1)); // '<' and after '>'
            }
        }

        search_pos = pos + 1;
    }
    None
}

fn find_closing_tag(xml: &str, tag_name: &str, from: usize) -> Option<usize> {
    let mut search_pos = from;
    while let Some(lt) = xml[search_pos..].find("</") {
        let pos = search_pos + lt;
        let after = &xml[pos + 2..];
        if let Some(gt) = after.find('>') {
            let tag = &after[..gt].trim();
            let local = tag.rfind(':').map(|i| &tag[i + 1..]).unwrap_or(tag);
            if local == tag_name {
                return Some(pos); // position of '</'
            }
        }
        search_pos = pos + 2;
    }
    None
}

fn xml_unescape(s: &str) -> String {
    // Order matters: &amp; must be replaced LAST to prevent double-unescaping.
    // E.g. "&amp;lt;" should become "&lt;" (literal), not "<".
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn parse_multistatus_refs(xml: &str, base_url: &str) -> Vec<EventRef> {
    let mut results = Vec::new();
    // Split on <d:response> or <response> blocks
    for response_block in split_response_blocks(xml) {
        let href = match extract_xml_text(&response_block, "href") {
            Some(h) => h,
            None => continue,
        };
        // Skip the calendar collection itself (href matches base path)
        if is_collection_href(&href, base_url) {
            continue;
        }
        // Only include .ics files
        if !href.ends_with(".ics") {
            continue;
        }
        let etag = extract_xml_text(&response_block, "getetag").unwrap_or_default();
        results.push(EventRef { href, etag });
    }
    results
}

fn parse_multistatus_events(xml: &str) -> Vec<EventResource> {
    let mut results = Vec::new();
    for response_block in split_response_blocks(xml) {
        let href = match extract_xml_text(&response_block, "href") {
            Some(h) => h,
            None => continue,
        };
        let etag = extract_xml_text(&response_block, "getetag").unwrap_or_default();
        let calendar_data = extract_xml_text(&response_block, "calendar-data").unwrap_or_default();
        if calendar_data.is_empty() {
            continue;
        }
        results.push(EventResource {
            href,
            etag,
            calendar_data,
        });
    }
    results
}

/// Changed events, hrefs to fetch, removed hrefs and whether the server
/// truncated the result.
fn parse_sync_response(xml: &str) -> (Vec<EventResource>, Vec<String>, Vec<String>, bool) {
    let mut changed = Vec::new();
    let mut pending_hrefs = Vec::new();
    let mut removed = Vec::new();
    let mut truncated = false;

    for response_block in split_response_blocks(xml) {
        let href = match extract_xml_text(&response_block, "href") {
            Some(h) => h,
            None => continue,
        };

        // A response-level status (RFC 4918: status xor propstat) of 404
        // means deleted, 507 on the collection that more changes follow
        if extract_xml_text(&response_block, "propstat").is_none()
            && let Some(status_text) = extract_xml_text(&response_block, "status")
        {
            if status_text.contains("404") {
                removed.push(href);
            } else if status_text.contains("507") {
                truncated = true;
            }
            continue;
        }

        let etag = extract_xml_text(&response_block, "getetag").unwrap_or_default();
        let calendar_data = extract_xml_text(&response_block, "calendar-data").unwrap_or_default();
        if !calendar_data.is_empty() {
            changed.push(EventResource {
                href,
                etag,
                calendar_data,
            });
        } else {
            // Some servers omit calendar-data from sync responses even when requested.
            // Collect the href so the caller can fetch it separately via multiget.
            pending_hrefs.push(href);
        }
    }

    (changed, pending_hrefs, removed, truncated)
}

/// A WebDAV answer must be a multistatus. A proxy or login page answering
/// with 200 and HTML would otherwise read as an empty calendar.
fn ensure_multistatus(text: &str, request: &str) -> Result<()> {
    if text.to_ascii_lowercase().contains("multistatus") {
        Ok(())
    } else {
        let start: String = text.chars().take(80).collect();
        Err(Error::CalDav(format!(
            "{request} returned no WebDAV multistatus, starting with {start:?}"
        )))
    }
}

/// The spans of the top-level `response` elements, with any namespace prefix.
fn response_spans(xml: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut from = 0;
    while let Some((open, content)) = find_open_tag(xml, "response", from) {
        let Some(close) = find_closing_tag(xml, "response", content) else {
            break;
        };
        let end = xml[close..]
            .find('>')
            .map_or(xml.len(), |gt| close + gt + 1);
        spans.push((open, end));
        from = end;
    }
    spans
}

fn split_response_blocks(xml: &str) -> Vec<String> {
    response_spans(xml)
        .into_iter()
        .map(|(start, end)| xml[start..end].to_string())
        .collect()
}

fn is_collection_href(href: &str, base_url: &str) -> bool {
    let base_path = base_url
        .find("://")
        .and_then(|i| base_url[i + 3..].find('/'))
        .map(|i| {
            let start = base_url.find("://").unwrap() + 3 + i;
            base_url[start..].trim_end_matches('/')
        })
        .unwrap_or("");

    let href_trimmed = href.trim_end_matches('/');
    href_trimmed == base_path || href_trimmed.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- XML parser tests --

    #[test]
    fn test_extract_xml_text_simple() {
        let xml = r#"<d:multistatus><d:response><d:href>/cal/event.ics</d:href></d:response></d:multistatus>"#;
        assert_eq!(
            extract_xml_text(xml, "href"),
            Some("/cal/event.ics".to_string())
        );
    }

    #[test]
    fn test_extract_xml_text_with_prefix() {
        let xml = r#"<d:prop><cs:getctag>tag-123</cs:getctag></d:prop>"#;
        assert_eq!(
            extract_xml_text(xml, "getctag"),
            Some("tag-123".to_string())
        );
    }

    #[test]
    fn test_extract_xml_text_multiline() {
        let xml = "<c:calendar-data>BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:123\r\nEND:VEVENT\r\nEND:VCALENDAR</c:calendar-data>";
        let result = extract_xml_text(xml, "calendar-data").unwrap();
        assert!(result.starts_with("BEGIN:VCALENDAR"));
        assert!(result.contains("UID:123"));
        assert!(result.ends_with("END:VCALENDAR"));
    }

    #[test]
    fn test_extract_xml_text_missing_tag() {
        let xml = r#"<d:prop><d:displayname>Cal</d:displayname></d:prop>"#;
        assert_eq!(extract_xml_text(xml, "getctag"), None);
    }

    #[test]
    fn test_extract_xml_text_empty_tag() {
        let xml = r#"<d:prop><d:displayname></d:displayname></d:prop>"#;
        assert_eq!(extract_xml_text(xml, "displayname"), None);
    }

    #[test]
    fn test_extract_xml_text_self_closing() {
        let xml = r#"<d:prop><d:displayname/><cs:getctag>abc</cs:getctag></d:prop>"#;
        assert_eq!(extract_xml_text(xml, "getctag"), Some("abc".to_string()));
        assert_eq!(extract_xml_text(xml, "displayname"), None);
    }

    #[test]
    fn test_xml_unescape_basic() {
        assert_eq!(xml_unescape("hello &amp; world"), "hello & world");
        assert_eq!(xml_unescape("a &lt; b &gt; c"), "a < b > c");
        assert_eq!(xml_unescape("&quot;quoted&quot;"), "\"quoted\"");
    }

    #[test]
    fn test_xml_unescape_no_double_unescape() {
        // "&amp;lt;" represents the literal text "&lt;", NOT "<"
        assert_eq!(xml_unescape("&amp;lt;"), "&lt;");
        assert_eq!(xml_unescape("&amp;amp;"), "&amp;");
    }

    #[test]
    fn test_xml_escape_roundtrip() {
        let original = "a < b & c > d \"e\"";
        let escaped = xml_escape(original);
        // Raw metacharacters should be gone (& is replaced with &amp;, so no raw & remains
        // except as part of escape sequences)
        assert!(!escaped.contains("a < b"));
        assert!(escaped.contains("&amp;"));
        assert!(escaped.contains("&lt;"));
        // unescape should recover original
        assert_eq!(xml_unescape(&escaped), original);
    }

    #[test]
    fn test_strip_cdata() {
        assert_eq!(
            strip_cdata("<![CDATA[BEGIN:VCALENDAR\nEND:VCALENDAR]]>"),
            "BEGIN:VCALENDAR\nEND:VCALENDAR"
        );
        assert_eq!(strip_cdata("no cdata here"), "no cdata here");
        assert_eq!(strip_cdata("  <![CDATA[inner]]>  "), "inner");
    }

    #[test]
    fn test_split_response_blocks() {
        let xml = r#"<?xml version="1.0"?>
<d:multistatus xmlns:d="DAV:">
  <d:response>
    <d:href>/cal/</d:href>
  </d:response>
  <d:response>
    <d:href>/cal/event1.ics</d:href>
    <d:propstat><d:prop><d:getetag>"etag1"</d:getetag></d:prop></d:propstat>
  </d:response>
  <d:response>
    <d:href>/cal/event2.ics</d:href>
  </d:response>
</d:multistatus>"#;
        let blocks = split_response_blocks(xml);
        assert_eq!(blocks.len(), 3);
        // Block 0 is the collection itself (href is just "/cal/", no .ics)
        assert!(blocks[0].contains("<d:href>/cal/</d:href>"));
        assert!(!blocks[0].contains(".ics"));
        // Blocks 1 and 2 are events
        assert!(blocks[1].contains("event1.ics"));
        assert!(blocks[1].contains("etag1"));
        assert!(blocks[2].contains("event2.ics"));
    }

    #[test]
    fn test_responses_with_any_namespace_prefix() {
        let xml = r#"<ns0:multistatus xmlns:ns0="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
<ns0:response><ns0:href>/cal/a.ics</ns0:href><ns0:propstat><ns0:prop>
<ns0:getetag>"ea"</ns0:getetag><C:calendar-data>BEGIN:VCALENDAR</C:calendar-data>
</ns0:prop></ns0:propstat></ns0:response>
<ns0:response><ns0:href>/cal/gone.ics</ns0:href><ns0:status>HTTP/1.1 404 Not Found</ns0:status></ns0:response>
<ns0:responsedescription>done</ns0:responsedescription>
<ns0:sync-token>t9</ns0:sync-token></ns0:multistatus>"#;
        let (changed, pending, removed, truncated) = parse_sync_response(xml);
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].href, "/cal/a.ics");
        assert_eq!(changed[0].etag, "\"ea\"");
        assert!(pending.is_empty());
        assert_eq!(removed, ["/cal/gone.ics"]);
        assert!(!truncated);
        let after = response_spans(xml).last().unwrap().1;
        assert_eq!(
            extract_xml_text(&xml[after..], "sync-token").as_deref(),
            Some("t9")
        );
    }

    #[test]
    fn test_split_response_blocks_with_case_folding_that_changes_length() {
        // "İ" lowercases to two characters, one byte longer
        let summary = "İ".repeat(30);
        let xml = format!(
            "<d:multistatus xmlns:d=\"DAV:\"><d:response><d:href>/cal/a.ics</d:href>\
             <c:calendar-data>SUMMARY:{summary}</c:calendar-data></d:response>\
             <d:response><d:href>/cal/b.ics</d:href></d:response></d:multistatus>"
        );
        let blocks = split_response_blocks(&xml);
        assert_eq!(blocks.len(), 2);
        assert!(blocks[0].contains(&summary) && blocks[0].ends_with("</d:response>"));
        assert!(blocks[1].starts_with("<d:response><d:href>/cal/b.ics"));
    }

    #[test]
    fn test_parse_multistatus_refs_filters_collection() {
        let xml = r#"<?xml version="1.0"?>
<d:multistatus xmlns:d="DAV:">
  <d:response>
    <d:href>/calendars/alice/default/</d:href>
    <d:propstat><d:prop><d:getetag>"col-etag"</d:getetag></d:prop></d:propstat>
  </d:response>
  <d:response>
    <d:href>/calendars/alice/default/event1.ics</d:href>
    <d:propstat><d:prop><d:getetag>"e1"</d:getetag></d:prop></d:propstat>
  </d:response>
</d:multistatus>"#;
        let refs =
            parse_multistatus_refs(xml, "https://server.example.com/calendars/alice/default/");
        assert_eq!(refs.len(), 1);
        assert!(refs[0].href.contains("event1.ics"));
        assert_eq!(refs[0].etag, "\"e1\"");
    }

    #[test]
    fn test_parse_sync_response_with_deletions() {
        let xml = r#"<?xml version="1.0"?>
<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
  <d:response>
    <d:href>/cal/changed.ics</d:href>
    <d:propstat>
      <d:prop>
        <d:getetag>"new-etag"</d:getetag>
        <c:calendar-data>BEGIN:VCALENDAR
END:VCALENDAR</c:calendar-data>
      </d:prop>
      <d:status>HTTP/1.1 200 OK</d:status>
    </d:propstat>
  </d:response>
  <d:response>
    <d:href>/cal/deleted.ics</d:href>
    <d:status>HTTP/1.1 404 Not Found</d:status>
  </d:response>
  <d:sync-token>http://example.com/sync/new-token</d:sync-token>
</d:multistatus>"#;
        let (changed, pending, removed, _) = parse_sync_response(xml);
        assert_eq!(changed.len(), 1);
        assert!(changed[0].href.contains("changed.ics"));
        assert_eq!(pending.len(), 0);
        assert_eq!(removed.len(), 1);
        assert!(removed[0].contains("deleted.ics"));
    }

    #[test]
    fn test_parse_sync_response_pending_hrefs_without_data() {
        let xml = r#"<?xml version="1.0"?>
<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
  <d:response>
    <d:href>/cal/with-data.ics</d:href>
    <d:propstat>
      <d:prop>
        <d:getetag>"etag-1"</d:getetag>
        <c:calendar-data>BEGIN:VCALENDAR
END:VCALENDAR</c:calendar-data>
      </d:prop>
      <d:status>HTTP/1.1 200 OK</d:status>
    </d:propstat>
  </d:response>
  <d:response>
    <d:href>/cal/no-data.ics</d:href>
    <d:propstat>
      <d:prop>
        <d:getetag>"etag-2"</d:getetag>
      </d:prop>
      <d:status>HTTP/1.1 200 OK</d:status>
    </d:propstat>
  </d:response>
  <d:response>
    <d:href>/cal/deleted.ics</d:href>
    <d:status>HTTP/1.1 404 Not Found</d:status>
  </d:response>
</d:multistatus>"#;
        let (changed, pending, removed, _) = parse_sync_response(xml);
        assert_eq!(changed.len(), 1);
        assert!(changed[0].href.contains("with-data.ics"));
        assert_eq!(pending.len(), 1);
        assert!(pending[0].contains("no-data.ics"));
        assert_eq!(removed.len(), 1);
        assert!(removed[0].contains("deleted.ics"));
    }

    // -- URL resolution tests --

    #[test]
    fn test_url_full_url_passthrough() {
        let client = CalDavClient::new(
            "https://server.com/cal/",
            ResolvedAuth {
                username: "u".to_string(),
                password: "p".to_string(),
            },
        )
        .unwrap();
        assert_eq!(
            client.url("https://other.com/path"),
            "https://other.com/path"
        );
    }

    #[test]
    fn test_url_absolute_path() {
        let client = CalDavClient::new(
            "https://server.com/calendars/alice/default/",
            ResolvedAuth {
                username: "u".to_string(),
                password: "p".to_string(),
            },
        )
        .unwrap();
        assert_eq!(
            client.url("/calendars/alice/default/event.ics"),
            "https://server.com/calendars/alice/default/event.ics"
        );
    }

    #[test]
    fn test_url_relative_path() {
        let client = CalDavClient::new(
            "https://server.com/calendars/alice/default/",
            ResolvedAuth {
                username: "u".to_string(),
                password: "p".to_string(),
            },
        )
        .unwrap();
        assert_eq!(
            client.url("event.ics"),
            "https://server.com/calendars/alice/default/event.ics"
        );
    }

    // -- is_collection_href tests --

    #[test]
    fn test_is_collection_href_matches() {
        assert!(is_collection_href(
            "/calendars/alice/default/",
            "https://server.com/calendars/alice/default/"
        ));
        assert!(is_collection_href(
            "/calendars/alice/default",
            "https://server.com/calendars/alice/default/"
        ));
    }

    #[test]
    fn test_is_collection_href_no_match() {
        assert!(!is_collection_href(
            "/calendars/alice/default/event.ics",
            "https://server.com/calendars/alice/default/"
        ));
    }

    #[test]
    fn test_is_collection_href_empty() {
        assert!(is_collection_href("", "https://server.com/cal/"));
    }

    // -- Network tests against a mock CalDAV server --

    use wiremock::matchers::{basic_auth, body_string_contains, header, method, path, path_regex};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    const ICS: &str = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:e1\r\nEND:VEVENT\r\nEND:VCALENDAR";

    fn mock_client(server: &MockServer) -> CalDavClient {
        CalDavClient::new(
            &format!("{}/cal/", server.uri()),
            ResolvedAuth {
                username: "u".to_string(),
                password: "p".to_string(),
            },
        )
        .unwrap()
    }

    const FAST_RETRY: RetryPolicy = RetryPolicy {
        max_retries: 5,
        base_delay: Duration::from_millis(1),
    };

    /// Client with millisecond backoff and an optional short request timeout.
    fn fast_client(base_url: &str, timeout: Option<Duration>) -> CalDavClient {
        let mut builder = Client::builder();
        if let Some(t) = timeout {
            builder = builder.timeout(t);
        }
        CalDavClient {
            client: builder.build().unwrap(),
            base_url: base_url.to_string(),
            auth: ResolvedAuth {
                username: "u".to_string(),
                password: "p".to_string(),
            },
            retry: FAST_RETRY,
        }
    }

    fn multistatus(inner: &str) -> ResponseTemplate {
        ResponseTemplate::new(207).set_body_string(format!(
            r#"<?xml version="1.0" encoding="utf-8"?>
<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav" xmlns:cs="http://calendarserver.org/ns/" xmlns:ic="http://apple.com/ns/ical/">
{inner}
</d:multistatus>"#
        ))
    }

    fn body(req: &Request) -> String {
        String::from_utf8(req.body.clone()).unwrap()
    }

    async fn requests(server: &MockServer) -> Vec<Request> {
        server.received_requests().await.unwrap()
    }

    #[tokio::test]
    async fn test_get_calendar_props_parses_ctag_and_sends_auth() {
        let server = MockServer::start().await;
        Mock::given(method("PROPFIND"))
            .and(path("/cal"))
            .and(header("Depth", "0"))
            .and(basic_auth("u", "p"))
            .respond_with(multistatus(
                r#"<d:response><d:href>/cal/</d:href><d:propstat><d:prop>
<d:displayname>Work &amp; Life</d:displayname><cs:getctag>"ctag-1"</cs:getctag>
</d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>"#,
            ))
            .expect(1)
            .mount(&server)
            .await;

        let props = mock_client(&server).get_calendar_props().await.unwrap();
        assert_eq!(props.ctag.as_deref(), Some("\"ctag-1\""));
        assert_eq!(props.display_name.as_deref(), Some("Work & Life"));
        assert!(body(&requests(&server).await[0]).contains("<cs:getctag/>"));
    }

    #[tokio::test]
    async fn test_get_calendar_props_error_status_is_not_retried() {
        let server = MockServer::start().await;
        Mock::given(method("PROPFIND"))
            .respond_with(ResponseTemplate::new(401).set_body_string("nope"))
            .expect(1)
            .mount(&server)
            .await;

        let err = mock_client(&server).get_calendar_props().await.unwrap_err();
        assert!(err.to_string().contains("401"), "{err}");
    }

    #[tokio::test]
    async fn test_list_events_filters_collection_and_non_ics() {
        let server = MockServer::start().await;
        Mock::given(method("PROPFIND"))
            .and(header("Depth", "1"))
            .respond_with(multistatus(
                r#"<d:response><d:href>/cal/</d:href><d:propstat><d:prop><d:getetag>"col"</d:getetag></d:prop></d:propstat></d:response>
<d:response><d:href>/cal/a.ics</d:href><d:propstat><d:prop><d:getetag>"ea"</d:getetag></d:prop></d:propstat></d:response>
<d:response><d:href>/cal/notes.txt</d:href><d:propstat><d:prop><d:getetag>"en"</d:getetag></d:prop></d:propstat></d:response>
<d:response><d:href>/cal/b.ics</d:href><d:propstat><d:prop><d:getetag>"eb"</d:getetag></d:prop></d:propstat></d:response>"#,
            ))
            .expect(1)
            .mount(&server)
            .await;

        let refs = mock_client(&server).list_events().await.unwrap();
        let got: Vec<(&str, &str)> = refs
            .iter()
            .map(|r| (r.href.as_str(), r.etag.as_str()))
            .collect();
        assert_eq!(
            got,
            vec![("/cal/a.ics", "\"ea\""), ("/cal/b.ics", "\"eb\"")]
        );
    }

    #[tokio::test]
    async fn test_list_events_error_status() {
        let server = MockServer::start().await;
        Mock::given(method("PROPFIND"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let err = mock_client(&server).list_events().await.unwrap_err();
        assert!(err.to_string().contains("404"), "{err}");
    }

    #[tokio::test]
    async fn test_multiget_empty_sends_no_request() {
        let server = MockServer::start().await;
        Mock::given(method("REPORT"))
            .respond_with(multistatus(""))
            .expect(0)
            .mount(&server)
            .await;

        assert!(mock_client(&server).multiget(&[]).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_multiget_escapes_hrefs_and_parses_cdata() {
        let server = MockServer::start().await;
        Mock::given(method("REPORT"))
            .and(path("/cal"))
            .and(header("Depth", "1"))
            .and(body_string_contains("calendar-multiget"))
            .respond_with(multistatus(&format!(
                r#"<d:response><d:href>/cal/a&amp;b.ics</d:href><d:propstat><d:prop>
<d:getetag>"e1"</d:getetag><c:calendar-data><![CDATA[{ICS}]]></c:calendar-data>
</d:prop></d:propstat></d:response>
<d:response><d:href>/cal/missing.ics</d:href><d:status>HTTP/1.1 404 Not Found</d:status></d:response>"#
            )))
            .expect(1)
            .mount(&server)
            .await;

        let hrefs = vec!["/cal/a&b.ics".to_string(), "/cal/missing.ics".to_string()];
        let events = mock_client(&server).multiget(&hrefs).await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].href, "/cal/a&b.ics");
        assert_eq!(events[0].etag, "\"e1\"");
        assert_eq!(events[0].calendar_data, ICS);

        let sent = body(&requests(&server).await[0]);
        assert!(sent.contains("<d:href>/cal/a&amp;b.ics</d:href>"), "{sent}");
        assert!(sent.contains("<d:href>/cal/missing.ics</d:href>"), "{sent}");
    }

    #[tokio::test]
    async fn test_multiget_error_status() {
        let server = MockServer::start().await;
        Mock::given(method("REPORT"))
            .respond_with(ResponseTemplate::new(400))
            .mount(&server)
            .await;

        let err = mock_client(&server)
            .multiget(&["/cal/a.ics".to_string()])
            .await
            .unwrap_err();
        assert!(err.to_string().contains("calendar-multiget"), "{err}");
    }

    #[tokio::test]
    async fn test_sync_collection_initial() {
        let server = MockServer::start().await;
        Mock::given(method("REPORT"))
            .and(path("/cal"))
            .and(body_string_contains("<d:sync-token/>"))
            .respond_with(multistatus(&format!(
                r#"<d:response><d:href>/cal/a.ics</d:href><d:propstat><d:prop>
<d:getetag>"ea"</d:getetag><c:calendar-data>{ICS}</c:calendar-data>
</d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
<d:response><d:href>/cal/b.ics</d:href><d:propstat><d:prop><d:getetag>"eb"</d:getetag></d:prop>
<d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
<d:response><d:href>/cal/gone.ics</d:href><d:status>HTTP/1.1 404 Not Found</d:status></d:response>
<d:sync-token>http://example.com/sync/1</d:sync-token>"#
            )))
            .expect(1)
            .mount(&server)
            .await;

        let resp = mock_client(&server).sync_collection(None).await.unwrap();
        assert_eq!(resp.sync_token, "http://example.com/sync/1");
        assert_eq!(resp.changed.len(), 1);
        assert_eq!(resp.changed[0].href, "/cal/a.ics");
        assert_eq!(resp.changed[0].calendar_data, ICS);
        assert_eq!(resp.pending_hrefs, vec!["/cal/b.ics"]);
        assert_eq!(resp.removed, vec!["/cal/gone.ics"]);
    }

    /// Radicale puts the sync-token before the responses.
    #[tokio::test]
    async fn test_sync_collection_token_before_responses() {
        let server = MockServer::start().await;
        Mock::given(method("REPORT"))
            .and(path("/cal"))
            .respond_with(multistatus(
                r#"<d:sync-token>http://radicale.org/ns/sync/abc</d:sync-token><d:response><d:href>/cal/b.ics</d:href><d:propstat><d:prop><d:getetag>"eb"</d:getetag></d:prop>
<d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>"#,
            ))
            .mount(&server)
            .await;

        let resp = mock_client(&server).sync_collection(None).await.unwrap();
        assert_eq!(resp.sync_token, "http://radicale.org/ns/sync/abc");
        assert_eq!(resp.pending_hrefs, vec!["/cal/b.ics"]);
    }

    #[tokio::test]
    async fn test_sync_collection_hrefs_asks_for_no_calendar_data() {
        let server = MockServer::start().await;
        Mock::given(method("REPORT"))
            .and(path("/cal"))
            .respond_with(multistatus(
                r#"<d:response><d:href>/cal/b.ics</d:href><d:propstat><d:prop><d:getetag>"eb"</d:getetag></d:prop>
<d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
<d:sync-token>http://example.com/sync/2</d:sync-token>"#,
            ))
            .expect(1)
            .mount(&server)
            .await;

        let resp = mock_client(&server)
            .sync_collection_hrefs(Some("http://example.com/sync/1"))
            .await
            .unwrap();
        assert_eq!(resp.sync_token, "http://example.com/sync/2");
        assert_eq!(resp.pending_hrefs, vec!["/cal/b.ics"]);
        let body = String::from_utf8(requests(&server).await[0].body.clone()).unwrap();
        assert!(
            body.contains("<d:getetag/>") && !body.contains("calendar-data"),
            "{body}"
        );
    }

    #[tokio::test]
    async fn test_sync_collection_follows_truncated_results() {
        let server = MockServer::start().await;
        let event = |name: &str| {
            format!(
                r#"<d:response><d:href>/cal/{name}.ics</d:href><d:propstat><d:prop>
<d:getetag>"e"</d:getetag><c:calendar-data>{ICS}</c:calendar-data>
</d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>"#
            )
        };
        let truncated = r#"<d:response><d:href>/cal/</d:href>
<d:status>HTTP/1.1 507 Insufficient Storage</d:status></d:response>"#;
        Mock::given(method("REPORT"))
            .and(body_string_contains("<d:sync-token/>"))
            .respond_with(multistatus(&format!(
                "{}{truncated}<d:sync-token>t1</d:sync-token>",
                event("a")
            )))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("REPORT"))
            .and(body_string_contains("<d:sync-token>t1</d:sync-token>"))
            .respond_with(multistatus(&format!(
                "{}<d:sync-token>t2</d:sync-token>",
                event("b")
            )))
            .expect(1)
            .mount(&server)
            .await;

        let resp = mock_client(&server).sync_collection(None).await.unwrap();
        let hrefs: Vec<&str> = resp.changed.iter().map(|e| e.href.as_str()).collect();
        assert_eq!(hrefs, ["/cal/a.ics", "/cal/b.ics"]);
        assert!(resp.pending_hrefs.is_empty(), "{:?}", resp.pending_hrefs);
        assert_eq!(resp.sync_token, "t2");
    }

    #[tokio::test]
    async fn test_html_instead_of_multistatus_is_an_error() {
        let server = MockServer::start().await;
        Mock::given(method("PROPFIND"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("<html><body>Please log in</body></html>"),
            )
            .mount(&server)
            .await;
        let err = mock_client(&server).list_events().await.unwrap_err();
        assert!(err.to_string().contains("no WebDAV multistatus"), "{err}");
    }

    #[tokio::test]
    async fn test_sync_collection_incremental_escapes_token() {
        let server = MockServer::start().await;
        Mock::given(method("REPORT"))
            .and(body_string_contains(
                "<d:sync-token>http://x/t?a=1&amp;b=&lt;2&gt;</d:sync-token>",
            ))
            .respond_with(multistatus(
                "<d:sync-token>http://x/t?a=2&amp;b=3</d:sync-token>",
            ))
            .expect(1)
            .mount(&server)
            .await;

        let resp = mock_client(&server)
            .sync_collection(Some("http://x/t?a=1&b=<2>"))
            .await
            .unwrap();
        assert_eq!(resp.sync_token, "http://x/t?a=2&b=3");
        assert!(resp.changed.is_empty() && resp.pending_hrefs.is_empty());
        assert!(resp.removed.is_empty());
    }

    #[tokio::test]
    async fn test_sync_collection_invalid_token_statuses() {
        for status in [403u16, 409] {
            let server = MockServer::start().await;
            Mock::given(method("REPORT"))
                .respond_with(ResponseTemplate::new(status))
                .expect(1)
                .mount(&server)
                .await;
            let err = mock_client(&server)
                .sync_collection(Some("stale"))
                .await
                .unwrap_err();
            assert!(matches!(err, Error::SyncTokenExpired), "{status}: {err}");
        }
    }

    #[tokio::test]
    async fn test_sync_collection_unsupported() {
        let server = MockServer::start().await;
        Mock::given(method("REPORT"))
            .respond_with(ResponseTemplate::new(501))
            .expect(1)
            .mount(&server)
            .await;
        let err = mock_client(&server)
            .sync_collection(None)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::CalDav(_)), "{err}");
    }

    #[tokio::test]
    async fn test_sync_collection_propstat_404_is_not_removal() {
        let server = MockServer::start().await;
        Mock::given(method("REPORT"))
            .respond_with(multistatus(
                r#"<d:response><d:href>/cal/a.ics</d:href>
<d:propstat><d:prop><c:calendar-data/></d:prop><d:status>HTTP/1.1 404 Not Found</d:status></d:propstat>
<d:propstat><d:prop><d:getetag>"ea"</d:getetag></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>
</d:response>
<d:sync-token>t2</d:sync-token>"#,
            ))
            .mount(&server)
            .await;

        let resp = mock_client(&server).sync_collection(None).await.unwrap();
        assert!(resp.removed.is_empty(), "{:?}", resp.removed);
        assert_eq!(resp.pending_hrefs, vec!["/cal/a.ics"]);
    }

    #[tokio::test]
    async fn test_put_event_unconditional() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/cal/new.ics"))
            .and(header("Content-Type", "text/calendar; charset=utf-8"))
            .and(basic_auth("u", "p"))
            .respond_with(ResponseTemplate::new(201).insert_header("ETag", "\"n1\""))
            .expect(1)
            .mount(&server)
            .await;

        let etag = mock_client(&server)
            .put_event("/cal/new.ics", ICS, None)
            .await
            .unwrap();
        assert_eq!(etag, "\"n1\"");
        let req = &requests(&server).await[0];
        assert_eq!(body(req), ICS);
        assert!(req.headers.get("If-Match").is_none());
        assert!(req.headers.get("If-None-Match").is_none());
    }

    #[tokio::test]
    async fn test_put_event_with_etag_sends_if_match() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/cal/x.ics"))
            .and(header("If-Match", "\"old\""))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;

        let etag = mock_client(&server)
            .put_event("x.ics", ICS, Some("\"old\""))
            .await
            .unwrap();
        assert_eq!(etag, "");
    }

    #[tokio::test]
    async fn test_put_event_error_status() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .respond_with(ResponseTemplate::new(412).set_body_string("precondition"))
            .mount(&server)
            .await;

        let err = mock_client(&server)
            .put_event("/cal/x.ics", ICS, None)
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("412") && msg.contains("precondition"), "{msg}");
    }

    #[tokio::test]
    async fn test_delete_event_statuses() {
        for (status, ok) in [(204u16, true), (404, true), (403, false)] {
            let server = MockServer::start().await;
            Mock::given(method("DELETE"))
                .and(path("/cal/x.ics"))
                .and(basic_auth("u", "p"))
                .respond_with(ResponseTemplate::new(status))
                .expect(1)
                .mount(&server)
                .await;
            let res = mock_client(&server).delete_event("/cal/x.ics").await;
            assert_eq!(res.is_ok(), ok, "status {status}: {res:?}");
        }
    }

    #[tokio::test]
    async fn test_retry_respects_retry_after() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .respond_with(ResponseTemplate::new(503).insert_header("Retry-After", "0"))
            .up_to_n_times(2)
            .expect(2)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;

        let start = std::time::Instant::now();
        mock_client(&server).delete_event("x.ics").await.unwrap();
        // Retry-After: 0 must override the 1s/2s exponential backoff
        assert!(start.elapsed() < std::time::Duration::from_millis(900));
    }

    #[tokio::test]
    async fn test_request_timeout_is_retried() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .respond_with(ResponseTemplate::new(408).insert_header("Retry-After", "0"))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;

        mock_client(&server).delete_event("x.ics").await.unwrap();
    }

    #[tokio::test]
    async fn test_retry_gives_up_after_max_retries() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "0"))
            .expect(6)
            .mount(&server)
            .await;

        let err = mock_client(&server)
            .delete_event("x.ics")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("429"), "{err}");
    }

    #[tokio::test]
    async fn test_retry_exponential_backoff_without_retry_after() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .respond_with(ResponseTemplate::new(502))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .respond_with(ResponseTemplate::new(201))
            .mount(&server)
            .await;

        let start = std::time::Instant::now();
        fast_client(&server.uri(), None)
            .put_event("x.ics", ICS, None)
            .await
            .unwrap();
        assert!(start.elapsed() >= FAST_RETRY.base_delay);
        let reqs = requests(&server).await;
        assert_eq!(reqs.len(), 2);
        // Body is rebuilt for the retry
        assert_eq!(body(&reqs[1]), ICS);
    }

    #[test]
    fn test_backoff_grows_exponentially_and_caps() {
        let policy = RetryPolicy::default();
        let secs: Vec<u64> = (0..7).map(|a| policy.backoff(a).as_secs()).collect();
        assert_eq!(secs, vec![1, 2, 4, 8, 16, 16, 16]);
        assert_eq!(policy.max_retries, 5);
        assert_eq!(FAST_RETRY.backoff(3), Duration::from_millis(8));
    }

    #[tokio::test]
    async fn test_retry_connect_error_gives_up_after_max_retries() {
        // Bind and drop a listener to get a local port that refuses connections
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        // Timeout so a dropped (instead of refused) connect cannot hang the suite
        let client = fast_client(
            &format!("http://127.0.0.1:{port}"),
            Some(Duration::from_millis(500)),
        );

        let start = std::time::Instant::now();
        let err = client.delete_event("x.ics").await.unwrap_err();
        assert!(matches!(&err, Error::Http(e) if e.is_connect()), "{err}");
        // All five backoffs (1+2+4+8+16 ms) were slept before giving up
        assert!(start.elapsed() >= Duration::from_millis(31));
    }

    #[tokio::test]
    async fn test_retry_timeout_recovers_after_transient_failures() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .respond_with(ResponseTemplate::new(204).set_delay(Duration::from_millis(500)))
            .up_to_n_times(2)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;

        fast_client(&server.uri(), Some(Duration::from_millis(50)))
            .delete_event("x.ics")
            .await
            .unwrap();
        assert_eq!(requests(&server).await.len(), 3);
    }

    #[tokio::test]
    async fn test_retry_timeout_gives_up_after_max_retries() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .respond_with(ResponseTemplate::new(204).set_delay(Duration::from_millis(500)))
            .expect(6)
            .mount(&server)
            .await;

        let err = fast_client(&server.uri(), Some(Duration::from_millis(20)))
            .delete_event("x.ics")
            .await
            .unwrap_err();
        assert!(matches!(&err, Error::Http(e) if e.is_timeout()), "{err}");
    }

    fn principal_response(href: &str) -> ResponseTemplate {
        multistatus(&format!(
            r#"<d:response><d:href>{href}</d:href><d:propstat><d:prop>
<d:current-user-principal><d:href>/principals/alice/</d:href></d:current-user-principal>
</d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>"#
        ))
    }

    async fn mount_home_and_calendars(server: &MockServer) {
        Mock::given(method("PROPFIND"))
            .and(path("/principals/alice/"))
            .and(body_string_contains("calendar-home-set"))
            .respond_with(multistatus(
                r#"<d:response><d:href>/principals/alice/</d:href><d:propstat><d:prop>
<c:calendar-home-set><d:href>/calendars/alice/</d:href></c:calendar-home-set>
</d:prop></d:propstat></d:response>"#,
            ))
            .mount(server)
            .await;
        Mock::given(method("PROPFIND"))
            .and(path("/calendars/alice/"))
            .and(header("Depth", "1"))
            .respond_with(multistatus(
                r#"<d:response><d:href>/calendars/alice/</d:href><d:propstat><d:prop>
<d:resourcetype><d:collection/></d:resourcetype></d:prop></d:propstat></d:response>
<d:response><d:href>/calendars/alice/work/</d:href><d:propstat><d:prop>
<d:displayname>Work</d:displayname>
<d:resourcetype><d:collection/><c:calendar/></d:resourcetype>
<ic:calendar-color>#FF0000</ic:calendar-color>
</d:prop></d:propstat></d:response>"#,
            ))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn test_discover_calendars_via_well_known_redirect() {
        let server = MockServer::start().await;
        Mock::given(path("/.well-known/caldav"))
            .respond_with(
                ResponseTemplate::new(301)
                    .insert_header("Location", format!("{}/principals/alice/", server.uri())),
            )
            .mount(&server)
            .await;
        Mock::given(path_regex("^/principals/alice/?$"))
            .and(body_string_contains("current-user-principal"))
            .and(basic_auth("u", "p"))
            .respond_with(principal_response("/principals/alice/"))
            .mount(&server)
            .await;
        mount_home_and_calendars(&server).await;

        let cals = CalDavClient::discover_calendars(&server.uri(), "u", "p")
            .await
            .unwrap();
        assert_eq!(cals.len(), 1);
        assert_eq!(cals[0].href, "/calendars/alice/work/");
        assert_eq!(
            cals[0].url,
            format!("{}/calendars/alice/work/", server.uri())
        );
        assert_eq!(cals[0].display_name.as_deref(), Some("Work"));
        assert_eq!(cals[0].color.as_deref(), Some("#FF0000"));
    }

    #[tokio::test]
    async fn test_discover_calendars_missing_principal() {
        let server = MockServer::start().await;
        Mock::given(method("PROPFIND"))
            .respond_with(multistatus(""))
            .mount(&server)
            .await;

        let err = CalDavClient::discover_calendars(&server.uri(), "u", "p")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("current-user-principal"), "{err}");
    }

    #[tokio::test]
    async fn test_discover_calendars_uses_principal_href() {
        let server = MockServer::start().await;
        Mock::given(path("/.well-known/caldav"))
            .respond_with(
                ResponseTemplate::new(301)
                    .insert_header("Location", format!("{}/dav/", server.uri())),
            )
            .mount(&server)
            .await;
        Mock::given(path_regex("^/dav/?$"))
            .respond_with(principal_response("/dav/"))
            .mount(&server)
            .await;
        mount_home_and_calendars(&server).await;

        let cals = CalDavClient::discover_calendars(&server.uri(), "u", "p")
            .await
            .unwrap();
        assert_eq!(cals.len(), 1);
    }

    #[tokio::test]
    async fn test_discover_calendars_without_well_known() {
        let server = MockServer::start().await;
        Mock::given(path("/.well-known/caldav"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        Mock::given(path_regex("^/principals/alice/?$"))
            .and(body_string_contains("current-user-principal"))
            .respond_with(principal_response("/principals/alice/"))
            .mount(&server)
            .await;
        mount_home_and_calendars(&server).await;

        let base = format!("{}/principals/alice", server.uri());
        let cals = CalDavClient::discover_calendars(&base, "u", "p")
            .await
            .unwrap();
        assert_eq!(cals.len(), 1);
    }

    /// A server whose calendar home lists `home_listing`.
    async fn discovery_server(home_listing: &str) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(path("/.well-known/caldav"))
            .respond_with(
                ResponseTemplate::new(301)
                    .insert_header("Location", format!("{}/principals/alice/", server.uri())),
            )
            .mount(&server)
            .await;
        Mock::given(path_regex("^/principals/alice/?$"))
            .and(body_string_contains("current-user-principal"))
            .respond_with(principal_response("/principals/alice/"))
            .mount(&server)
            .await;
        Mock::given(path_regex("^/principals/alice/?$"))
            .and(body_string_contains("calendar-home-set"))
            .respond_with(multistatus(
                r#"<d:response><d:href>/principals/alice/</d:href><d:propstat><d:prop>
<c:calendar-home-set><d:href>/calendars/alice/</d:href></c:calendar-home-set>
</d:prop></d:propstat></d:response>"#,
            ))
            .mount(&server)
            .await;
        Mock::given(method("PROPFIND"))
            .and(path("/calendars/alice/"))
            .respond_with(multistatus(home_listing))
            .mount(&server)
            .await;

        server
    }

    #[tokio::test]
    async fn test_discover_calendars_skips_non_calendar_collections() {
        let server = discovery_server(
            r#"<d:response><d:href>/calendars/alice/inbox/</d:href><d:propstat><d:prop>
<d:resourcetype><d:collection/><c:schedule-inbox/></d:resourcetype></d:prop></d:propstat></d:response>"#,
        )
        .await;
        let cals = CalDavClient::discover_calendars(&server.uri(), "u", "p")
            .await
            .unwrap();
        assert!(cals.is_empty(), "{cals:?}");
    }

    #[tokio::test]
    async fn test_discovery_reports_writability_and_skips_task_lists() {
        let server = discovery_server(
            r#"<d:response><d:href>/calendars/alice/work/</d:href><d:propstat><d:prop>
<d:resourcetype><d:collection/><c:calendar/></d:resourcetype>
<d:current-user-privilege-set><d:privilege><d:read/></d:privilege><d:privilege><d:write/></d:privilege></d:current-user-privilege-set>
<c:supported-calendar-component-set><c:comp name="VEVENT"/><c:comp name="VTODO"/></c:supported-calendar-component-set>
</d:prop></d:propstat></d:response>
<d:response><d:href>/calendars/alice/shared/</d:href><d:propstat><d:prop>
<d:resourcetype><d:collection/><c:calendar/></d:resourcetype>
<d:current-user-privilege-set><d:privilege><d:read/></d:privilege></d:current-user-privilege-set>
</d:prop></d:propstat></d:response>
<d:response><d:href>/calendars/alice/tasks/</d:href><d:propstat><d:prop>
<d:resourcetype><d:collection/><c:calendar/></d:resourcetype>
<c:supported-calendar-component-set><c:comp name="VTODO"/></c:supported-calendar-component-set>
</d:prop></d:propstat></d:response>
<d:response><d:href>/calendars/alice/owned/</d:href><d:propstat><d:prop>
<d:resourcetype><d:collection/><c:calendar/></d:resourcetype>
<d:current-user-privilege-set><d:privilege><d:all/></d:privilege></d:current-user-privilege-set>
</d:prop></d:propstat></d:response>
<d:response><d:href>/calendars/alice/plain/</d:href><d:propstat><d:prop>
<d:resourcetype><d:collection/><c:calendar/></d:resourcetype>
</d:prop></d:propstat></d:response>"#,
        )
        .await;
        let cals = CalDavClient::discover_calendars(&server.uri(), "u", "p")
            .await
            .unwrap();
        let found: Vec<(&str, bool)> = cals.iter().map(|c| (c.href.as_str(), c.writable)).collect();
        assert_eq!(
            found,
            [
                ("/calendars/alice/work/", true),
                ("/calendars/alice/shared/", false),
                ("/calendars/alice/owned/", true),
                ("/calendars/alice/plain/", true),
            ]
        );
    }

    #[tokio::test]
    async fn test_discovery_asks_for_privileges_and_components() {
        let server = discovery_server("").await;
        CalDavClient::discover_calendars(&server.uri(), "u", "p")
            .await
            .unwrap();
        let requests = server.received_requests().await.unwrap();
        let listing = String::from_utf8_lossy(&requests.last().unwrap().body).to_string();
        assert!(listing.contains("current-user-privilege-set"), "{listing}");
        assert!(
            listing.contains("supported-calendar-component-set"),
            "{listing}"
        );
    }
}
