//! The Settings window's model: the syncs and accounts of the config, the
//! form being edited, and the change a save or delete makes, as one
//! `SettingsChange` for the backend.

use std::collections::HashMap;

use capycal::{AccountConfig, CalendarConfig, Config, ConfigEditor, Credential, SyncConfig};

use crate::account_form::AccountForm;
pub use crate::account_form::KeychainOp;
use crate::picker::{CalendarChoice, Role};
use crate::sync_form::SyncForm;

/// One change of the config file, applied with `ConfigEditor`.
#[derive(Debug, Clone, PartialEq)]
pub enum Edit {
    SetAccount(String, AccountConfig),
    RenameAccount(String, String),
    RemoveAccount(String),
    SetCalendar(String, CalendarConfig),
    RemoveCalendar(String),
    SetSync(SyncConfig),
    RenameSync(String, String),
    RemoveSync(String),
    /// `[daemon] log_level`.
    SetLogLevel(String),
}

/// Everything one save or delete does, in the order the backend applies it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SettingsChange {
    /// Syncs whose copies are removed before the config changes.
    pub purge_first: Vec<String>,
    pub keychain: Vec<KeychainOp>,
    pub edits: Vec<Edit>,
    /// Deleted syncs whose copies stay: their state is forgotten.
    pub forget_state: Vec<String>,
    /// Purged syncs that are still configured and run again afterwards.
    pub resume: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    None,
    Sync(String),
    NewSync,
    Account(String),
    NewAccount,
    /// How the app starts, logs and where its config is.
    General,
}

/// A question before a change that removes something.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Confirm {
    pub heading: String,
    pub message: String,
    pub button: &'static str,
    pub destructive: bool,
}

pub const CHANGED_ON_DISK: &str = "Changed on disk";
const FEED_MISSING: &str = "Paste the feed's address";

pub struct Settings {
    syncs: Vec<SyncConfig>,
    accounts: Vec<(String, AccountConfig)>,
    calendars: HashMap<String, CalendarConfig>,
    /// The TOML text of every entry when it was loaded, by section and name.
    texts: HashMap<(&'static str, String), String>,
    selection: Selection,
    sync_form: Option<SyncForm>,
    account_form: Option<AccountForm>,
    /// Calendars picked in the sync form that are not in the config yet.
    new_calendars: Vec<(String, CalendarConfig)>,
    log_level: String,
    banner: Option<&'static str>,
}

impl Settings {
    pub fn new(config: &Config, editor: &ConfigEditor) -> Self {
        let mut settings = Self {
            syncs: Vec::new(),
            accounts: Vec::new(),
            calendars: HashMap::new(),
            texts: HashMap::new(),
            selection: Selection::None,
            sync_form: None,
            account_form: None,
            new_calendars: Vec::new(),
            banner: None,
            log_level: String::new(),
        };
        settings.take(config, editor);
        let first = settings
            .syncs
            .first()
            .map_or(Selection::NewSync, |s| Selection::Sync(s.name.clone()));
        settings.select(first);
        settings
    }

    fn take(&mut self, config: &Config, editor: &ConfigEditor) {
        self.log_level = config.daemon.log_level.clone();
        self.syncs = config.syncs.clone();
        let mut accounts: Vec<(String, AccountConfig)> = config
            .accounts
            .iter()
            .map(|(n, a)| (n.clone(), a.clone()))
            .collect();
        accounts.sort_by(|a, b| a.0.cmp(&b.0));
        self.accounts = accounts;
        self.calendars = config.calendars.clone();
        self.texts.clear();
        let names = [
            ("syncs", self.sync_names()),
            ("accounts", self.account_names()),
            ("calendars", self.calendars.keys().cloned().collect()),
        ];
        for (section, names) in names {
            for name in names {
                if let Some(text) = editor.entry_text(section, &name) {
                    self.texts.insert((section, name), text);
                }
            }
        }
    }

    /// The key of a configured calendar equal to `calendar`.
    pub fn calendar_key_for(&self, calendar: &CalendarConfig) -> Option<String> {
        let same = |c: &CalendarConfig| match (&c.eventkit_calendar, &calendar.eventkit_calendar) {
            _ if c.is_ics() || calendar.is_ics() => c.ics_url == calendar.ics_url,
            (Some(a), Some(b)) => a == b,
            (None, None) => {
                c.account == calendar.account
                    && c.url.as_deref().map(|u| u.trim_end_matches('/'))
                        == calendar.url.as_deref().map(|u| u.trim_end_matches('/'))
            }
            _ => false,
        };
        let mut keys: Vec<&String> = self
            .calendars
            .iter()
            .filter(|(_, c)| same(c))
            .map(|(k, _)| k)
            .collect();
        keys.sort();
        keys.first().map(|k| k.to_string())
    }

