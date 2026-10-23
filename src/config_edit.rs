//! Edits `capycal.toml` in place for the desktop apps: one table at a time,
//! keeping comments, order and every table it does not touch.

use std::path::{Path, PathBuf};

use toml_edit::{DocumentMut, InlineTable, Item, Table, Value};

use crate::config::{
    AccountConfig, CalendarConfig, ConditionConfig, Config, Credential, SyncConfig,
    TransformConfig, default_eventkit_days_future, default_eventkit_days_past,
};
use crate::error::{Error, Result};

/// The top-level tables the editor writes, in the order new ones are placed.
const SECTIONS: [&str; 3] = ["accounts", "calendars", "syncs"];

pub struct ConfigEditor {
    path: PathBuf,
    doc: DocumentMut,
}

impl ConfigEditor {
    /// Read `path`; a missing file is an empty config.
    pub fn load(path: &Path) -> Result<Self> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(Error::Config(format!("{}: {e}", path.display()))),
        };
        let mut doc = text
            .parse::<DocumentMut>()
            .map_err(|e| Error::Config(format!("failed to parse {}: {e}", path.display())))?;
        // Normally done by Config::load already; never write an old version
        crate::config_migrate::migrate(&mut doc)
            .map_err(|e| Error::Config(format!("{}: {e}", path.display())))?;
        Ok(Self {
            path: path.to_path_buf(),
            doc,
        })
    }

    /// An editor of an empty config at `path`, for a file that cannot be
    /// read; saving it would replace the file.
    pub fn empty(path: &Path) -> Self {
        let mut doc = DocumentMut::new();
        let _ = crate::config_migrate::migrate(&mut doc);
        Self {
            path: path.to_path_buf(),
            doc,
        }
    }

    /// The edited config, parsed and validated like `Config::load` does.
    pub fn config(&self) -> Result<Config> {
        let mut config: Config = toml::from_str(&self.doc.to_string())
            .map_err(|e| Error::Config(format!("failed to parse {}: {e}", self.path.display())))?;
        config.config_dir = self.path.parent().map(Path::to_path_buf);
        config.validate()?;
        Ok(config)
    }

    /// Validate, then replace the file atomically, through a symlink and
    /// with the old file's permissions (a new file is private, it may hold
    /// passwords). Returns the saved config.
    pub fn save(&self) -> Result<Config> {
        let config = self.config()?;
        write_atomic(&self.path, &self.doc.to_string()).map_err(|e| {
            let target = std::fs::canonicalize(&self.path).unwrap_or_else(|_| self.path.clone());
            Error::Config(format!("{}: {e}", target.display()))
        })?;
        Ok(config)
    }

    /// The TOML text of `[<section>.<name>]` (or its inline table), to tell
    /// whether an entry changed on disk.
    pub fn entry_text(&self, section: &str, name: &str) -> Option<String> {
        match self.doc.get(section)?.get(name)? {
            Item::Table(table) => {
                let mut doc = DocumentMut::new();
                for (key, item) in table.iter() {
                    doc.insert(key, item.clone());
                }
                Some(doc.to_string().trim_start().to_string())
            }
            item => Some(item.to_string()),
        }
    }

    pub fn set_account(&mut self, name: &str, account: &AccountConfig) {
        let table = self.table("accounts", name);
        table.clear();
        table.insert("url", Item::Value(account.url.as_str().into()));
        table.insert("username", Item::Value(account.username.as_str().into()));
        table.insert("password", Item::Value(credential(&account.password)));
    }

    /// Rename an account, the `account` of its calendars and a keychain
    /// credential named after it. The keychain entry itself is the caller's.
    pub fn rename_account(&mut self, old: &str, new: &str) -> Result<()> {
        if old == new {
            return Ok(());
        }
        self.rename("accounts", "account", old, new)?;
        if let Some(password) = self.doc["accounts"][new].get_mut("password")
            && password.get("keychain").and_then(Item::as_str) == Some(old)
        {
            *password = Item::Value(credential(&Credential::Keychain {
                keychain: new.to_string(),
            }));
        }
        if self.doc.contains_key("calendars") {
            for (_, calendar) in self.section("calendars").iter_mut() {
                if calendar.get("account").and_then(Item::as_str) == Some(old) {
                    calendar["account"] = Item::Value(new.into());
                }
            }
        }
        Ok(())
    }

    pub fn remove_account(&mut self, name: &str) {
        self.remove("accounts", name);
    }

    /// Write `[syncs.<sync.name>]`, leaving out values equal to their default.
    pub fn set_sync(&mut self, sync: &SyncConfig) {
        let table = self.table("syncs", &sync.name);
        table.clear();
        table.insert("source", Item::Value(sync.source.as_str().into()));
        table.insert("target", Item::Value(sync.target.as_str().into()));
        table.insert("interval", Item::Value(duration(sync.interval)));
        if !sync.enabled {
            table.insert("enabled", Item::Value(false.into()));
        }
        if !sync.fields.is_empty() {
            table.insert("fields", Item::Value(strings(&sync.fields)));
        }
        if sync.eventkit_days_past != default_eventkit_days_past() {
            table.insert(
                "eventkit_days_past",
                Item::Value((sync.eventkit_days_past as i64).into()),
            );
        }
        if sync.eventkit_days_future != default_eventkit_days_future() {
            table.insert(
                "eventkit_days_future",
                Item::Value((sync.eventkit_days_future as i64).into()),
            );
        }
        if sync.eventkit_all_events {
            table.insert("eventkit_all_events", Item::Value(true.into()));
        }
        if !sync.transforms.is_empty() {
            let mut list = toml_edit::Array::new();
            for t in &sync.transforms {
                let mut value = Value::InlineTable(transform(t));
                value.decor_mut().set_prefix("\n  ");
                list.push_formatted(value);
            }
            list.set_trailing_comma(true);
            list.set_trailing("\n");
            table.insert("transforms", Item::Value(Value::Array(list)));
        }
    }

    /// Write `enabled` of sync `name` and nothing else: `enabled = false`,
    /// or no key for the default. Comments and the rest of the table stay.
    pub fn set_enabled(&mut self, name: &str, on: bool) -> Result<()> {
        let syncs = self.section("syncs");
        let item = syncs
            .get_mut(name)
            .ok_or_else(|| Error::Config(format!("sync '{name}' not found")))?;
        let table = table_mut(item);
        if on {
            table.remove("enabled");
        } else {
            table.insert("enabled", Item::Value(false.into()));
        }
        Ok(())
    }

    /// Write `[daemon] log_level`: no key for the default "info" (and no
    /// table left empty); a new `[daemon]` table goes before the others.
    pub fn set_log_level(&mut self, level: &str) {
        let exists = self.doc.get("daemon").is_some_and(Item::is_table);
        if level == "info" {
            if let Some(daemon) = self.doc.get_mut("daemon").and_then(Item::as_table_mut) {
                daemon.remove("log_level");
                if daemon.is_empty() {
                    self.doc.remove("daemon");
                    // Take back the blank line set before the next table
                    let first = max_position(self.doc.as_table()).and_then(|max| {
                        (0..=max).find(|p| table_at(self.doc.as_table_mut(), *p).is_some())
                    });
                    // Unless the version stands above it
                    let values = self.doc.iter().any(|(_, item)| item.is_value());
                    if let Some(next) = first.and_then(|p| table_at(self.doc.as_table_mut(), p))
                        && !values
                        && next.decor().prefix().and_then(|p| p.as_str()) == Some("\n")
                    {
                        next.decor_mut().set_prefix("");
                    }
                }
            }
            return;
        }
        if !exists {
            let first = self
                .doc
                .iter()
                .filter_map(|(_, item)| item.as_table())
                .flat_map(|t| {
                    t.iter()
                        .filter_map(|(_, i)| i.as_table())
                        .filter_map(Table::position)
                        .chain(t.position())
                        .collect::<Vec<_>>()
                })
                .min();
            let at =
                first.unwrap_or_else(|| max_position(self.doc.as_table()).map_or(0, |p| p + 1));
            shift_positions(self.doc.as_table_mut(), at);
            // A blank line between [daemon] and the table after it
            if let Some(next) = table_at(self.doc.as_table_mut(), at + 1)
                && next
                    .decor()
                    .prefix()
                    .and_then(|p| p.as_str())
                    .is_none_or(str::is_empty)
            {
                next.decor_mut().set_prefix("\n");
            }
            let mut daemon = Table::new();
            daemon.set_position(Some(at));
            self.doc.insert("daemon", Item::Table(daemon));
        }
        let daemon = table_mut(self.doc.entry("daemon").or_insert(Item::None));
        daemon.insert("log_level", toml_edit::value(level));
    }

    pub fn rename_sync(&mut self, old: &str, new: &str) -> Result<()> {
        if old == new {
            return Ok(());
        }
        self.rename("syncs", "sync", old, new)
    }

    pub fn remove_sync(&mut self, name: &str) {
        self.remove("syncs", name);
    }

    pub fn set_calendar(&mut self, name: &str, calendar: &CalendarConfig) {
        let table = self.table("calendars", name);
        table.clear();
        if let Some(account) = &calendar.account {
            table.insert("account", Item::Value(account.as_str().into()));
        }
        if let Some(url) = &calendar.url {
            table.insert("url", Item::Value(url.as_str().into()));
        }
        if let Some(id) = &calendar.eventkit_calendar {
            table.insert("eventkit_calendar", Item::Value(id.as_str().into()));
        }
        if let Some(feed) = &calendar.ics_url {
            table.insert("ics_url", Item::Value(credential(feed)));
        }
        if calendar.concurrency != crate::config::DEFAULT_CONCURRENCY {
            table.insert(
                "concurrency",
                Item::Value((calendar.concurrency as i64).into()),
            );
        }
    }

    pub fn remove_calendar(&mut self, name: &str) {
        self.remove("calendars", name);
    }

    /// The table `[<section>.<name>]`, created after the last table of its
    /// section (or before the first of a later one) if it does not exist.
    fn table(&mut self, section: &str, name: &str) -> &mut Table {
        if !matches!(self.section(section).get(name), Some(Item::Table(_))) {
            let position = self.new_position(section);
            let item = self.section(section).entry(name).or_insert(Item::None);
            table_mut(item).set_position(Some(position));
        }
        table_mut(&mut self.section(section)[name])
    }

    /// The top-level table of a section, created (implicit) if missing.
    fn section(&mut self, section: &str) -> &mut Table {
        let item = self.doc.entry(section).or_insert(Item::None);
        let was_table = item.is_table();
        let table = table_mut(item);
        if !was_table {
            table.set_implicit(true);
        }
        table
    }

    /// The document position for a new table of `section`, shifting every
    /// table at or after it: after the last table of the section, else
    /// before the first of a later section, else after all tables.
    fn new_position(&mut self, section: &str) -> isize {
        let index = SECTIONS
            .iter()
            .position(|s| *s == section)
            .expect("a known section");
        let positions = |doc: &DocumentMut, s: &str| -> Vec<isize> {
            doc.get(s)
                .and_then(Item::as_table)
                .map(|t| {
                    t.iter()
                        .filter_map(|(_, i)| i.as_table())
                        .flat_map(|t| [t.position(), max_position(t)])
                        .flatten()
                        .collect()
                })
                .unwrap_or_default()
        };
        let later = || {
            SECTIONS[index + 1..]
                .iter()
                .filter_map(|s| positions(&self.doc, s).into_iter().min())
                .min()
        };
        let at = match positions(&self.doc, section).into_iter().max() {
            Some(last) => last + 1,
            None => {
                later().unwrap_or_else(|| max_position(self.doc.as_table()).map_or(0, |p| p + 1))
            }
        };
        shift_positions(self.doc.as_table_mut(), at);
        at
    }

    fn rename(&mut self, section: &str, what: &str, old: &str, new: &str) -> Result<()> {
        let table = self.section(section);
        if table.contains_key(new) {
            return Err(Error::Config(format!("{what} '{new}' already exists")));
        }
        let item = table
            .remove(old)
            .ok_or_else(|| Error::Config(format!("{what} '{old}' not found")))?;
        table.insert(new, item);
        Ok(())
    }

    fn remove(&mut self, section: &str, name: &str) {
        if self.doc.contains_key(section) {
            self.section(section).remove(name);
        }
    }
}

