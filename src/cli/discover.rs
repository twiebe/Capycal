use std::collections::HashSet;

use crate::caldav::client::{CalDavClient, DiscoveredCalendar};
use crate::config::calendar_key;
use crate::eventkit::CalendarInfo;

use super::{CliError, CliResult};

/// The calendar name as a TOML key, quoted when it isn't a bare key.
fn toml_key(display_name: &str) -> String {
    quote_key(&calendar_key(display_name))
}

fn quote_key(name: &str) -> String {
    if name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        name.to_string()
    } else {
        toml_string(name)
    }
}

/// An environment variable name part: ASCII only, German umlauts spelled out.
fn env_name(name: &str) -> String {
    let mut out = String::new();
    for c in calendar_key(name).chars() {
        match c {
            'ä' => out.push_str("AE"),
            'ö' => out.push_str("OE"),
            'ü' => out.push_str("UE"),
            'ß' => out.push_str("SS"),
            c if c.is_ascii_alphanumeric() => out.push(c.to_ascii_uppercase()),
            _ => out.push('_'),
        }
    }
    out
}

/// A TOML string literal, escaped as needed.
fn toml_string(value: &str) -> String {
    toml::Value::String(value.to_string()).to_string()
}

/// Prompt for credentials and print the calendars found on a CalDAV server.
pub async fn caldav(url: &str) -> CliResult {
    eprint!("username: ");
    let mut username = String::new();
    std::io::stdin()
        .read_line(&mut username)
        .map_err(CliError::msg)?;
    let username = username.trim();

    let password = rpassword::prompt_password("password: ").map_err(CliError::msg)?;

    print!("{}", discover_snippets(url, username, &password).await?);
    Ok(())
}

/// Discover the calendars on a CalDAV server, list them on stderr and return TOML snippets.
async fn discover_snippets(url: &str, username: &str, password: &str) -> Result<String, CliError> {
    eprintln!("discovering calendars on {url} ...");

    let calendars = CalDavClient::discover_calendars(url, username, password).await?;
    if calendars.is_empty() {
        return Err(CliError::Stderr("no calendars found".to_string()));
    }
    eprintln!("found {} calendar(s):\n", calendars.len());
    for cal in &calendars {
        let name = cal.display_name.as_deref().unwrap_or("(unnamed)");
        let color = cal.color.as_deref().unwrap_or("");
        eprintln!("  {name}");
        eprintln!("    url = \"{}\"", cal.url);
        if !color.is_empty() {
            eprintln!("    color = {color}");
        }
        eprintln!();
    }
    eprintln!("--- TOML snippets ---\n");
    Ok(caldav_snippets(url, &calendars, username))
}

/// An account name from the server's host: `cloud.example.org` becomes
/// `cloud_example_org`.
fn account_name(url: &str) -> String {
    match reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
    {
        Some(host) => calendar_key(&host.replace('.', " ")),
        None => "account".to_string(),
    }
}

/// An `[accounts.<name>]` snippet for the server and a `[calendars.<name>]`
/// snippet per calendar, named after its display name.
fn caldav_snippets(url: &str, calendars: &[DiscoveredCalendar], username: &str) -> String {
    let account = account_name(url);
    let mut snippets = format!(
        "[accounts.{}]\nurl = {}\nusername = {}\npassword = {{ env = \"CAPYCAL_{}_PASSWORD\" }}\n\n",
        quote_key(&account),
        toml_string(url),
        toml_string(username),
        env_name(&account)
    );
    for cal in calendars {
        let display_name = cal.display_name.as_deref().unwrap_or("calendar");
        snippets.push_str(&format!(
            "[calendars.{}]\naccount = {}\nurl = {}\n\n",
            toml_key(display_name),
            toml_string(&account),
            toml_string(&cal.url)
        ));
    }
    snippets
}

/// The calendars of a CalDAV account that hold events, for the desktop
/// apps' calendar picker.
pub async fn discover_caldav_calendars(
    url: &str,
    username: &str,
    password: &str,
) -> crate::Result<Vec<DiscoveredCalendar>> {
    CalDavClient::discover_calendars(url, username, password).await
}

/// Print the calendars known to macOS Calendar.app.
#[cfg(target_os = "macos")]
pub fn eventkit() -> CliResult {
    let calendars = crate::eventkit::list_calendars()?;
    if calendars.is_empty() {
        return Err(CliError::Stderr(
            "no calendars found in Calendar.app".to_string(),
        ));
    }
    eprint!("{}", eventkit_listing(&calendars));
    print!("{}", eventkit_snippets(&calendars));
    Ok(())
}

/// Heading of the calendars without a source (account).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const NO_SOURCE: &str = "Other";