    pub fn calendar_keys(&self) -> Vec<String> {
        self.calendars.keys().cloned().collect()
    }

    pub fn first_start(&self) -> bool {
        self.syncs.is_empty()
    }

    pub fn selection(&self) -> &Selection {
        &self.selection
    }

    pub fn sync_names(&self) -> Vec<String> {
        self.syncs.iter().map(|s| s.name.clone()).collect()
    }

    /// The saved target of `sync` and every saved sync writing into it.
    pub fn target_of(&self, sync: &str) -> Option<(String, Vec<String>)> {
        let target = &self.syncs.iter().find(|s| s.name == sync)?.target;
        let writing = (self.syncs.iter())
            .filter(|s| s.target == *target)
            .map(|s| s.name.clone())
            .collect();
        Some((target.clone(), writing))
    }

    /// The calendar picked in the sync form under `key` that is not in the
    /// config yet.
    pub fn new_calendar(&self, key: &str) -> Option<&CalendarConfig> {
        self.new_calendars
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, c)| c)
    }

    /// `[daemon] log_level` of the config as loaded.
    pub fn log_level(&self) -> &str {
        &self.log_level
    }

    /// The calendar under `key`, just picked or changed in the form, else
    /// as configured.
    pub fn calendar(&self, key: &str) -> Option<&CalendarConfig> {
        self.new_calendar(key).or_else(|| self.calendars.get(key))
    }

    /// Set the address of the sync form's source, a feed. A feed new in this
    /// form takes its key from the address.
    pub fn set_feed_url(&mut self, text: &str) {
        let Some(key) = self.sync_form.as_ref().map(|f| f.source().to_string()) else {
            return;
        };
        let Some(mut calendar) = self.calendar(&key).filter(|c| c.is_ics()).cloned() else {
            return;
        };
        calendar.ics_url = Some(Credential::Plain(text.trim().to_string()));
        match self.calendars.get(&key) {
            // The address of a configured feed picks that one
            None => {
                let (key, new_entry) = match self.calendar_key_for(&calendar) {
                    Some(existing) => (existing, false),
                    None => (crate::picker::feed_key(text), true),
                };
                self.pick(
                    Role::Source,
                    CalendarChoice {
                        key,
                        calendar,
                        new_entry,
                    },
                );
            }
            Some(saved) => {
                let changed = *saved != calendar;
                self.new_calendars.retain(|(k, _)| *k != key);
                if changed {
                    self.new_calendars.push((key, calendar));
                }
            }
        }
    }

    /// What is wrong with the address of the sync form's source feed; an
    /// empty one only keeps Save off (`feed_problem`).
    pub fn feed_error(&self) -> Option<&'static str> {
        self.feed_problem().filter(|e| *e != FEED_MISSING)
    }

    /// Why the source feed's address cannot be read yet, also when empty.
    pub fn feed_problem(&self) -> Option<&'static str> {
        let source = self.sync_form.as_ref()?.source();
        match &self.calendar(source)?.ics_url {
            Some(Credential::Plain(url)) if url.is_empty() => Some(FEED_MISSING),
            Some(Credential::Plain(url)) if !crate::picker::feed_url_valid(url) => {
                Some(crate::picker::FEED_INVALID)
            }
            _ => None,
        }
    }

    /// Whether the calendar under `key` (configured or just picked) is a
    /// Calendar.app one.
    pub fn is_eventkit_calendar(&self, key: &str) -> bool {
        self.calendars
            .get(key)
            .or_else(|| self.new_calendar(key))
            .is_some_and(|c| c.eventkit_calendar.is_some())
    }

    pub fn sync_enabled(&self, name: &str) -> bool {
        self.syncs.iter().any(|s| s.name == name && s.enabled)
    }

    /// The syncs whose source or target is a calendar of this account.
    pub fn syncs_using(&self, account: &str) -> Vec<String> {
        let of_account = |key: &str| {
            self.calendars
                .get(key)
                .is_some_and(|c| c.account.as_deref() == Some(account))
        };
        self.syncs
            .iter()
            .filter(|s| of_account(&s.source) || of_account(&s.target))
            .map(|s| s.name.clone())
            .collect()
    }

    pub fn account_names(&self) -> Vec<String> {
        self.accounts.iter().map(|(n, _)| n.clone()).collect()
    }

    /// Show an entry; its form starts from the config.
    /// A new sync copied from the sync `name`, not saved yet.
    pub fn select_duplicate(&mut self, name: &str) {
        let Some(sync) = self.sync(name).cloned() else {
            return;
        };
        self.select(Selection::NewSync);
        self.sync_form = Some(SyncForm::duplicate(&sync, &self.sync_names()));
        self.load_concurrency();
    }

    /// Give the form the `concurrency` of its target calendar.
    fn load_concurrency(&mut self) {
        let Some(form) = &self.sync_form else {
            return;
        };
        let concurrency = self
            .calendar(form.target())
            .map_or(capycal::DEFAULT_CONCURRENCY, |c| c.concurrency);
        if let Some(form) = self.sync_form.as_mut() {
            form.set_target_concurrency(concurrency);
        }
    }

    pub fn select(&mut self, selection: Selection) {
        self.sync_form = None;
        self.account_form = None;
        self.new_calendars.clear();
        self.banner = None;
        match &selection {
            Selection::Sync(name) => {
                self.sync_form = self.sync(name).map(SyncForm::edit);
            }
            Selection::NewSync => self.sync_form = Some(SyncForm::new_sync(&self.sync_names())),
            Selection::Account(name) => {
                self.account_form = self.account(name).map(|a| AccountForm::edit(name, a));
            }
            Selection::NewAccount => self.account_form = Some(AccountForm::new_account()),
            Selection::None | Selection::General => {}
        }
        self.selection = selection;
        self.load_concurrency();
    }

    fn sync(&self, name: &str) -> Option<&SyncConfig> {
        self.syncs.iter().find(|s| s.name == name)
    }

    fn account(&self, name: &str) -> Option<&AccountConfig> {
        self.accounts
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, a)| a)
    }

    pub fn sync_form(&mut self) -> Option<&mut SyncForm> {
        self.sync_form.as_mut()
    }

    pub fn account_form(&mut self) -> Option<&mut AccountForm> {
        self.account_form.as_mut()
    }

    /// Use a calendar as source or target. A new entry gets a key that no
    /// configured or other new entry has.
    pub fn pick(&mut self, role: Role, choice: CalendarChoice) {
        let Some(form) = self.sync_form.as_mut() else {
            return;
        };
        let other = match role {
            Role::Source => form.target().to_string(),
            Role::Target => form.source().to_string(),
        };
        // Only the other role's new entry stays pending
        self.new_calendars.retain(|(k, _)| *k == other);
        let mut key = choice.key;
        if choice.new_entry {
            let base = key.clone();
            key = (1..)
                .map(|n| match n {
                    1 => base.clone(),
                    n => format!("{base}_{n}"),
                })
                .find(|k| {
                    !self.calendars.contains_key(k)
                        && !self.new_calendars.iter().any(|(p, _)| p == k)
                })
                .unwrap_or(base);
            self.new_calendars.push((key.clone(), choice.calendar));
        }
        match role {
            Role::Source => form.set_source(&key),
            Role::Target => form.set_target(&key),
        }
        let names: Vec<String> = self.syncs.iter().map(|s| s.name.clone()).collect();
        form.propose_name(&names);
        if role == Role::Target {
            self.load_concurrency();
        }
    }

    /// Calendars that no sync but `without` uses after the change, among
    /// `candidates`.
    fn unused(&self, candidates: &[&str], without: &str, still_used: &[&str]) -> Vec<String> {
        let mut unused: Vec<String> = Vec::new();
        for key in candidates {
            let used = still_used.contains(key)
                || self
                    .syncs
                    .iter()
                    .filter(|s| s.name != without)
                    .any(|s| s.source == *key || s.target == *key);
            if !used && self.calendars.contains_key(*key) && !unused.iter().any(|u| u == key) {
                unused.push(key.to_string());
            }
        }
        unused
    }

    fn unused_note(unused: &[String]) -> String {
        unused
            .iter()
            .map(|k| format!("\n\n{k} is no longer used and is removed from the config."))
            .collect()
    }

    pub fn save_sync(&self) -> Result<(SettingsChange, Option<Confirm>), String> {
        let form = self.sync_form.as_ref().ok_or("No sync selected")?;
        let original = form.original_name().and_then(|n| self.sync(n));
        let others: Vec<String> = self
            .sync_names()
            .into_iter()
            .filter(|n| Some(n.as_str()) != form.original_name())
            .collect();
        let sync = form.build(&others)?;
        if let Some(e) = self.feed_problem() {
            return Err(e.to_string());
        }
        let mut change = SettingsChange::default();
        let concurrency = form.concurrency_change();
        let with_concurrency = |key: &str, calendar: &CalendarConfig| match concurrency {
            Some(n) if key == sync.target => CalendarConfig {
                concurrency: n,
                ..calendar.clone()
            },
            _ => calendar.clone(),
        };
        for (key, calendar) in &self.new_calendars {
            if *key == sync.source || *key == sync.target {
                change.edits.push(Edit::SetCalendar(
                    key.clone(),
                    with_concurrency(key, calendar),
                ));
            }
        }
        if let Some(calendar) = self
            .calendars
            .get(&sync.target)
            .filter(|_| concurrency.is_some())
        {
            change.edits.push(Edit::SetCalendar(
                sync.target.clone(),
                with_concurrency(&sync.target, calendar),
            ));
        }
        let renamed = original.filter(|o| o.name != sync.name);
        if let Some(old) = renamed {
            change
                .edits
                .push(Edit::RenameSync(old.name.clone(), sync.name.clone()));
        }
        change.edits.push(Edit::SetSync(sync.clone()));
        let unused = match original {
            Some(o) => self.unused(
                &[&o.source, &o.target],
                &o.name,
                &[&sync.source, &sync.target],
            ),
            None => Vec::new(),
        };
        change
            .edits
            .extend(unused.iter().cloned().map(Edit::RemoveCalendar));

        // Against the sync on disk, which an Overwrite may have changed
        let purge_old = original
            .filter(|o| o.name != sync.name || o.target != sync.target)
            .map(|o| o.name.clone());
        let confirm = match (&purge_old, original) {
            (Some(old), Some(o)) => {
                change.purge_first.push(old.clone());
                if renamed.is_none() {
                    change.resume.push(sync.name.clone());
                }
                Some(Confirm {
                    heading: "Remove synced events?".to_string(),
                    message: format!(
                        "The events \"{old}\" copied into {} are removed, then \"{}\" copies \
                         the source again.{}",
                        o.target,
                        sync.name,
                        Self::unused_note(&unused)
                    ),
                    button: "Remove and Sync Again",
                    destructive: true,
                })
            }
            _ if !unused.is_empty() => Some(Confirm {
                heading: format!("Save \"{}\"?", sync.name),
                message: Self::unused_note(&unused).trim_start().to_string(),
                button: "Save",
                destructive: false,
            }),
            _ => None,
        };
        Ok((change, confirm))
    }

    /// Remove the selected sync, its copies unless `keep_copies`, and the
    /// calendars only it used.
    pub fn delete_sync(&self, keep_copies: bool) -> (SettingsChange, Confirm) {
        let mut change = SettingsChange::default();
        let Selection::Sync(name) = &self.selection else {
            return (
                change,
                Confirm {
                    heading: String::new(),
                    message: String::new(),
                    button: "Remove Sync",
                    destructive: true,
                },
            );
        };
        let sync = self.sync(name);
        let target = sync.map(|s| s.target.clone()).unwrap_or_default();
        let unused = match sync {
            Some(s) => self.unused(&[&s.source, &s.target], name, &[]),
            None => Vec::new(),
        };
        change.edits.push(Edit::RemoveSync(name.clone()));
        change
            .edits
            .extend(unused.iter().cloned().map(Edit::RemoveCalendar));
        let copies = if keep_copies {
            change.forget_state.push(name.clone());
            format!("Its events in {target} stay; Capycal forgets them.")
        } else {
            change.purge_first.push(name.clone());
            format!("Its events in {target} are removed too.")
        };
        let confirm = Confirm {
            heading: format!("Remove \"{name}\"?"),
            message: format!("{copies}{}", Self::unused_note(&unused)),
            button: "Remove Sync",
            destructive: true,
        };
        (change, confirm)
    }

    pub fn save_account(&self) -> Result<SettingsChange, String> {
        let form = self.account_form.as_ref().ok_or("No account selected")?;
        let others: Vec<String> = self
            .account_names()
            .into_iter()
            .filter(|n| Some(n.as_str()) != form.original_name())
            .collect();
        let (name, account, keychain, renamed) = form.build(&others)?;
        let mut change = SettingsChange {
            keychain,
            ..SettingsChange::default()
        };
        if let Some(old) = renamed {
            change.edits.push(Edit::RenameAccount(old, name.clone()));
        }
        change.edits.push(Edit::SetAccount(name, account));
        Ok(change)
    }

    /// Remove the selected account and its calendars; refused while a sync
    /// uses one of them.
    pub fn delete_account(&self) -> Result<SettingsChange, String> {
        let Selection::Account(name) = &self.selection else {
            return Err("No account selected".to_string());
        };
        let mut calendars: Vec<&String> = self
            .calendars
            .iter()
            .filter(|(_, c)| c.account.as_deref() == Some(name.as_str()))
            .map(|(k, _)| k)
            .collect();
        calendars.sort();
        let users: Vec<&str> = self
            .syncs
            .iter()
            .filter(|s| calendars.contains(&&s.source) || calendars.contains(&&s.target))
            .map(|s| s.name.as_str())
            .collect();
        if !users.is_empty() {
            return Err(format!(
                "Used by the syncs {}. Remove them first.",
                users.join(", ")
            ));
        }
        let mut change = SettingsChange::default();
        change.edits.extend(
            calendars
                .into_iter()
                .map(|k| Edit::RemoveCalendar(k.clone())),
        );
        change.edits.push(Edit::RemoveAccount(name.clone()));
        if self.account(name).map(|a| &a.password)
            == Some(&Credential::Keychain {
                keychain: name.clone(),
            })
        {
            change
                .keychain
                .push(KeychainOp::Delete { name: name.clone() });
        }
        Ok(change)
    }

    /// Drop the draft: the form shows the config again.
    pub fn revert(&mut self) {
        self.select(self.selection.clone());
    }

    fn selected_key(&self) -> Option<(&'static str, String)> {
        match &self.selection {
            Selection::Sync(n) => Some(("syncs", n.clone())),
            Selection::Account(n) => Some(("accounts", n.clone())),
            _ => None,
        }
    }

    fn is_dirty(&self) -> bool {
        // A configured feed whose address changed
        let feed_changed = self
            .new_calendars
            .iter()
            .any(|(k, c)| self.calendars.get(k).is_some_and(|saved| saved != c));
        feed_changed
            || self.sync_form.as_ref().is_some_and(SyncForm::is_dirty)
            || self
                .account_form
                .as_ref()
                .is_some_and(AccountForm::is_dirty)
    }

    /// The file changed on disk. Without a draft the form follows it; a
    /// draft stays, with a banner if its own entry changed.
    pub fn file_changed(&mut self, config: &Config, editor: &ConfigEditor) {
        let key = self.selected_key();
        let before = key.as_ref().and_then(|k| self.texts.get(k).cloned());
        self.take(config, editor);
        let after = key.as_ref().and_then(|k| self.texts.get(k).cloned());
        if self.is_dirty() {
            if before != after {
                self.banner = Some(CHANGED_ON_DISK);
            }
            return;
        }
        let selection = match &self.selection {
            Selection::Sync(n) if self.sync(n).is_none() => Selection::None,
            Selection::Account(n) if self.account(n).is_none() => Selection::None,
            s => s.clone(),
        };
        self.select(selection);
    }

    /// The app's own save went through: take the new file and show
    /// `selection` with a clean form.
    pub fn saved(&mut self, config: &Config, editor: &ConfigEditor, selection: Selection) {
        self.take(config, editor);
        self.select(selection);
    }

    pub fn sync_form_ref(&self) -> Option<&SyncForm> {
        self.sync_form.as_ref()
    }

    pub fn account_form_ref(&self) -> Option<&AccountForm> {
        self.account_form.as_ref()
    }

    /// Unsaved changes in the selected form.
    pub fn dirty(&self) -> bool {
        self.is_dirty()
    }

    /// Why the selected form cannot be saved, once it was changed.
    pub fn problem(&self) -> Option<String> {
        if let Some(form) = &self.sync_form {
            let others: Vec<String> = self
                .sync_names()
                .into_iter()
                .filter(|n| Some(n.as_str()) != form.original_name())
                .collect();
            if let Some(e) = self.feed_problem() {
                return Some(e.to_string());
            }
            return form.is_dirty().then(|| form.build(&others).err()).flatten();
        }
        if let Some(form) = &self.account_form {
            let others: Vec<String> = self
                .account_names()
                .into_iter()
                .filter(|n| Some(n.as_str()) != form.original_name())
                .collect();
            return form.is_dirty().then(|| form.build(&others).err()).flatten();
        }
        None
    }

    pub fn banner(&self) -> Option<&str> {
        self.banner
    }

    /// Keep the draft over the file's version; saving writes it.
    pub fn overwrite(&mut self) {
        self.banner = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::picker::{CalendarChoice, Role};
    use crate::sync_form::SyncField;
    use capycal::{CalendarConfig, Config, ConfigEditor, Credential, SyncConfig};

    const CONFIG: &str = r#"[accounts.work]
url = "https://x/"
username = "u"
password = { keychain = "work" }

[accounts.spare]
url = "https://y/"
username = "u"
password = { keychain = "spare" }

[calendars.a]
account = "work"
url = "https://x/a/"

[calendars.b]
account = "work"
url = "https://x/b/"

[calendars.c]
account = "work"
url = "https://x/c/"

[syncs.a-to-b]
source = "a"
target = "b"
interval = "5m"
fields = ["SUMMARY"]

[syncs.c-to-b]
source = "c"
target = "b"
interval = "5m"
"#;

    fn path(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("capycal-{}-settings-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("capycal.toml")
    }

    fn load(path: &std::path::Path) -> (Config, ConfigEditor) {
        let editor = ConfigEditor::load(path).unwrap();
        (editor.config().unwrap(), editor)
    }

    fn settings(name: &str) -> (Settings, std::path::PathBuf) {
        let path = path(name);
        std::fs::write(&path, CONFIG).unwrap();
        let (config, editor) = load(&path);
        (Settings::new(&config, &editor), path)
    }

    fn caldav(url: &str) -> CalendarConfig {
        CalendarConfig {
            account: Some("work".into()),
            url: Some(url.into()),
            eventkit_calendar: None,
            ics_url: None,
            concurrency: capycal::DEFAULT_CONCURRENCY,
        }
    }

    fn sync(name: &str, source: &str, target: &str, fields: &[&str]) -> SyncConfig {
        let mut s = SyncConfig::new(name, source, target);
        s.fields = fields.iter().map(|f| f.to_string()).collect();
        s
    }

    #[test]
    fn test_saving_a_new_sync_with_a_new_calendar_entry() {
        let (mut s, _) = settings("new-sync");
        s.select(Selection::NewSync);
        s.pick(
            Role::Source,
            CalendarChoice {
                key: "a".into(),
                calendar: caldav("https://x/a/"),
                new_entry: false,
            },
        );
        s.pick(
            Role::Target,
            CalendarChoice {
                key: "d".into(),
                calendar: caldav("https://x/d/"),
                new_entry: true,
            },
        );
        let (change, confirm) = s.save_sync().unwrap();
        assert_eq!(confirm, None);
        assert_eq!(
            change.edits,
            [
                Edit::SetCalendar("d".into(), caldav("https://x/d/")),
                Edit::SetSync(sync("a-to-d", "a", "d", &[])),
            ]
        );
        assert!(change.purge_first.is_empty() && change.keychain.is_empty());
    }

    #[test]
    fn test_renaming_and_retargeting_purges_once_and_resumes_nothing() {
        let (mut s, _) = settings("rename");
        s.select(Selection::Sync("a-to-b".into()));
        let form = s.sync_form().unwrap();
        form.set_name("renamed");
        form.set_target("c");
        let (change, confirm) = s.save_sync().unwrap();
        let confirm = confirm.unwrap();
        assert_eq!(confirm.button, "Remove and Sync Again");
        assert!(confirm.destructive);
        assert!(
            confirm.message.contains("\"a-to-b\""),
            "{}",
            confirm.message
        );
        assert_eq!(change.purge_first, ["a-to-b"]);
        assert!(change.resume.is_empty());
        assert_eq!(
            change.edits,
            [
                Edit::RenameSync("a-to-b".into(), "renamed".into()),
                Edit::SetSync(sync("renamed", "a", "c", &["SUMMARY"])),
            ]
        );
    }

    #[test]
    fn test_changing_only_the_target_resumes_the_purged_sync() {
        let (mut s, _) = settings("retarget");
        s.select(Selection::Sync("a-to-b".into()));
        s.sync_form().unwrap().set_target("c");
        let (change, confirm) = s.save_sync().unwrap();
        assert!(confirm.is_some());
        assert_eq!(change.purge_first, ["a-to-b"]);
        assert_eq!(change.resume, ["a-to-b"]);
    }

    #[test]
    fn test_saving_unchanged_names_needs_no_confirmation() {
        let (mut s, _) = settings("plain-save");
        s.select(Selection::Sync("a-to-b".into()));
        s.sync_form().unwrap().set_interval("1h");
        let (change, confirm) = s.save_sync().unwrap();
        assert_eq!(confirm, None);
        assert!(change.purge_first.is_empty());
        assert_eq!(change.edits.len(), 1);
    }

    #[test]
    fn test_parallel_writes_change_the_target_calendar() {
        let (mut s, _) = settings("concurrency");
        s.select(Selection::Sync("a-to-b".into()));
        let form = s.sync_form().unwrap();
        assert_eq!(form.text(SyncField::Concurrency), "2", "the default");
        form.set_concurrency("2");
        assert!(!form.is_dirty());
        form.set_concurrency("x");
        assert!(form.build(&[]).is_err());
        form.set_concurrency("4");
        assert!(form.is_dirty());
        let (change, confirm) = s.save_sync().unwrap();
        assert_eq!(confirm, None);
        assert_eq!(
            change.edits,
            [
                Edit::SetCalendar(
                    "b".into(),
                    CalendarConfig {
                        concurrency: 4,
                        ..caldav("https://x/b/")
                    }
                ),
                Edit::SetSync(sync("a-to-b", "a", "b", &["SUMMARY"])),
            ]
        );

        // A new target entry gets it too; picking another target drops it
        s.select(Selection::NewSync);
        s.sync_form().unwrap().set_concurrency("3");
        s.pick(
            Role::Target,
            CalendarChoice {
                key: "d".into(),
                calendar: caldav("https://x/d/"),
                new_entry: true,
            },
        );
        assert_eq!(s.sync_form().unwrap().text(SyncField::Concurrency), "2");
        s.sync_form().unwrap().set_concurrency("1");
        s.pick(
            Role::Source,
            CalendarChoice {
                key: "a".into(),
                calendar: caldav("https://x/a/"),
                new_entry: false,
            },
        );
        let (change, _) = s.save_sync().unwrap();
        assert_eq!(
            change.edits[0],
            Edit::SetCalendar(
                "d".into(),
                CalendarConfig {
                    concurrency: 1,
                    ..caldav("https://x/d/")
                }
            )
        );
        assert_eq!(change.edits.len(), 2);
    }

    #[test]
    fn test_deleting_a_sync_removes_unused_calendars_and_optionally_keeps_copies() {
        let (mut s, _) = settings("delete-sync");
        s.select(Selection::Sync("c-to-b".into()));
        let (change, confirm) = s.delete_sync(false);
        assert_eq!(change.purge_first, ["c-to-b"]);
        assert_eq!(
            change.edits,
            [
                Edit::RemoveSync("c-to-b".into()),
                Edit::RemoveCalendar("c".into())
            ]
        );
        assert_eq!(confirm.button, "Remove Sync");
        assert!(confirm.destructive);
        assert!(
            confirm.message.contains("c is no longer used"),
            "{}",
            confirm.message
        );

        let (change, confirm) = s.delete_sync(true);
        assert!(change.purge_first.is_empty());
        assert_eq!(change.forget_state, ["c-to-b"]);
        assert!(confirm.message.contains("stay"), "{}", confirm.message);
    }

    #[test]
    fn test_account_deletion_is_blocked_by_syncs() {
        let (mut s, _) = settings("delete-account");
        s.select(Selection::Account("work".into()));
        assert_eq!(
            s.delete_account().unwrap_err(),
            "Used by the syncs a-to-b, c-to-b. Remove them first."
        );
        s.select(Selection::Account("spare".into()));
        let change = s.delete_account().unwrap();
        assert_eq!(change.edits, [Edit::RemoveAccount("spare".into())]);
        assert_eq!(
            change.keychain,
            [KeychainOp::Delete {
                name: "spare".into()
            }]
        );
    }

    #[test]
    fn test_account_rename_follows_its_keychain_entry() {
        let (mut s, _) = settings("rename-account");
        s.select(Selection::Account("work".into()));
        s.account_form().unwrap().set_name("office");
        let change = s.save_account().unwrap();
        assert_eq!(
            change.keychain,
            [KeychainOp::Rename {
                old: "work".into(),
                new: "office".into()
            }]
        );
        match &change.edits[..] {
            [
                Edit::RenameAccount(old, new),
                Edit::SetAccount(name, account),
            ] => {
                assert_eq!(
                    (old.as_str(), new.as_str(), name.as_str()),
                    ("work", "office", "office")
                );
                assert_eq!(
                    account.password,
                    Credential::Keychain {
                        keychain: "office".into()
                    }
                );
            }
            edits => panic!("{edits:?}"),
        }
    }

    #[test]
    fn test_external_change_reloads_or_shows_a_banner() {
        let (mut s, path) = settings("external");
        s.select(Selection::Sync("a-to-b".into()));
        let rewrite = |from: &str, to: &str| {
            let text = std::fs::read_to_string(&path).unwrap();
            std::fs::write(&path, text.replacen(from, to, 1)).unwrap();
            load(&path)
        };

        // No draft: the form follows the file
        let (config, editor) = rewrite("interval = \"5m\"", "interval = \"1h\"");
        s.file_changed(&config, &editor);
        assert_eq!(s.banner(), None);
        assert_eq!(s.sync_form().unwrap().text(SyncField::Interval), "1h");

        // A draft of the same sync: banner, draft kept
        s.sync_form().unwrap().set_interval("2h");
        let (config, editor) = rewrite("interval = \"1h\"", "interval = \"3h\"");
        s.file_changed(&config, &editor);
        assert_eq!(s.banner(), Some("Changed on disk"));
        assert_eq!(s.sync_form().unwrap().text(SyncField::Interval), "2h");
        s.overwrite();
        assert_eq!(s.banner(), None);
        assert_eq!(s.sync_form().unwrap().text(SyncField::Interval), "2h");
        s.revert();
        assert_eq!(s.sync_form().unwrap().text(SyncField::Interval), "3h");

        // A draft and a change of another sync: no banner
        s.sync_form().unwrap().set_interval("2h");
        let text = std::fs::read_to_string(&path).unwrap();
        let other = text.rfind("interval = \"5m\"").unwrap();
        std::fs::write(
            &path,
            format!("{}interval = \"9m\"{}", &text[..other], &text[other + 15..]),
        )
        .unwrap();
        let (config, editor) = load(&path);
        s.file_changed(&config, &editor);
        assert_eq!(s.banner(), None);
        assert_eq!(s.sync_form().unwrap().text(SyncField::Interval), "2h");
    }

    #[test]
    fn test_first_start_without_syncs() {
        let path = path("first");
        std::fs::write(&path, "").unwrap();
        let (config, editor) = load(&path);
        let s = Settings::new(&config, &editor);
        assert!(s.first_start());
        assert_eq!(*s.selection(), Selection::NewSync);
    }

    #[test]
    fn test_same_named_new_calendars_get_their_own_keys() {
        let (mut s, _) = settings("same-names");
        s.select(Selection::NewSync);
        let pick = |url: &str| CalendarChoice {
            key: "calendar".into(),
            calendar: caldav(url),
            new_entry: true,
        };
        s.pick(Role::Source, pick("https://x/one/"));
        s.pick(Role::Target, pick("https://x/two/"));
        let (change, _) = s.save_sync().unwrap();
        assert_eq!(
            change.edits,
            [
                Edit::SetCalendar("calendar".into(), caldav("https://x/one/")),
                Edit::SetCalendar("calendar_2".into(), caldav("https://x/two/")),
                Edit::SetSync(sync(
                    "calendar-to-calendar_2",
                    "calendar",
                    "calendar_2",
                    &[]
                )),
            ]
        );
        // Picking the source again replaces its pending entry
        s.pick(Role::Source, pick("https://x/three/"));
        let (change, _) = s.save_sync().unwrap();
        assert_eq!(change.edits.len(), 3, "{:?}", change.edits);
    }

    #[test]
    fn test_overwrite_compares_with_the_version_on_disk() {
        let (mut s, path) = settings("overwrite-impact");
        s.select(Selection::Sync("a-to-b".into()));
        s.sync_form().unwrap().set_interval("2h");
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, text.replacen("target = \"b\"", "target = \"c\"", 1)).unwrap();
        let (config, editor) = load(&path);
        s.file_changed(&config, &editor);
        s.overwrite();
        let (change, confirm) = s.save_sync().unwrap();
        assert_eq!(change.purge_first, ["a-to-b"]);
        assert!(confirm.is_some());
    }

    #[test]
    fn test_saved_rebases_the_form_on_the_new_file() {
        let (mut s, path) = settings("saved");
        s.select(Selection::Sync("a-to-b".into()));
        s.sync_form().unwrap().set_name("renamed");
        let (change, _) = s.save_sync().unwrap();
        assert!(!change.edits.is_empty());
        // What the backend writes
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, text.replace("[syncs.a-to-b]", "[syncs.renamed]")).unwrap();
        let (config, editor) = load(&path);
        s.saved(&config, &editor, Selection::Sync("renamed".into()));
        assert_eq!(*s.selection(), Selection::Sync("renamed".into()));
        assert!(!s.sync_form().unwrap().is_dirty());
        s.file_changed(&config, &editor);
        assert_eq!(s.banner(), None);
    }
}