impl std::fmt::Display for ConfigEditor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.doc)
    }
}

fn duration(d: std::time::Duration) -> Value {
    duration_text(d).into()
}

/// A duration as humantime text; whole days as days, since humantime would
/// write 60 days in months, hours and seconds.
pub fn duration_text(d: std::time::Duration) -> String {
    const DAY: u64 = 86400;
    match d.as_secs() {
        secs if secs > 0 && secs % DAY == 0 && d.subsec_nanos() == 0 => format!("{}d", secs / DAY),
        _ => humantime::format_duration(d).to_string(),
    }
}

fn strings(values: &[String]) -> Value {
    Value::Array(values.iter().map(String::as_str).collect())
}

/// One transform as an inline table, `field` and `action` first.
fn transform(t: &TransformConfig) -> InlineTable {
    let mut table = InlineTable::new();
    if !t.field.is_empty() {
        table.insert("field", t.field.as_str().into());
    }
    table.insert("action", t.action.as_str().into());
    let text = [
        ("pattern", &t.pattern),
        ("replacement", &t.replacement),
        ("search", &t.search),
    ];
    for (key, value) in text {
        if let Some(v) = value {
            table.insert(key, v.as_str().into());
        }
    }
    if let Some(count) = t.count {
        table.insert("count", (count as i64).into());
    }
    if let Some(value) = &t.value {
        table.insert("value", value.as_str().into());
    }
    for (key, value) in [
        ("pre_event", t.pre_event),
        ("post_event", t.post_event),
        ("trigger", t.trigger),
    ] {
        if let Some(d) = value {
            table.insert(key, duration(d));
        }
    }
    if !t.when.is_empty() {
        let when: toml_edit::Array = t.when.iter().map(condition).collect();
        table.insert("when", Value::Array(when));
    }
    table
}

