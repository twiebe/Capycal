//! The calendar picker of the sync form: the calendars of each account, of
//! Calendar.app and the configured feeds, matched with the `[calendars]`
//! entries of the config.

use std::collections::HashMap;

use capycal::{CalendarConfig, CalendarInfo, Config, DiscoveredCalendar, calendar_key};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Source,
    Target,
}

#[derive(Debug, Clone)]
pub enum Listing<T> {
    Loading,
    Loaded(Vec<T>),
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickerItem {
    pub label: String,
    /// The calendar's own name, without where it is.
    pub title: String,
    /// The Calendar.app account (source) it belongs to.
    pub section: Option<String>,
    pub enabled: bool,
    pub hint: Option<&'static str>,
    pub selected: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickerGroup {
    pub title: String,
    pub items: Vec<PickerItem>,
    /// "Loading…" or why the calendars could not be listed.
    pub status: Option<String>,
}

/// A picked calendar: an existing entry or a new one to add.
#[derive(Debug, Clone, PartialEq)]
pub struct CalendarChoice {
    pub key: String,
    pub calendar: CalendarConfig,
    pub new_entry: bool,
}

/// One item and what choosing it means.
struct Entry {
    label: String,
    title: String,
    section: Option<String>,
    writable: bool,
    hint: Option<&'static str>,
    /// The configured entry it is.
    key: Option<String>,
    /// Key proposal and entry for a calendar not configured yet.
    proposal: (String, CalendarConfig),
}

pub struct CalendarPicker {
    accounts: Vec<(String, Listing<DiscoveredCalendar>)>,
    calendar_app: Option<Listing<CalendarInfo>>,
    configured: HashMap<String, CalendarConfig>,
}

pub const CALENDAR_APP: &str = "Calendar.app";
pub const FEEDS: &str = "Feeds";

pub const FEED_INVALID: &str = "Paste the address, starting with https:// or webcal://";

/// Whether `url` can be a feed's address.
pub fn feed_url_valid(url: &str) -> bool {
    reqwest::Url::parse(url.trim()).is_ok_and(|u| {
        matches!(u.scheme(), "http" | "https" | "webcal" | "webcals") && u.host_str().is_some()
    })
}

/// A key for a new feed: the name of its site, "google" for
/// calendar.google.com.
pub fn feed_key(url: &str) -> String {
    let host = reqwest::Url::parse(url.trim())
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_default();
    let labels: Vec<&str> = host.split('.').collect();
    match labels.len() {
        n if n >= 2 && labels[n - 2].chars().any(|c| c.is_alphabetic()) => {
            calendar_key(labels[n - 2])
        }
        _ => "feed".to_string(),
    }
}

fn same_url(a: &str, b: &str) -> bool {
    a.trim_end_matches('/') == b.trim_end_matches('/')
}

impl CalendarPicker {
    pub fn new(config: &Config, with_calendar_app: bool) -> Self {
        let mut accounts: Vec<String> = config.accounts.keys().cloned().collect();
        accounts.sort();
        Self {
            accounts: accounts
                .into_iter()
                .map(|a| (a, Listing::Loading))
                .collect(),
            calendar_app: with_calendar_app.then_some(Listing::Loading),
            configured: config.calendars.clone(),
        }
    }

    pub fn set_account_listing(&mut self, account: &str, listing: Listing<DiscoveredCalendar>) {
        if let Some((_, l)) = self.accounts.iter_mut().find(|(a, _)| a == account) {
            *l = listing;
        }
    }

    pub fn set_calendar_app_listing(&mut self, listing: Listing<CalendarInfo>) {
        if self.calendar_app.is_some() {
            self.calendar_app = Some(listing);
        }
    }

    /// Follow a changed config: listings of remaining accounts stay, new
    /// accounts start loading.
    pub fn rebase(&mut self, config: &Config) {
        let mut accounts: Vec<String> = config.accounts.keys().cloned().collect();
        accounts.sort();
        let mut old = std::mem::take(&mut self.accounts);
        self.accounts = accounts
            .into_iter()
            .map(|a| match old.iter().position(|(o, _)| *o == a) {
                Some(i) => old.swap_remove(i),
                None => (a, Listing::Loading),
            })
            .collect();
        self.configured = config.calendars.clone();
    }

    /// List every account and Calendar.app again; returns the accounts.
    pub fn refresh(&mut self) -> Vec<String> {
        for (_, listing) in &mut self.accounts {
            *listing = Listing::Loading;
        }
        if self.calendar_app.is_some() {
            self.calendar_app = Some(Listing::Loading);
        }
        self.accounts.iter().map(|(a, _)| a.clone()).collect()
    }

    pub fn has_calendar_app(&self) -> bool {
        self.calendar_app.is_some()
    }

    /// Accounts whose calendars are not listed yet.
    pub fn accounts_to_discover(&self) -> Vec<String> {
        self.accounts
            .iter()
            .filter(|(_, l)| matches!(l, Listing::Loading))
            .map(|(a, _)| a.clone())
            .collect()
    }

    pub fn groups(&self, role: Role, current: Option<&str>) -> Vec<PickerGroup> {
        self.entries()
            .into_iter()
            .map(|(title, status, entries)| PickerGroup {
                title,
                status,
                items: entries
                    .into_iter()
                    .map(|e| {
                        let read_only = role == Role::Target && !e.writable;
                        PickerItem {
                            selected: e.key.is_some() && e.key.as_deref() == current,
                            label: e.label,
                            title: e.title,
                            section: e.section,
                            enabled: !read_only,
                            hint: if read_only { Some("Read-only") } else { e.hint },
                        }
                    })
                    .collect(),
            })
            .collect()
    }

    pub fn choose(&self, group: usize, item: usize) -> CalendarChoice {
        let entry = self
            .entries()
            .into_iter()
            .nth(group)
            .and_then(|(_, _, entries)| entries.into_iter().nth(item))
            .expect("a shown item");
        match entry.key {
            Some(key) => CalendarChoice {
                calendar: self.configured[&key].clone(),
                key,
                new_entry: false,
            },
            None => {
                let (base, calendar) = entry.proposal;
                let key = (1..)
                    .map(|n| match n {
                        1 => base.clone(),
                        n => format!("{base}_{n}"),
                    })
                    .find(|k| !self.configured.contains_key(k))
                    .unwrap_or_default();
                CalendarChoice {
                    key,
                    calendar,
                    new_entry: true,
                }
            }
        }
    }

    fn status<T>(listing: &Listing<T>) -> Option<String> {
        match listing {
            Listing::Loading => Some("Loading…".to_string()),
            Listing::Failed(e) => Some(e.clone()),
            Listing::Loaded(_) => None,
        }
    }

    /// Configured entries matching `filter`, sorted by key, as items.
    fn known(
        &self,
        filter: impl Fn(&CalendarConfig) -> bool,
        hint: Option<&'static str>,
    ) -> Vec<Entry> {
        let mut keys: Vec<&String> = self
            .configured
            .iter()
            .filter(|(_, c)| filter(c))
            .map(|(k, _)| k)
            .collect();
        keys.sort();
        keys.into_iter()
            .map(|k| Entry {
                label: k.clone(),
                title: k.clone(),
                section: None,
                writable: true,
                hint,
                key: Some(k.clone()),
                proposal: (k.clone(), self.configured[k].clone()),
            })
            .collect()
    }

    fn entries(&self) -> Vec<(String, Option<String>, Vec<Entry>)> {
        let mut groups = Vec::new();
        for (account, listing) in &self.accounts {
            let of_account = |c: &CalendarConfig| c.account.as_deref() == Some(account.as_str());
            let entries = match listing {
                Listing::Loaded(found) => {
                    let mut entries: Vec<Entry> = found
                        .iter()
                        .map(|cal| {
                            let display = cal.display_name.clone().unwrap_or_else(|| {
                                cal.href
                                    .trim_end_matches('/')
                                    .rsplit('/')
                                    .next()
                                    .unwrap_or_default()
                                    .to_string()
                            });
                            let key = self
                                .configured
                                .iter()
                                .find(|(_, c)| {
                                    of_account(c)
                                        && c.url.as_deref().is_some_and(|u| same_url(u, &cal.url))
                                })
                                .map(|(k, _)| k.clone());
                            Entry {
                                proposal: (
                                    calendar_key(&display),
                                    CalendarConfig {
                                        account: Some(account.clone()),
                                        url: Some(cal.url.clone()),
                                        eventkit_calendar: None,
                                        ics_url: None,
                                        concurrency: capycal::DEFAULT_CONCURRENCY,
                                    },
                                ),
                                title: display.clone(),
                                section: None,
                                label: display,
                                writable: cal.writable,
                                hint: None,
                                key,
                            }
                        })
                        .collect();
                    entries.sort_by_key(|e| e.label.to_lowercase());
                    let missing = self.known(
                        |c| {
                            of_account(c)
                                && !found
                                    .iter()
                                    .any(|f| c.url.as_deref().is_some_and(|u| same_url(u, &f.url)))
                        },
                        Some("Not found on the server"),
                    );
                    entries.extend(missing);
                    entries
                }
                _ => self.known(of_account, None),
            };
            groups.push((account.clone(), Self::status(listing), entries));
        }
        if let Some(listing) = &self.calendar_app {
            let entries = match listing {
                Listing::Loaded(found) => found
                    .iter()
                    .map(|cal| {
                        let account_title =
                            cal.source.as_ref().map(|s| format!("{s}/{}", cal.title));
                        let key = self
                            .configured
                            .iter()
                            .find(|(_, c)| {
                                c.eventkit_calendar.as_deref().is_some_and(|id| {
                                    id == cal.identifier || Some(id) == account_title.as_deref()
                                })
                            })
                            .map(|(k, _)| k.clone());
                        let source = cal.source.as_deref().unwrap_or("local");
                        Entry {
                            label: match &cal.source {
                                Some(s) => format!("{} ({s})", cal.title),
                                None => cal.title.clone(),
                            },
                            title: cal.title.clone(),
                            section: cal.source.clone(),
                            writable: cal.writable,
                            hint: None,
                            key,
                            proposal: (
                                calendar_key(&format!("{source} {}", cal.title)),
                                CalendarConfig {
                                    account: None,
                                    url: None,
                                    eventkit_calendar: Some(cal.identifier.clone()),
                                    ics_url: None,
                                    concurrency: capycal::DEFAULT_CONCURRENCY,
                                },
                            ),
                        }
                    })
                    .collect::<Vec<_>>(),
                _ => self.known(|c| c.eventkit_calendar.is_some(), None),
            };
            // By account, as Calendar.app shows them, then by name
            let mut entries = entries;
            entries.sort_by_key(|e| {
                (
                    e.section.as_deref().unwrap_or_default().to_lowercase(),
                    e.title.to_lowercase(),
                )
            });
            groups.push((CALENDAR_APP.to_string(), Self::status(listing), entries));
        }
        let mut feeds = self.known(|c| c.is_ics(), None);
        for feed in &mut feeds {
            feed.writable = false;
        }
        if !feeds.is_empty() {
            groups.push((FEEDS.to_string(), None, feeds));
        }
        groups
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use capycal::{CalendarInfo, Config, DiscoveredCalendar};

    fn config() -> Config {
        toml::from_str(
            "[accounts.work]\nurl = \"https://x/\"\nusername = \"u\"\npassword = \"p\"\n\
             [calendars.team]\naccount = \"work\"\nurl = \"https://x/team/\"\n",
        )
        .unwrap()
    }

    fn found(href: &str, name: &str, writable: bool) -> DiscoveredCalendar {
        DiscoveredCalendar {
            href: href.into(),
            url: format!("https://x{href}"),
            display_name: Some(name.into()),
            color: None,
            writable,
        }
    }

    #[test]
    fn test_known_calendars_show_while_loading_and_offline() {
        let mut picker = CalendarPicker::new(&config(), false);
        assert_eq!(picker.accounts_to_discover(), ["work"]);
        let groups = picker.groups(Role::Source, Some("team"));
        assert_eq!(groups[0].title, "work");
        assert_eq!(groups[0].status.as_deref(), Some("Loading…"));
        assert_eq!(groups[0].items[0].label, "team");
        assert!(groups[0].items[0].selected);
        picker.set_account_listing("work", Listing::Failed("offline".into()));
        assert_eq!(
            picker.groups(Role::Source, None)[0].status.as_deref(),
            Some("offline")
        );
        assert_eq!(picker.groups(Role::Source, None)[0].items.len(), 1);
    }

    #[test]
    fn test_choosing_reuses_or_proposes_entries_and_greys_out_read_only_targets() {
        let mut picker = CalendarPicker::new(&config(), false);
        picker.set_account_listing(
            "work",
            Listing::Loaded(vec![
                found("/team/", "Team", true),
                found("/shared/", "Team", false),
            ]),
        );
        let groups = picker.groups(Role::Target, None);
        let labels: Vec<&str> = groups[0].items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, ["Team", "Team"]);
        assert!(groups[0].items[0].enabled);
        assert!(!groups[0].items[1].enabled);
        assert_eq!(groups[0].items[1].hint, Some("Read-only"));
        assert!(picker.groups(Role::Source, None)[0].items[1].enabled);

        let reused = picker.choose(0, 0);
        assert_eq!((reused.key.as_str(), reused.new_entry), ("team", false));
        let new = picker.choose(0, 1);
        assert_eq!((new.key.as_str(), new.new_entry), ("team_2", true));
        assert_eq!(new.calendar.account.as_deref(), Some("work"));
        assert_eq!(new.calendar.url.as_deref(), Some("https://x/shared/"));
    }

    #[test]
    fn test_calendar_app_group() {
        let mut picker = CalendarPicker::new(&config(), true);
        picker.set_calendar_app_listing(Listing::Loaded(vec![CalendarInfo {
            title: "Family".into(),
            source: Some("iCloud".into()),
            identifier: "ID-1".into(),
            writable: true,
        }]));
        let groups = picker.groups(Role::Target, None);
        assert_eq!(groups[1].title, "Calendar.app");
        assert_eq!(groups[1].items[0].label, "Family (iCloud)");
        let choice = picker.choose(1, 0);
        assert_eq!(choice.key, "icloud_family");
        assert_eq!(choice.calendar.eventkit_calendar.as_deref(), Some("ID-1"));
    }

    #[test]
    fn test_calendar_app_sorted_by_account_then_name() {
        let mut picker = CalendarPicker::new(&config(), true);
        let cal = |title: &str, source: &str| CalendarInfo {
            title: title.into(),
            source: Some(source.into()),
            identifier: format!("{source}-{title}"),
            writable: true,
        };
        picker.set_calendar_app_listing(Listing::Loaded(vec![
            cal("Work", "iCloud"),
            cal("Calendar", "Exchange"),
            cal("family", "iCloud"),
        ]));
        let items = &picker.groups(Role::Source, None)[1].items;
        let shown: Vec<(&str, Option<&str>)> = items
            .iter()
            .map(|i| (i.title.as_str(), i.section.as_deref()))
            .collect();
        assert_eq!(
            shown,
            [
                ("Calendar", Some("Exchange")),
                ("family", Some("iCloud")),
                ("Work", Some("iCloud"))
            ]
        );
        // Choosing follows the shown order
        assert_eq!(
            picker.choose(1, 2).calendar.eventkit_calendar.as_deref(),
            Some("iCloud-Work")
        );
    }

    #[test]
    fn test_feeds_are_sources_only() {
        let mut cfg = config();
        cfg.calendars.insert(
            "google".into(),
            toml::from_str("ics_url = \"https://calendar.google.com/basic.ics\"").unwrap(),
        );
        let picker = CalendarPicker::new(&cfg, false);
        let groups = picker.groups(Role::Source, Some("google"));
        assert_eq!(groups[1].title, FEEDS);
        assert_eq!(groups[1].items[0].label, "google");
        assert!(groups[1].items[0].enabled && groups[1].items[0].selected);
        let target = &picker.groups(Role::Target, None)[1].items[0];
        assert!(!target.enabled);
        assert_eq!(target.hint, Some("Read-only"));
        assert_eq!(picker.choose(1, 0).key, "google");
        // Not one of the account's calendars
        assert_eq!(groups[0].items.len(), 1);
    }

    #[test]
    fn test_feed_key_and_address() {
        assert_eq!(
            feed_key("https://calendar.google.com/calendar/ical/x/basic.ics"),
            "google"
        );
        assert_eq!(
            feed_key("webcal://p01-calendars.icloud.com/published/2/x"),
            "icloud"
        );
        assert_eq!(feed_key("https://127.0.0.1/x.ics"), "feed");
        assert_eq!(feed_key("not an address"), "feed");
        assert!(feed_url_valid(" webcal://example.com/a.ics "));
        assert!(feed_url_valid("https://example.com/a.ics"));
        assert!(!feed_url_valid("example.com/a.ics"));
        assert!(!feed_url_valid("ftp://example.com/a.ics"));
        assert!(!feed_url_valid(""));
    }

    #[test]
    fn test_rebase_keeps_listings_of_remaining_accounts() {
        let mut picker = CalendarPicker::new(&config(), false);
        picker.set_account_listing("work", Listing::Loaded(vec![found("/team/", "Team", true)]));
        let mut more = config();
        more.accounts
            .insert("home".into(), more.accounts["work"].clone());
        picker.rebase(&more);
        assert_eq!(picker.accounts_to_discover(), ["home"]);
        assert_eq!(picker.groups(Role::Source, None)[1].items[0].label, "Team");
    }
}