/// The calendars grouped by source (account), sources and calendars sorted
/// by name; calendars without a source come last. Each calendar comes with a
/// unique `<account>_<name>` config key.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn by_source(calendars: &[CalendarInfo]) -> Vec<(&str, Vec<(String, &CalendarInfo)>)> {
    let mut sorted: Vec<&CalendarInfo> = calendars.iter().collect();
    sorted.sort_by_key(|c| {
        (
            c.source.is_none(),
            c.source.as_deref().map(str::to_lowercase),
            c.title.to_lowercase(),
        )
    });
    let mut keys = HashSet::new();
    let mut groups: Vec<(&str, Vec<(String, &CalendarInfo)>)> = Vec::new();
    for cal in sorted {
        let account = cal.source.as_deref().unwrap_or("local");
        let base = calendar_key(&format!("{account} {}", cal.title));
        let key = (1..)
            .map(|n| match n {
                1 => base.clone(),
                n => format!("{base}_{n}"),
            })
            .find(|key| keys.insert(key.clone()))
            .map(|key| quote_key(&key))
            .unwrap_or_default();
        let source = cal.source.as_deref().unwrap_or(NO_SOURCE);
        match groups.last_mut() {
            Some((last, group)) if *last == source => group.push((key, cal)),
            _ => groups.push((source, vec![(key, cal)])),
        }
    }
    groups
}

/// The calendar list of `--discover-eventkit`, ending with the snippets header.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn eventkit_listing(calendars: &[CalendarInfo]) -> String {
    let mut text = format!("found {} calendar(s) in Calendar.app:\n", calendars.len());
    for (source, group) in by_source(calendars) {
        text.push_str(&format!("\n{source}\n"));
        for (_, cal) in group {
            text.push_str(&format!("  {} (id: {})\n", cal.title, cal.identifier));
        }
    }
    text.push_str("\n--- TOML snippets ---\n\n");
    text
}

/// The `[calendars.<account>_<name>]` snippets of `--discover-eventkit` under a
/// comment per source, using the identifier with the "Account/Name" form as
/// a comment.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn eventkit_snippets(calendars: &[CalendarInfo]) -> String {
    let mut text = String::new();
    for (source, group) in by_source(calendars) {
        text.push_str(&format!("# {source}\n\n"));
        for (key, cal) in group {
            let account = cal.source.as_deref().unwrap_or("local");
            text.push_str(&format!(
                "[calendars.{key}]\neventkit_calendar = {}\n# eventkit_calendar = {}\n\n",
                toml_string(&cal.identifier),
                toml_string(&format!("{account}/{}", cal.title))
            ));
        }
    }
    text
}