/// One condition as an inline table, `field` first.
fn condition(c: &ConditionConfig) -> InlineTable {
    let mut table = InlineTable::new();
    if !c.any.is_empty() {
        let any: toml_edit::Array = c.any.iter().map(condition).collect();
        table.insert("any", Value::Array(any));
    }
    if !c.field.is_empty() {
        table.insert("field", c.field.as_str().into());
    }
    for (key, value) in [("contains", &c.contains), ("matches", &c.matches)] {
        if let Some(v) = value {
            table.insert(key, v.as_str().into());
        }
    }
    if let Some(empty) = c.empty {
        table.insert("empty", empty.into());
    }
    if let Some(days) = &c.days {
        table.insert("days", strings(days));
    }
    for (key, value) in [("start_time", &c.start_time), ("end_time", &c.end_time)] {
        if let Some(v) = value {
            table.insert(key, v.as_str().into());
        }
    }
    if let Some(all_day) = c.all_day {
        table.insert("all_day", all_day.into());
    }
    if c.not {
        table.insert("not", true.into());
    }
    table
}

/// Write `text` to a new file `path` that is never readable by more people
/// than `like` (or only by the owner if `like` does not exist).
/// Replace the file at `path` atomically, through a symlink and with the old
/// file's permissions (a new file is private, it may hold passwords).
pub(crate) fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let name = target.file_name().unwrap_or_default().to_string_lossy();
    let tmp = target.with_file_name(format!(".{name}.tmp"));
    let written = write_private(&tmp, text, &target).and_then(|()| std::fs::rename(&tmp, &target));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

/// Write a new file at `path` with the permissions of `like` (private when
/// `like` does not exist).
pub(crate) fn write_private(path: &Path, text: &str, like: &Path) -> std::io::Result<()> {
    use std::io::Write;
    let _ = std::fs::remove_file(path);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mode = std::fs::metadata(like).map_or(0o600, |m| m.permissions().mode() & 0o777);
        options.mode(mode);
    }
    let mut file = options.open(path)?;
    file.write_all(text.as_bytes())?;
    #[cfg(unix)]
    if let Ok(meta) = std::fs::metadata(like) {
        // The umask may have removed bits the old file had
        file.set_permissions(meta.permissions())?;
    }
    Ok(())
}

/// `item` as a table: an inline table becomes a regular one, so it is edited
/// like the tables capycal writes; anything else becomes an empty table.
fn table_mut(item: &mut Item) -> &mut Table {
    if let Item::Value(Value::InlineTable(inline)) = item {
        *item = Item::Table(std::mem::take(inline).into_table());
    }
    if !item.is_table() {
        *item = Item::Table(Table::new());
    }
    item.as_table_mut().expect("made a table above")
}

/// The table at `position` in `table`, nested ones included.
pub(crate) fn table_at(table: &mut Table, position: isize) -> Option<&mut Table> {
    for (_, item) in table.iter_mut() {
        if let Item::Table(t) = item {
            if t.position() == Some(position) {
                return Some(t);
            }
            if let Some(found) = table_at(t, position) {
                return Some(found);
            }
        }
    }
    None
}

/// The highest position of any table in `table`, nested ones included.
pub(crate) fn max_position(table: &Table) -> Option<isize> {
    table
        .iter()
        .flat_map(|(_, item)| match item {
            Item::Table(t) => vec![t],
            Item::ArrayOfTables(tables) => tables.iter().collect(),
            _ => vec![],
        })
        .flat_map(|t| [t.position(), max_position(t)])
        .flatten()
        .max()
}

/// Move every table at `from` or later one position down.
fn shift_positions(table: &mut Table, from: isize) {
    for (_, item) in table.iter_mut() {
        match item {
            Item::Table(t) => {
                if let Some(p) = t.position().filter(|p| *p >= from) {
                    t.set_position(Some(p + 1));
                }
                shift_positions(t, from);
            }
            Item::ArrayOfTables(tables) => {
                for t in tables.iter_mut() {
                    if let Some(p) = t.position().filter(|p| *p >= from) {
                        t.set_position(Some(p + 1));
                    }
                    shift_positions(t, from);
                }
            }
            _ => {}
        }
    }
}