#[cfg(not(target_os = "macos"))]
pub fn eventkit() -> CliResult {
    Err(CliError::msg(
        "--discover-eventkit is only available on macOS",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_toml_key() {
        assert_eq!(toml_key("Work"), "work");
        assert_eq!(toml_key("My Calendar"), "my_calendar");
        assert_eq!(toml_key("team-cal"), "team-cal");
        assert_eq!(toml_key("Ärzte & Co."), "\"ärzte_co\"");
        assert_eq!(toml_key("iCloud/Privat"), "icloudprivat");
        assert_eq!(toml_key("Café [Paris]"), "\"café_paris\"");
        assert_eq!(toml_key("  Team   Cal  "), "team_cal");
        assert_eq!(toml_key("&&&"), "calendar");
        assert_eq!(env_name("Ärzte & Co."), "AERZTE_CO");
        assert_eq!(env_name("Straße"), "STRASSE");

        // A quoted key loads and the name is usable as a sync source
        let toml = format!(
            "[calendars.{}]\nurl = \"https://example.com/a/\"\n\
             [calendars.b]\nurl = \"https://example.com/b/\"\n\
             [syncs.s]\nsource = \"ärzte_co\"\ntarget = \"b\"\ninterval = \"5m\"\nfields = [\"DTSTART\"]\n",
            toml_key("Ärzte & Co.")
        );
        let config: crate::config::Config = toml::from_str(&toml).unwrap();
        assert!(config.calendars.contains_key("ärzte_co"));
    }

    fn cal(title: &str, source: Option<&str>, identifier: &str) -> CalendarInfo {
        CalendarInfo {
            title: title.to_string(),
            source: source.map(String::from),
            identifier: identifier.to_string(),
            writable: true,
        }
    }

    #[test]
    fn test_eventkit_listing_and_snippets() {
        let calendars = [
            cal("Home \"main\"", None, "ID-2"),
            cal("Work", Some("Exchange"), "ID-1"),
            cal("Privat", Some("iCloud"), "ID-3"),
            cal("Family", Some("iCloud"), "ID-4"),
        ];
        // Grouped by source, sorted by name, calendars without source last
        assert_eq!(
            eventkit_listing(&calendars),
            "found 4 calendar(s) in Calendar.app:\n\nExchange\n  Work (id: ID-1)\n\n\
             iCloud\n  Family (id: ID-4)\n  Privat (id: ID-3)\n\n\
             Other\n  Home \"main\" (id: ID-2)\n\n--- TOML snippets ---\n\n"
        );
        let snippets = eventkit_snippets(&calendars);
        assert!(snippets.starts_with(
            "# Exchange\n\n[calendars.exchange_work]\neventkit_calendar = \"ID-1\"\n\
             # eventkit_calendar = \"Exchange/Work\"\n\n# iCloud\n\n[calendars.icloud_family]\n"
        ));
        let parsed: toml::Table = toml::from_str(&snippets).unwrap();
        assert_eq!(
            parsed["calendars"]["local_home_main"]["eventkit_calendar"].as_str(),
            Some("ID-2")
        );
        // Equal names in different accounts or the same one get unique keys
        let calendars = [
            cal("Calendar", Some("iCloud"), "ID-1"),
            cal("Calendar", Some("Exchange"), "ID-2"),
            cal("Calendar", Some("Exchange"), "ID-3"),
        ];
        let parsed: toml::Table = toml::from_str(&eventkit_snippets(&calendars)).unwrap();
        let keys: Vec<&String> = parsed["calendars"].as_table().unwrap().keys().collect();
        assert_eq!(
            keys,
            [
                "exchange_calendar",
                "exchange_calendar_2",
                "icloud_calendar"
            ]
        );
    }

    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn multistatus(inner: &str) -> ResponseTemplate {
        ResponseTemplate::new(207).set_body_string(format!(
            r#"<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">{inner}</d:multistatus>"#
        ))
    }

    /// Serve the principal, home-set and calendar-list PROPFIND chain for `calendars`.
    async fn server_with_calendars(calendars: &str) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("PROPFIND"))
            .and(path("/principals/alice"))
            .and(body_string_contains("current-user-principal"))
            .respond_with(multistatus(
                "<d:response><d:href>/principals/alice/</d:href><d:propstat><d:prop>\
                 <d:current-user-principal><d:href>/principals/alice/</d:href></d:current-user-principal>\
                 </d:prop></d:propstat></d:response>",
            ))
            .mount(&server)
            .await;
        Mock::given(method("PROPFIND"))
            .and(path("/principals/alice/"))
            .and(body_string_contains("calendar-home-set"))
            .respond_with(multistatus(
                "<d:response><d:href>/principals/alice/</d:href><d:propstat><d:prop>\
                 <c:calendar-home-set><d:href>/calendars/alice/</d:href></c:calendar-home-set>\
                 </d:prop></d:propstat></d:response>",
            ))
            .mount(&server)
            .await;
        Mock::given(method("PROPFIND"))
            .and(path("/calendars/alice/"))
            .respond_with(multistatus(calendars))
            .mount(&server)
            .await;
        server
    }

    fn calendar(href: &str, name: &str) -> String {
        format!(
            "<d:response><d:href>{href}</d:href><d:propstat><d:prop>{name}\
             <d:resourcetype><d:collection/><c:calendar/></d:resourcetype>\
             </d:prop></d:propstat></d:response>"
        )
    }

    #[tokio::test]
    async fn test_discover_snippets_formats_toml() {
        let cals = calendar(
            "/calendars/alice/team/",
            "<d:displayname>Team Work</d:displayname>",
        ) + &calendar("/calendars/alice/x/", "");
        let server = server_with_calendars(&cals).await;
        let base = format!("{}/principals/alice", server.uri());

        let snippets = discover_snippets(&base, "alice", "secret").await.unwrap();
        let uri = server.uri();
        assert_eq!(
            snippets,
            format!(
                "[accounts.127_0_0_1]\nurl = \"{base}\"\nusername = \"alice\"\n\
                 password = {{ env = \"CAPYCAL_127_0_0_1_PASSWORD\" }}\n\n\
                 [calendars.team_work]\naccount = \"127_0_0_1\"\nurl = \"{uri}/calendars/alice/team/\"\n\n\
                 [calendars.calendar]\naccount = \"127_0_0_1\"\nurl = \"{uri}/calendars/alice/x/\"\n\n"
            )
        );
        let config: crate::config::Config = toml::from_str(&snippets).unwrap();
        config.validate().unwrap();
        assert_eq!(config.calendars.len(), 2);
    }

    #[test]
    fn test_account_name_from_the_host() {
        assert_eq!(
            account_name("https://cloud.example.org/remote.php/dav"),
            "cloud_example_org"
        );
        assert_eq!(account_name("http://127.0.0.1:5232/"), "127_0_0_1");
        assert_eq!(account_name("not a url"), "account");
    }

    #[tokio::test]
    async fn test_discover_snippets_without_calendars() {
        let server = server_with_calendars("").await;
        let base = format!("{}/principals/alice", server.uri());

        let CliError::Stderr(msg) = discover_snippets(&base, "u", "p").await.unwrap_err() else {
            panic!("expected stderr error");
        };
        assert_eq!(msg, "no calendars found");
    }

    #[tokio::test]
    async fn test_discover_snippets_reports_discovery_error() {
        let server = MockServer::start().await;

        let CliError::Stderr(msg) = discover_snippets(&server.uri(), "u", "p")
            .await
            .unwrap_err()
        else {
            panic!("expected stderr error");
        };
        assert!(msg.starts_with("error: "), "{msg}");
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn test_eventkit_unavailable_off_macos() {
        let CliError::Stderr(msg) = eventkit().unwrap_err() else {
            panic!("expected stderr error");
        };
        assert_eq!(msg, "error: --discover-eventkit is only available on macOS");
    }
}