fn credential(credential: &Credential) -> Value {
    let table = |key: &str, value: &str| {
        let mut t = InlineTable::new();
        t.insert(key, value.into());
        Value::InlineTable(t)
    };
    match credential {
        Credential::Plain(p) => p.as_str().into(),
        Credential::Env { env } => table("env", env),
        Credential::File { file } => table("file", &file.to_string_lossy()),
        Credential::Command { command } => table("command", command),
        Credential::Keychain { keychain } => table("keychain", keychain),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AccountConfig, CalendarConfig, Credential};

    fn test_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("capycal-{}-edit-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("capycal.toml")
    }

    fn account(url: &str, password: Credential) -> AccountConfig {
        AccountConfig {
            url: url.to_string(),
            username: "alice".to_string(),
            password,
        }
    }

    fn caldav(account: &str, url: &str) -> CalendarConfig {
        CalendarConfig {
            account: Some(account.to_string()),
            url: Some(url.to_string()),
            eventkit_calendar: None,
            ics_url: None,
            concurrency: crate::config::DEFAULT_CONCURRENCY,
        }
    }

    const COMMENTED: &str = r#"# my capycal config

[daemon]
log_level = "debug" # keep this

# the work server
[accounts.work]
url = "https://work.example.com/"
username = "alice"
password = { env = "WORK_PW" }

[calendars.work]
account = "work"
url = "https://work.example.com/cal/"

# blockers go here
[calendars.home]
account = "work"
url = "https://work.example.com/home/"
"#;

    #[test]
    fn test_missing_file_is_an_empty_config() {
        let editor = ConfigEditor::load(&test_path("missing")).unwrap();
        assert_eq!(editor.to_string(), "version = 2\n");
        assert!(editor.config().unwrap().syncs.is_empty());
    }

    #[test]
    fn test_new_account_goes_after_the_accounts_and_comments_stay() {
        let path = test_path("new-account");
        std::fs::write(&path, COMMENTED).unwrap();
        let mut editor = ConfigEditor::load(&path).unwrap();
        editor.set_account(
            "nextcloud",
            &account(
                "https://cloud.example.org/",
                Credential::Keychain {
                    keychain: "nextcloud".to_string(),
                },
            ),
        );
        let text = editor.to_string();
        assert!(
            text.starts_with(
                "# my capycal config\n\nversion = 2\n\n[daemon]\nlog_level = \"debug\" # keep this\n"
            ),
            "{text}"
        );
        let nextcloud = text.find("[accounts.nextcloud]").unwrap();
        assert!(text.find("[accounts.work]").unwrap() < nextcloud, "{text}");
        assert!(nextcloud < text.find("[calendars.work]").unwrap(), "{text}");
        assert!(text.contains(
            "[accounts.nextcloud]\nurl = \"https://cloud.example.org/\"\nusername = \"alice\"\npassword = { keychain = \"nextcloud\" }\n"
        ), "{text}");
        assert!(
            text.contains("# blockers go here\n[calendars.home]"),
            "{text}"
        );
        editor.config().unwrap();
    }

    #[test]
    fn test_changing_an_account_keeps_its_header_comment() {
        let path = test_path("change-account");
        std::fs::write(&path, COMMENTED).unwrap();
        let mut editor = ConfigEditor::load(&path).unwrap();
        editor.set_account(
            "work",
            &account(
                "https://new.example.com/",
                Credential::Plain("pw".to_string()),
            ),
        );
        let text = editor.to_string();
        assert!(text.contains(
            "# the work server\n[accounts.work]\nurl = \"https://new.example.com/\"\nusername = \"alice\"\npassword = \"pw\"\n"
        ), "{text}");
    }

    #[test]
    fn test_rename_account_moves_references_and_keychain_name() {
        let path = test_path("rename-account");
        std::fs::write(&path, COMMENTED).unwrap();
        let mut editor = ConfigEditor::load(&path).unwrap();
        editor.set_account(
            "work",
            &account(
                "https://work.example.com/",
                Credential::Keychain {
                    keychain: "work".to_string(),
                },
            ),
        );
        editor.rename_account("work", "office").unwrap();
        let config = editor.config().unwrap();
        assert!(config.accounts.contains_key("office"));
        assert!(!config.accounts.contains_key("work"));
        assert_eq!(config.calendars["home"].account.as_deref(), Some("office"));
        assert!(
            matches!(&config.accounts["office"].password, Credential::Keychain { keychain } if keychain == "office")
        );
        let text = editor.to_string();
        assert!(
            text.find("[accounts.office]").unwrap() < text.find("[calendars.work]").unwrap(),
            "{text}"
        );
        assert!(editor.rename_account("office", "office").is_ok());
        editor.set_account(
            "other",
            &account("https://o/", Credential::Plain("p".into())),
        );
        let err = editor
            .rename_account("office", "other")
            .unwrap_err()
            .to_string();
        assert!(err.contains("account 'other' already exists"), "{err}");
    }

    #[test]
    fn test_calendars_first_in_an_empty_file_and_defaults_left_out() {
        let path = test_path("calendars");
        let mut editor = ConfigEditor::load(&path).unwrap();
        editor.set_calendar("work", &caldav("acc", "https://x/work/"));
        editor.set_account("acc", &account("https://x/", Credential::Plain("p".into())));
        editor.set_calendar(
            "big",
            &CalendarConfig {
                concurrency: 4,
                ..caldav("acc", "https://x/big/")
            },
        );
        editor.set_calendar(
            "mac",
            &CalendarConfig {
                account: None,
                url: None,
                eventkit_calendar: Some("iCloud/Family".to_string()),
                ics_url: None,
                concurrency: crate::config::DEFAULT_CONCURRENCY,
            },
        );
        assert_eq!(
            editor.to_string(),
            "version = 2\n\n[accounts.acc]\nurl = \"https://x/\"\nusername = \"alice\"\npassword = \"p\"\n\n\
             [calendars.work]\naccount = \"acc\"\nurl = \"https://x/work/\"\n\n\
             [calendars.big]\naccount = \"acc\"\nurl = \"https://x/big/\"\nconcurrency = 4\n\n\
             [calendars.mac]\neventkit_calendar = \"iCloud/Family\"\n"
        );
        editor.remove_calendar("mac");
        editor.remove_calendar("big");
        assert!(!editor.to_string().contains("[calendars.big]"));
    }

    #[test]
    fn test_save_validates_and_writes_atomically() {
        let path = test_path("save");
        std::fs::write(&path, COMMENTED).unwrap();
        let mut editor = ConfigEditor::load(&path).unwrap();
        editor.remove_account("work");
        let err = editor.save().unwrap_err().to_string();
        assert!(err.contains("unknown account 'work'"), "{err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), COMMENTED);

        editor.set_account(
            "work",
            &account("https://w/", Credential::Plain("p".into())),
        );
        let config = editor.save().unwrap();
        assert_eq!(config.accounts["work"].url, "https://w/");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), editor.to_string());
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap()).unwrap().collect();
        assert_eq!(leftovers.len(), 1, "temp file left behind");
    }

    #[cfg(unix)]
    #[test]
    fn test_new_config_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let path = test_path("private");
        let mut editor = ConfigEditor::load(&path).unwrap();
        editor.set_account(
            "work",
            &account("https://w/", Credential::Plain("p".into())),
        );
        editor.save().unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_save_keeps_permissions_and_writes_through_a_symlink() {
        use std::os::unix::fs::PermissionsExt;
        let real = test_path("symlink");
        std::fs::write(&real, COMMENTED).unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o600)).unwrap();
        let link = real.with_file_name("link.toml");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let mut editor = ConfigEditor::load(&link).unwrap();
        editor.set_account(
            "work",
            &account("https://w/", Credential::Plain("p".into())),
        );
        editor.save().unwrap();
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(
            std::fs::read_to_string(&real)
                .unwrap()
                .contains("https://w/")
        );
        assert_eq!(
            std::fs::metadata(&real).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    use crate::config::{ConditionConfig, SyncConfig, TransformAction, TransformConfig};
    use std::time::Duration;

    const TWO_CALENDARS: &str = "[accounts.acc]\nurl = \"https://x/\"\nusername = \"u\"\npassword = \"p\"\n\n\
        [calendars.a]\naccount = \"acc\"\nurl = \"https://x/a/\"\n\n\
        [calendars.b]\naccount = \"acc\"\nurl = \"https://x/b/\"\n";

    fn sync(name: &str) -> SyncConfig {
        let mut sync: SyncConfig =
            toml::from_str("source = \"a\"\ntarget = \"b\"\ninterval = \"5m\"\n").unwrap();
        sync.name = name.to_string();
        sync
    }

    fn transform(action: TransformAction) -> TransformConfig {
        toml::from_str::<TransformConfig>(&format!("action = \"{}\"", action.as_str())).unwrap()
    }

    fn editor_with(name: &str, text: &str) -> ConfigEditor {
        let path = test_path(name);
        std::fs::write(&path, text).unwrap();
        ConfigEditor::load(&path).unwrap()
    }

    #[test]
    fn test_minimal_sync_writes_only_what_differs_from_the_defaults() {
        let mut editor = editor_with("sync-min", TWO_CALENDARS);
        editor.set_sync(&sync("a-to-b"));
        assert!(
            editor
                .to_string()
                .ends_with("\n[syncs.a-to-b]\nsource = \"a\"\ntarget = \"b\"\ninterval = \"5m\"\n"),
            "{editor}"
        );
        editor.config().unwrap();
    }

    #[test]
    fn test_full_sync_round_trips() {
        let mut editor = editor_with("sync-full", TWO_CALENDARS);
        let mut s = sync("b to ä");
        s.interval = Duration::from_secs(90 * 60);
        s.enabled = false;
        s.fields = vec!["SUMMARY".to_string(), "VALARM".to_string()];
        s.eventkit_days_past = 7;
        s.transforms = vec![
            TransformConfig {
                field: "SUMMARY".to_string(),
                value: Some("Busy ({{source_name}})".to_string()),
                ..transform(TransformAction::SetField)
            },
            TransformConfig {
                pre_event: Some(Duration::from_secs(1800)),
                post_event: Some(Duration::from_secs(900)),
                ..transform(TransformAction::AddBuffer)
            },
            TransformConfig {
                when: vec![
                    ConditionConfig {
                        days: Some(vec!["sat".to_string(), "sun".to_string()]),
                        start_time: Some("00:00".to_string()),
                        end_time: Some("23:59".to_string()),
                        not: true,
                        ..ConditionConfig::default()
                    },
                    ConditionConfig {
                        field: "LOCATION".to_string(),
                        empty: Some(false),
                        ..ConditionConfig::default()
                    },
                    ConditionConfig {
                        any: vec![
                            ConditionConfig {
                                all_day: Some(true),
                                ..ConditionConfig::default()
                            },
                            ConditionConfig {
                                field: "DESCRIPTION".to_string(),
                                contains: Some("Lunch".to_string()),
                                ..ConditionConfig::default()
                            },
                        ],
                        ..ConditionConfig::default()
                    },
                    ConditionConfig {
                        field: "DESCRIPTION".to_string(),
                        matches: Some("^1:1".to_string()),
                        ..ConditionConfig::default()
                    },
                ],
                ..transform(TransformAction::Skip)
            },
        ];
        editor.set_sync(&s);
        let text = editor.to_string();
        assert!(text.contains(
            "[syncs.\"b to ä\"]\nsource = \"a\"\ntarget = \"b\"\ninterval = \"1h 30m\"\nenabled = false\n\
             fields = [\"SUMMARY\", \"VALARM\"]\neventkit_days_past = 7\n\
             transforms = [\n\
             \x20 { field = \"SUMMARY\", action = \"set_field\", value = \"Busy ({{source_name}})\" },\n\
             \x20 { action = \"add_buffer\", pre_event = \"30m\", post_event = \"15m\" },\n\
             \x20 { action = \"skip\", when = [{ days = [\"sat\", \"sun\"], start_time = \"00:00\", end_time = \"23:59\", not = true }, \
             { field = \"LOCATION\", empty = false }, { any = [{ all_day = true }, { field = \"DESCRIPTION\", contains = \"Lunch\" }] }, \
             { field = \"DESCRIPTION\", matches = \"^1:1\" }] },\n\
             ]\n"
        ), "{text}");
        let config = editor.config().unwrap();
        let back = &config.syncs[0];
        assert_eq!(back.name, "b to ä");
        assert_eq!(back.interval, s.interval);
        assert!(!back.enabled);
        assert_eq!(back.transforms.len(), 3);
        assert_eq!(
            back.transforms[1].pre_event,
            Some(Duration::from_secs(1800))
        );
        assert_eq!(back.transforms[2].when, s.transforms[2].when);
    }

    #[test]
    fn test_replacing_array_of_tables_transforms() {
        let mut editor = editor_with(
            "sync-aot",
            &format!(
                "{TWO_CALENDARS}\n# keep me\n[syncs.s]\nsource = \"a\"\ntarget = \"b\"\ninterval = \"5m\"\n\n\
                 [[syncs.s.transforms]]\naction = \"skip\"\n[[syncs.s.transforms.when]]\nall_day = true\n\n[[syncs.s.transforms]]\naction = \"add_alarm\"\ntrigger = \"5m\"\n"
            ),
        );
        let mut s = sync("s");
        s.transforms = vec![TransformConfig {
            field: "LOCATION".to_string(),
            ..transform(TransformAction::RemoveField)
        }];
        editor.set_sync(&s);
        let text = editor.to_string();
        assert!(!text.contains("[[syncs.s.transforms]]"), "{text}");
        assert!(text.contains("# keep me\n[syncs.s]\n"), "{text}");
        assert_eq!(editor.config().unwrap().syncs[0].transforms.len(), 1);
    }

    #[test]
    fn test_rename_and_remove_sync_keep_the_order() {
        let mut editor = editor_with("sync-rename", TWO_CALENDARS);
        for name in ["one", "two", "three"] {
            editor.set_sync(&sync(name));
        }
        editor.rename_sync("two", "zwei").unwrap();
        let names: Vec<String> = editor
            .config()
            .unwrap()
            .syncs
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names, ["one", "zwei", "three"]);
        assert!(
            editor
                .rename_sync("one", "three")
                .unwrap_err()
                .to_string()
                .contains("sync 'three' already exists")
        );
        editor.remove_sync("one");
        let names: Vec<String> = editor
            .config()
            .unwrap()
            .syncs
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names, ["zwei", "three"]);
    }

    #[test]
    fn test_inline_and_dotted_forms_are_edited_as_tables() {
        let acc = || account("https://x/", Credential::Plain("p".into()));
        let mut editor = editor_with(
            "inline-section",
            "accounts = { acc = { url = \"https://x/\", username = \"u\", password = \"p\" } }\n\n\
             [calendars]\na = { account = \"acc\", url = \"https://x/a/\" }\n\
             b = { account = \"acc\", url = \"https://x/b/\" }\n",
        );
        editor.set_account("acc", &acc());
        editor.set_account("other", &acc());
        editor.set_calendar("a", &caldav("other", "https://x/a2/"));
        editor.remove_calendar("b");
        let config = editor.config().unwrap();
        assert_eq!(config.calendars["a"].url.as_deref(), Some("https://x/a2/"));
        assert!(!config.calendars.contains_key("b"));
        assert_eq!(config.accounts.len(), 2);

        let mut editor = editor_with(
            "dotted",
            "accounts.acc = { url = \"https://x/\", username = \"u\", password = \"p\" }\n\
             calendars.a = { account = \"acc\", url = \"https://x/a/\" }\n",
        );
        editor.set_calendar("a", &caldav("acc", "https://x/a3/"));
        editor.rename_account("acc", "renamed").unwrap();
        let config = editor.config().unwrap();
        assert_eq!(config.calendars["a"].account.as_deref(), Some("renamed"));
        assert_eq!(config.calendars["a"].url.as_deref(), Some("https://x/a3/"));
    }

    #[test]
    fn test_new_sync_goes_after_the_transform_tables_of_the_last_one() {
        let mut editor = editor_with(
            "after-aot",
            &format!(
                "{TWO_CALENDARS}\n[syncs.s]\nsource = \"a\"\ntarget = \"b\"\ninterval = \"5m\"\n\n\
                 # s transforms\n[[syncs.s.transforms]]\naction = \"remove_field\"\nfield = \"LOCATION\"\n"
            ),
        );
        editor.set_sync(&sync("t"));
        let text = editor.to_string();
        assert!(
            text.find("[[syncs.s.transforms]]").unwrap() < text.find("[syncs.t]").unwrap(),
            "{text}"
        );
    }

    #[test]
    fn test_durations_are_written_readably() {
        for (secs, text) in [
            (300, "5m"),
            (90, "1m 30s"),
            (5400, "1h 30m"),
            (86400, "1d"),
            (7 * 86400, "7d"),
            (60 * 86400, "60d"),
            (86401, "1day 1s"),
        ] {
            let written = duration(Duration::from_secs(secs));
            assert_eq!(written.as_str(), Some(text));
            assert_eq!(humantime::parse_duration(text).unwrap().as_secs(), secs);
        }
    }

    #[test]
    fn test_entry_text_shows_one_entry() {
        let editor = editor_with("entry-text", TWO_CALENDARS);
        assert_eq!(
            editor.entry_text("calendars", "a").as_deref(),
            Some("account = \"acc\"\nurl = \"https://x/a/\"\n")
        );
        assert_eq!(editor.entry_text("calendars", "nope"), None);
    }

    #[test]
    fn test_log_level_goes_first_and_the_default_leaves_no_table() {
        let mut editor = editor_with("log-level", TWO_CALENDARS);
        let before = editor.to_string();
        editor.set_log_level("debug");
        let text = editor.to_string();
        assert!(
            text.starts_with("version = 2\n\n[daemon]\nlog_level = \"debug\"\n"),
            "{text}"
        );
        assert_eq!(editor.config().unwrap().daemon.log_level, "debug");
        editor.set_log_level("warn");
        assert_eq!(editor.config().unwrap().daemon.log_level, "warn");
        editor.set_log_level("info");
        assert_eq!(editor.to_string(), before);
    }

    #[test]
    fn test_set_enabled_touches_only_that_key() {
        let mut editor = editor_with(
            "enabled-key",
            &format!(
                "{TWO_CALENDARS}\n[syncs.s]\n# keep this\nsource = \"a\" # and this\ntarget = \"b\"\ninterval = \"5m\"\n\n\
                 [[syncs.s.transforms]]\naction = \"remove_field\"\nfield = \"LOCATION\"\n"
            ),
        );
        let before = editor.to_string();
        editor.set_enabled("s", false).unwrap();
        let disabled = editor.to_string();
        assert!(
            disabled.contains("# keep this\nsource = \"a\" # and this\n"),
            "{disabled}"
        );
        assert!(disabled.contains("[[syncs.s.transforms]]"), "{disabled}");
        assert!(!editor.config().unwrap().syncs[0].enabled);
        editor.set_enabled("s", true).unwrap();
        assert_eq!(editor.to_string(), before);
        assert!(editor.set_enabled("nope", false).is_err());
    }
}
