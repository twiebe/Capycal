//! The sync side of the app: owns config, lock and coordinator on the tokio
//! runtime and reports a `Snapshot` after every change.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use capycal::{
    CalendarInfo, Config, ConfigEditor, ConfigLock, ConfigWatcher, Coordinator, Credential, Error,
    LogLevel, STARTER_CONFIG, SyncStatus,
};
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::settings::{Edit, KeychainOp, SettingsChange};

/// Status changes arriving within this time are reported together.
const STATUS_THROTTLE: Duration = Duration::from_millis(300);

pub enum Command {
    SyncNow,
    /// Make the next cycle of this sync, or of every sync, a full sync,
    /// starting it now.
    FullSync(Option<String>),
    Reload,
    /// Create the config if needed, then answer with `Update::ConfigReady`.
    EditConfig,
    /// Count the events a purge of each sync would delete. Answered with
    /// `Update::PurgePreview`.
    CountPurge,
    /// Delete the events of these syncs from their targets, forget their
    /// state and disable them in the config.
    Purge(Vec<String>),
    /// Count every event in this calendar (key in the config), answered
    /// with `Update::CalendarCounted`.
    CountCalendar(String),
    /// Delete every event in this calendar, forget the state of the syncs
    /// writing to it and disable them. Answered with `Update::CalendarPurged`.
    PurgeCalendar(String),
    /// List the calendars of a configured account, answered with
    /// `Update::AccountCalendars`.
    DiscoverAccount(String),
    /// Try an account's URL and user name with a typed password, or with
    /// the stored one of `account`. Answered with `Update::AccountTested`.
    TestAccount {
        account: Option<String>,
        url: String,
        username: String,
        password: Option<String>,
    },
    /// List the Calendar.app calendars, answered with
    /// `Update::CalendarAppCalendars`.
    ListCalendarApp,
    /// Read the next event of `sync`'s source, `calendar` (configured or
    /// not yet), for the preview. Answered with `Update::NextSourceEvent`.
    NextSourceEvent {
        calendar: capycal::CalendarConfig,
        sync: capycal::SyncConfig,
    },
    /// Apply a change of the Settings window, answered with
    /// `Update::SettingsSaved`.
    SaveSettings(SettingsChange),
    /// The app closed the note of `Update::KeychainNotice`: load the config.
    KeychainNoticeClosed,
    /// Stop all syncs, then signal the sender.
    Shutdown(std::sync::mpsc::Sender<()>),
}

// By hand, so a typed password never shows (CLAUDE.md "Security")
impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Command::SyncNow => f.write_str("SyncNow"),
            Command::FullSync(sync) => f.debug_tuple("FullSync").field(sync).finish(),
            Command::Reload => f.write_str("Reload"),
            Command::EditConfig => f.write_str("EditConfig"),
            Command::CountPurge => f.write_str("CountPurge"),
            Command::Purge(syncs) => f.debug_tuple("Purge").field(syncs).finish(),
            Command::CountCalendar(c) => f.debug_tuple("CountCalendar").field(c).finish(),
            Command::PurgeCalendar(c) => f.debug_tuple("PurgeCalendar").field(c).finish(),
            Command::DiscoverAccount(account) => {
                f.debug_tuple("DiscoverAccount").field(account).finish()
            }
            Command::TestAccount {
                account,
                url,
                username,
                password,
            } => f
                .debug_struct("TestAccount")
                .field("account", account)
                .field("url", url)
                .field("username", username)
                .field("password", &password.as_ref().map(|_| "***"))
                .finish(),
            Command::ListCalendarApp => f.write_str("ListCalendarApp"),
            Command::NextSourceEvent { sync, .. } => f
                .debug_tuple("NextSourceEvent")
                .field(&sync.source)
                .finish(),
            Command::SaveSettings(change) => f.debug_tuple("SaveSettings").field(change).finish(),
            Command::KeychainNoticeClosed => f.write_str("KeychainNoticeClosed"),
            Command::Shutdown(_) => f.write_str("Shutdown"),
        }
    }
}

/// What the backend reports to the app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Update {
    Snapshot(Snapshot),
    /// The config file can be opened, or why not.
    ConfigReady(Result<(), String>),
    PurgePreview(PurgePreview),
    /// A purge (`Command::Purge`, or a saved change in Settings) has
    /// handled `done` events of `sync` so far; `total` is counted before a
    /// purge from Settings.
    PurgeProgress {
        sync: String,
        done: usize,
        total: Option<usize>,
    },
    /// A `Command::Purge` finished.
    Purged(Vec<PurgeOutcome>),
    CalendarCounted {
        calendar: String,
        events: Result<usize, String>,
    },
    /// A `Command::PurgeCalendar` has handled `done` events so far.
    CalendarPurgeProgress {
        calendar: String,
        done: usize,
    },
    /// A `Command::PurgeCalendar` finished: the deleted events and the
    /// ones that could not be deleted.
    CalendarPurged {
        calendar: String,
        result: Result<(usize, usize), String>,
    },
    /// A (re)load failed because of the config or its lock; the app shows
    /// the Problems window.
    ReloadFailed,
    AccountCalendars {
        account: String,
        result: Result<Vec<capycal::DiscoveredCalendar>, String>,
    },
    /// The number of calendars found with the tested credentials.
    AccountTested(Result<usize, String>),
    CalendarAppCalendars(Result<Vec<CalendarInfo>, String>),
    /// The next event of the calendar `source` as iCal, None when nothing
    /// is ahead.
    NextSourceEvent {
        source: String,
        result: Result<Option<String>, String>,
    },
    SettingsSaved(Result<(), String>),
    /// The config was (re)loaded or failed to; the Settings window rereads it.
    ConfigChanged,
    /// macOS would ask before capycal reads the Keychain entries of these
    /// accounts. The backend loads the config after
    /// `Command::KeychainNoticeClosed`.
    KeychainNotice(Vec<String>),
}

/// The keychain functions the backend calls; tests replace them.
#[derive(Clone, Copy)]
pub struct Keychain {
    pub password: fn(&str) -> Result<String, String>,
    pub set: fn(&str, &str) -> Result<(), String>,
    pub delete: fn(&str) -> Result<(), String>,
    /// Whether reading the entry would make macOS ask first.
    pub prompts: fn(&str) -> bool,
}

impl Default for Keychain {
    fn default() -> Self {
        Self {
            password: |name| capycal::keychain_password(name).map_err(|e| e.to_string()),
            set: |name, password| {
                capycal::set_keychain_password(name, password).map_err(|e| e.to_string())
            },
            delete: |name| capycal::delete_keychain_password(name).map_err(|e| e.to_string()),
            prompts: capycal::keychain_would_prompt,
        }
    }
}

/// Why the config is not (or not fully) running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    NoConfig,
    Config(String),
    /// Another process holds the config lock.
    Locked {
        pid: String,
    },
    Purge(String),
}

/// What the menu shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    /// Set once the first config load finished.
    pub loaded: bool,
    pub problem: Option<Problem>,
    /// A longer operation in progress, such as counting or removing events.
    pub activity: Option<String>,
    pub syncs: Vec<SyncStatus>,
}

/// The events a purge would delete, per sync.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PurgePreview {
    pub counts: Vec<PurgeCount>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PurgeCount {
    pub sync: String,
    /// Key of the sink calendar in the config.
    pub target: String,
    pub events: Result<usize, String>,
}

/// What a purge did for one sync.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PurgeOutcome {
    pub sync: String,
    /// The deleted events and the events that could not be deleted.
    pub result: Result<(usize, usize), String>,
}

impl PurgePreview {
    /// The syncs whose sink could be read; only these are purged.
    pub fn purgeable(&self) -> Vec<String> {
        self.counts
            .iter()
            .filter(|c| c.events.is_ok())
            .map(|c| c.sync.clone())
            .collect()
    }

    /// Per sync its count, or why it cannot be purged.
    pub fn lines(&self) -> Vec<(String, String)> {
        self.counts
            .iter()
            .map(|c| {
                let text = match &c.events {
                    Ok(1) => format!("1 event in calendar {}", c.target),
                    Ok(n) => format!("{n} events in calendar {}", c.target),
                    Err(e) => format!("cannot read calendar {}, skipped: {e}", c.target),
                };
                (c.sync.clone(), text)
            })
            .collect()
    }
}

pub struct Backend<F> {
    config_path: PathBuf,
    /// The config the coordinator runs.
    config: Option<Config>,
    lock: Option<ConfigLock>,
    coordinator: Coordinator,
    problem: Option<Problem>,
    activity: Option<String>,
    /// Applies `log_level` of each loaded config.
    log_level: LogLevel,
    keychain: Keychain,
    notify: Arc<F>,
}

impl<F: Fn(Update) + Send + Sync + 'static> Backend<F> {
    pub fn new(config_path: PathBuf, notify: F) -> Self {
        Self {
            config_path,
            config: None,
            lock: None,
            coordinator: Coordinator::new(),
            problem: None,
            activity: None,
            log_level: LogLevel::default(),
            keychain: Keychain::default(),
            notify: Arc::new(notify),
        }
    }

    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub fn with_log_level(self, log_level: LogLevel) -> Self {
        Self { log_level, ..self }
    }

    #[cfg(test)]
    pub fn with_keychain(self, keychain: Keychain) -> Self {
        Self { keychain, ..self }
    }

    /// Load the config and serve commands until shutdown or until the
    /// command channel closes. A change of the config file reloads it.
    pub async fn run(mut self, mut commands: mpsc::UnboundedReceiver<Command>) {
        let mut status = self.coordinator.status();
        let (_watcher, mut changes) = watch_config(&self.config_path).unzip();
        let Some(early) = self.explain_keychain(&mut commands).await else {
            return;
        };
        self.reload().await;
        self.publish();
        for command in early {
            if !self.handle(command).await {
                return;
            }
        }
        loop {
            tokio::select! {
                command = commands.recv() => match command {
                    Some(command) => if !self.handle(command).await {
                        return;
                    },
                    None => {
                        std::mem::take(&mut self.coordinator).shutdown().await;
                        return;
                    }
                },
                Some(()) = async {
                    match changes.as_mut() {
                        Some(changes) => changes.recv().await,
                        None => None,
                    }
                } => {
                    info!("config changed, reloading");
                    self.reload().await;
                }
                Ok(()) = status.changed() => tokio::time::sleep(STATUS_THROTTLE).await,
            }
            status.mark_unchanged();
            self.publish();
        }
    }

    /// Serve one command; false after a shutdown.
    async fn handle(&mut self, command: Command) -> bool {
        match command {
            Command::SyncNow => {
                self.coordinator.sync_now(None);
            }
            Command::FullSync(sync) => {
                info!(?sync, "full sync requested");
                self.coordinator.full_sync(sync.as_deref());
            }
            Command::Reload => self.reload().await,
            Command::EditConfig => self.edit_config(),
            Command::CountPurge => self.count_purge().await,
            Command::Purge(syncs) => self.purge(syncs).await,
            Command::CountCalendar(calendar) => self.count_calendar(calendar).await,
            Command::PurgeCalendar(calendar) => self.purge_calendar(calendar).await,
            Command::DiscoverAccount(account) => self.discover_account(account),
            Command::TestAccount {
                account,
                url,
                username,
                password,
            } => self.test_account(account, url, username, password),
            Command::ListCalendarApp => self.list_calendar_app(),
            Command::NextSourceEvent { calendar, sync } => self.next_source_event(calendar, sync),
            Command::SaveSettings(change) => self.save_settings(change).await,
            Command::KeychainNoticeClosed => {}
            Command::Shutdown(done) => {
                std::mem::take(&mut self.coordinator).shutdown().await;
                let _ = done.send(());
                return false;
            }
        }
        true
    }

    /// Before the first load reads the Keychain: when macOS would ask
    /// about an account's entry, have the app explain why and wait until
    /// its note is closed. Returns the commands that came meanwhile, None
    /// after a shutdown.
    async fn explain_keychain(
        &mut self,
        commands: &mut mpsc::UnboundedReceiver<Command>,
    ) -> Option<Vec<Command>> {
        let mut early = Vec::new();
        let Ok(config) = Config::load(&self.config_path) else {
            return Some(early);
        };
        let prompts = self.keychain.prompts;
        let accounts = tokio::task::spawn_blocking(move || {
            keychain_accounts(&config)
                .into_iter()
                .filter(|(_, entry)| prompts(entry))
                .map(|(account, _)| account)
                .collect::<Vec<_>>()
        })
        .await
        .unwrap_or_default();
        if accounts.is_empty() {
            return Some(early);
        }
        info!(?accounts, "macOS will ask for the Keychain");
        (self.notify)(Update::KeychainNotice(accounts));
        loop {
            match commands.recv().await? {
                Command::KeychainNoticeClosed => return Some(early),
                command @ Command::Shutdown(_) => {
                    self.handle(command).await;
                    return None;
                }
                command => early.push(command),
            }
        }
    }

    fn publish(&self) {
        (self.notify)(Update::Snapshot(Snapshot {
            loaded: true,
            problem: self.problem.clone(),
            activity: self.activity.clone(),
            syncs: self.coordinator.status().borrow().clone(),
        }));
    }

    async fn reload(&mut self) {
        self.problem = self.try_reload().await.err();
        (self.notify)(Update::ConfigChanged);
        if matches!(
            self.problem,
            Some(Problem::Config(_) | Problem::Locked { .. })
        ) {
            (self.notify)(Update::ReloadFailed);
        }
    }

    /// Load the config, lock it and (re)start its syncs. On failure the
    /// running syncs, if any, continue with the previous config.
    async fn try_reload(&mut self) -> Result<(), Problem> {
        if !self.config_path.exists() {
            warn!(config = %self.config_path.display(), "no config file");
            return Err(Problem::NoConfig);
        }
        let config = Config::load(&self.config_path).map_err(|e| {
            error!(error = %e, "invalid config");
            Problem::Config(e.to_string())
        })?;
        let locked = match self.lock.as_mut() {
            Some(lock) => lock.update(&config, &self.config_path),
            None => ConfigLock::acquire(&config, &self.config_path).map(|l| self.lock = Some(l)),
        };
        locked.map_err(|e| {
            error!(error = %e, "cannot lock config");
            lock_problem(e)
        })?;
        if let Err(e) = self.coordinator.reload(&config).await {
            error!(error = %e, "cannot start syncs");
            match &self.config {
                // Record the syncs that keep running again
                Some(running) => {
                    if let Some(lock) = self.lock.as_mut() {
                        let _ = lock.update(running, &self.config_path);
                    }
                }
                None => self.lock = None,
            }
            return Err(Problem::Config(e.to_string()));
        }
        info!(config = %self.config_path.display(), syncs = config.syncs.len(), "config loaded");
        self.log_level.set(&config.daemon.log_level);
        self.config = Some(config);
        Ok(())
    }

    fn clear_problem(&mut self, is_cleared: impl Fn(&Problem) -> bool) {
        if self.problem.as_ref().is_some_and(is_cleared) {
            self.problem = None;
        }
    }

    fn edit_config(&self) {
        let result = ensure_config_file(&self.config_path)
            .map(|_| ())
            .map_err(|e| format!("{}: {e}", self.config_path.display()));
        if let Err(e) = &result {
            error!(error = %e, "cannot write the starter config");
        }
        (self.notify)(Update::ConfigReady(result));
    }

    /// How to get the password of account `name`, without asking yet: the
    /// keychain or a command may block (prompt, Touch ID), so the caller
    /// runs it with `spawn_blocking`.
    fn password_source(&self, name: &str) -> Result<PasswordSource, String> {
        let config = self.config.as_ref().ok_or("The config is not loaded")?;
        let account = config
            .accounts
            .get(name)
            .ok_or_else(|| format!("Unknown account {name}"))?;
        Ok(PasswordSource {
            account: name.to_string(),
            url: account.url.clone(),
            username: account.username.clone(),
            credential: account.password.clone(),
            config_dir: config.config_dir.clone(),
            keychain: self.keychain,
        })
    }

    fn discover_account(&self, account: String) {
        let source = self.password_source(&account);
        let notify = self.notify.clone();
        tokio::spawn(async move {
            let result = match source {
                Ok(source) => {
                    let (url, username) = (source.url.clone(), source.username.clone());
                    match resolve_blocking(source).await {
                        Ok(password) => {
                            info!(account, "discovering calendars");
                            capycal::discover_caldav_calendars(&url, &username, &password)
                                .await
                                .map_err(|e| e.to_string())
                        }
                        Err(e) => Err(e),
                    }
                }
                Err(e) => Err(e),
            };
            if let Err(e) = &result {
                warn!(account, error = %e, "discovery failed");
            }
            notify(Update::AccountCalendars { account, result });
        });
    }

    fn test_account(
        &self,
        account: Option<String>,
        url: String,
        username: String,
        password: Option<String>,
    ) {
        let source = match (&password, &account) {
            (Some(_), _) => None,
            (None, Some(account)) => Some(self.password_source(account)),
            (None, None) => Some(Err("Enter the password".to_string())),
        };
        let notify = self.notify.clone();
        tokio::spawn(async move {
            let password = match (password, source) {
                (Some(p), _) => Ok(p),
                (None, Some(Ok(source))) => resolve_blocking(source).await,
                (None, Some(Err(e))) => Err(e),
                (None, None) => Err("Enter the password".to_string()),
            };
            let result = match password {
                Ok(password) => capycal::discover_caldav_calendars(&url, &username, &password)
                    .await
                    .map(|found| found.len())
                    .map_err(|e| e.to_string()),
                Err(e) => Err(e),
            };
            notify(Update::AccountTested(result));
        });
    }

    fn next_source_event(&self, calendar: capycal::CalendarConfig, sync: capycal::SyncConfig) {
        let notify = self.notify.clone();
        let Some(loaded) = &self.config else {
            notify(Update::NextSourceEvent {
                source: sync.source,
                result: Err("The config is not loaded".to_string()),
            });
            return;
        };
        // Only what reading the source needs: its account and calendar
        let config = capycal::Config {
            daemon: Default::default(),
            accounts: loaded.accounts.clone(),
            calendars: [(sync.source.clone(), calendar)].into(),
            syncs: Vec::new(),
            config_dir: loaded.config_dir.clone(),
        };
        tokio::spawn(async move {
            let result = capycal::next_source_event(&config, &sync, chrono::Utc::now())
                .await
                .map_err(|e| e.to_string());
            if let Err(e) = &result {
                warn!(calendar = sync.source, error = %e, "cannot read the next event");
            }
            notify(Update::NextSourceEvent {
                source: sync.source,
                result,
            });
        });
    }

    fn list_calendar_app(&self) {
        let notify = self.notify.clone();
        tokio::task::spawn_blocking(move || {
            notify(Update::CalendarAppCalendars(
                capycal::list_eventkit_calendars().map_err(|e| e.to_string()),
            ));
        });
    }

    async fn save_settings(&mut self, change: SettingsChange) {
        let mut purged = Vec::new();
        let result = self.apply(&change, &mut purged).await;
        if let Err(e) = &result {
            error!(error = %e, "cannot save the settings");
            // The config did not change: the purged syncs run on
            if let Some(config) = &self.config {
                for name in &purged {
                    if let Err(e) = self.coordinator.resume(config, name) {
                        warn!(sync = name, error = %e, "cannot resume the sync");
                    }
                }
            }
        }
        (self.notify)(Update::SettingsSaved(result));
    }

    /// Purge, copy keychain entries, write the config, then clean up and
    /// reload. A failure before the config is written undoes the copies;
    /// `purged` collects the syncs whose events were removed.
    async fn apply(
        &mut self,
        change: &SettingsChange,
        purged: &mut Vec<String>,
    ) -> Result<(), String> {
        for name in &change.purge_first {
            if self.config.is_none() {
                return Err(
                    "The config is not running here (invalid, or another Capycal runs it); \
                            see Problems"
                        .to_string(),
                );
            }
            self.activity = Some(format!("Removing the events of {name}"));
            self.publish();
            let config = self.config.as_ref().expect("checked above");
            // Counted first, so Settings can show how far it is
            let total = capycal::count_synced_events(config, name).await.ok();
            (self.notify)(Update::PurgeProgress {
                sync: name.clone(),
                done: 0,
                total,
            });
            let progress = self.purge_progress(name, total);
            let outcome = self.coordinator.purge(config, name, progress).await;
            self.activity = None;
            self.publish();
            purged.push(name.clone());
            match outcome {
                Ok((_, 0)) => {}
                Ok((_, errors)) => {
                    return Err(format!(
                        "{errors} events of {name} could not be removed; nothing was changed. \
                         Try again, or see the log."
                    ));
                }
                Err(e) => return Err(format!("Cannot remove the events of {name}: {e}")),
            }
        }
        let keychain = self.keychain;
        let ops = change.keychain.clone();
        let copies = tokio::task::spawn_blocking(move || copy_keychain(keychain, &ops))
            .await
            .map_err(|e| e.to_string())??;
        let undo = |copies: Vec<String>| async move {
            let _ = tokio::task::spawn_blocking(move || {
                for name in copies {
                    let _ = (keychain.delete)(&name);
                }
            })
            .await;
        };
        let saved = ConfigEditor::load(&self.config_path).and_then(|mut editor| {
            for edit in &change.edits {
                apply_edit(&mut editor, edit)?;
            }
            editor.save()
        });
        let config = match saved {
            Ok(config) => config,
            Err(e) => {
                undo(copies).await;
                let stored = change
                    .keychain
                    .iter()
                    .any(|op| matches!(op, KeychainOp::Set { .. }));
                return Err(if stored {
                    format!("{e} (the new password is already in the keychain)")
                } else {
                    e.to_string()
                });
            }
        };
        let ops = change.keychain.clone();
        let _ = tokio::task::spawn_blocking(move || {
            for op in &ops {
                let name = match op {
                    KeychainOp::Delete { name } => name,
                    KeychainOp::Rename { old, .. } => old,
                    KeychainOp::Set { .. } => continue,
                };
                if let Err(e) = (keychain.delete)(name) {
                    warn!(entry = name, error = %e, "cannot remove a keychain entry");
                }
            }
        })
        .await;
        info!("settings saved");
        self.reload().await;
        // After the reload stopped their runners, which could save it again
        for name in &change.forget_state {
            if let Err(e) = capycal::forget_sync_state(&config, name) {
                warn!(sync = name, error = %e, "cannot forget the sync state");
            }
        }
        if let Some(config) = &self.config {
            for name in &change.resume {
                if let Err(e) = self.coordinator.resume(config, name) {
                    warn!(sync = name, error = %e, "cannot resume the sync");
                }
            }
        }
        Ok(())
    }

    async fn count_purge(&mut self) {
        if self.config.is_none() {
            return;
        }
        self.activity = Some("Counting synced events".to_string());
        self.publish();
        let mut counts = Vec::new();
        if let Some(config) = &self.config {
            for s in &config.syncs {
                let events = capycal::count_synced_events(config, &s.name)
                    .await
                    .map_err(|e| {
                        error!(sync = %s.name, error = %e, "cannot count the synced events");
                        e.to_string()
                    });
                counts.push(PurgeCount {
                    sync: s.name.clone(),
                    target: s.target.clone(),
                    events,
                });
            }
        }
        self.activity = None;
        (self.notify)(Update::PurgePreview(PurgePreview { counts }));
    }

    async fn purge(&mut self, syncs: Vec<String>) {
        let Some(config) = &self.config else {
            return;
        };
        let mut failures = Vec::new();
        let mut outcomes = Vec::new();
        for name in &syncs {
            self.activity = Some(format!("Removing the events of {name}"));
            self.publish();
            let result = self
                .coordinator
                .purge(config, name, self.purge_progress(name, None))
                .await;
            match &result {
                Ok((deleted, 0)) => {
                    info!(sync = %name, deleted, "synced events removed")
                }
                Ok((deleted, errors)) => {
                    warn!(sync = %name, deleted, errors, "some synced events were not removed");
                    failures.push(format!("{name}: {errors} events not removed"));
                }
                Err(e) => {
                    error!(sync = %name, error = %e, "cannot remove the synced events");
                    failures.push(format!("{name}: {e}"));
                }
            }
            outcomes.push(PurgeOutcome {
                sync: name.clone(),
                result: result.map_err(|e| e.to_string()),
            });
        }
        self.activity = None;
        // They stay stopped, also after a restart, until enabled again
        let purged: Vec<String> = outcomes
            .iter()
            .filter(|o| o.result.is_ok())
            .map(|o| o.sync.clone())
            .collect();
        if !purged.is_empty()
            && let Err(e) = self.disable(&purged).await
        {
            // Paused until restart, then they would copy everything again
            failures.push(format!("{} not disabled: {e}", purged.join(", ")));
        }
        if failures.is_empty() {
            self.clear_problem(|p| matches!(p, Problem::Purge(_)));
        } else {
            self.problem = Some(Problem::Purge(failures.join("; ")));
        }
        (self.notify)(Update::Purged(outcomes));
    }

    async fn count_calendar(&mut self, calendar: String) {
        let Some(config) = &self.config else {
            return;
        };
        let events = capycal::count_calendar_events(config, &calendar)
            .await
            .map_err(|e| {
                error!(calendar = %calendar, error = %e, "cannot count the events");
                e.to_string()
            });
        (self.notify)(Update::CalendarCounted { calendar, events });
    }

    async fn purge_calendar(&mut self, calendar: String) {
        let Some(config) = &self.config else {
            return;
        };
        let writing: Vec<String> = (config.syncs.iter())
            .filter(|s| s.target == calendar)
            .map(|s| s.name.clone())
            .collect();
        self.activity = Some(format!("Removing all events of {calendar}"));
        self.publish();
        let progress = {
            let calendar = calendar.clone();
            self.throttled(move |done| Update::CalendarPurgeProgress {
                calendar: calendar.clone(),
                done,
            })
        };
        let result = self
            .coordinator
            .purge_calendar(config, &calendar, progress)
            .await;
        self.activity = None;
        let mut failure = match &result {
            Ok((deleted, 0)) => {
                info!(calendar = %calendar, deleted, "all events removed");
                None
            }
            Ok((deleted, errors)) => {
                warn!(calendar = %calendar, deleted, errors, "some events were not removed");
                Some(format!("{calendar}: {errors} events not removed"))
            }
            Err(e) => {
                error!(calendar = %calendar, error = %e, "cannot remove the events");
                Some(format!("{calendar}: {e}"))
            }
        };
        // Like after removing synced events: the syncs stay off until
        // switched on again, or they would copy everything back
        if result.is_ok()
            && !writing.is_empty()
            && let Err(e) = self.disable(&writing).await
        {
            failure = Some(format!("{} not disabled: {e}", writing.join(", ")));
        }
        match failure {
            None => self.clear_problem(|p| matches!(p, Problem::Purge(_))),
            Some(f) => self.problem = Some(Problem::Purge(f)),
        }
        (self.notify)(Update::CalendarPurged {
            calendar,
            result: result.map_err(|e| e.to_string()),
        });
    }

    /// Reports the progress of a purge at most every 100 ms; `Purged`
    /// follows anyway.
    fn purge_progress(&self, sync: &str, total: Option<usize>) -> Arc<dyn Fn(usize) + Send + Sync> {
        let sync = sync.to_string();
        self.throttled(move |done| Update::PurgeProgress {
            sync: sync.clone(),
            done,
            total,
        })
    }

    /// A progress callback that sends `update(done)` at most every 100 ms.
    fn throttled(
        &self,
        update: impl Fn(usize) -> Update + Send + Sync + 'static,
    ) -> Arc<dyn Fn(usize) + Send + Sync> {
        let notify = self.notify.clone();
        let last = std::sync::Mutex::new(None::<std::time::Instant>);
        Arc::new(move |done| {
            let mut last = last.lock().unwrap_or_else(|e| e.into_inner());
            if last.is_some_and(|t| t.elapsed() < std::time::Duration::from_millis(100)) {
                return;
            }
            *last = Some(std::time::Instant::now());
            notify(update(done));
        })
    }

    /// Write `enabled = false` for `syncs` in one save and reload.
    async fn disable(&mut self, syncs: &[String]) -> Result<(), String> {
        let written = ConfigEditor::load(&self.config_path).and_then(|mut editor| {
            for name in syncs {
                editor.set_enabled(name, false)?;
            }
            editor.save()
        });
        match written {
            Ok(_) => {
                info!(syncs = ?syncs, "syncs disabled");
                self.reload().await;
                Ok(())
            }
            Err(e) => {
                error!(syncs = ?syncs, error = %e, "cannot disable the syncs");
                Err(e.to_string())
            }
        }
    }
}

/// The other desktop app (capycal-ui or capycal-app) that already runs the
/// config at `path`, if any. A second app would only report the lock; the
/// CLI holding it is a state the app shows instead.
/// The accounts of the enabled syncs whose password is in the Keychain, by
/// name, with their entry.
fn keychain_accounts(config: &Config) -> Vec<(String, String)> {
    let used: std::collections::BTreeSet<&str> = config
        .syncs
        .iter()
        .filter(|s| s.enabled)
        .flat_map(|s| [&s.source, &s.target])
        .filter_map(|c| config.calendars.get(c)?.account.as_deref())
        .collect();
    used.into_iter()
        .filter_map(|name| match &config.accounts.get(name)?.password {
            Credential::Keychain { keychain } => Some((name.to_string(), keychain.clone())),
            _ => None,
        })
        .collect()
}

/// What reading an account's password needs, off the backend's thread.
struct PasswordSource {
    account: String,
    url: String,
    username: String,
    credential: Credential,
    config_dir: Option<PathBuf>,
    keychain: Keychain,
}

/// The password of an account: its keychain entry through the backend's
/// `Keychain`, any other credential as the daemon resolves it.
async fn resolve_blocking(source: PasswordSource) -> Result<String, String> {
    tokio::task::spawn_blocking(move || match &source.credential {
        Credential::Keychain { keychain } => (source.keychain.password)(keychain),
        credential => credential
            .resolve(
                &format!("account '{}'", source.account),
                source.config_dir.as_deref(),
            )
            .map_err(|e| e.to_string()),
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Write the keychain entries a change sets and copy renamed ones (their
/// old entry goes after the config is saved). Returns the new names of the
/// copies; a failure removes the copies made so far.
fn copy_keychain(keychain: Keychain, ops: &[KeychainOp]) -> Result<Vec<String>, String> {
    let mut copies = Vec::new();
    for op in ops {
        let done = match op {
            KeychainOp::Set { name, password } => (keychain.set)(name, password),
            KeychainOp::Rename { old, new } => (keychain.password)(old)
                .map_err(|e| {
                    format!("cannot move the password of {old} ({e}); type the password to store it again")
                })
                .and_then(|password| (keychain.set)(new, &password))
                .map(|()| copies.push(new.clone())),
            KeychainOp::Delete { .. } => Ok(()),
        };
        if let Err(e) = done {
            for name in &copies {
                let _ = (keychain.delete)(name);
            }
            return Err(format!("Keychain: {e}"));
        }
    }
    Ok(copies)
}

fn apply_edit(editor: &mut ConfigEditor, edit: &Edit) -> capycal::Result<()> {
    match edit {
        Edit::SetAccount(name, account) => editor.set_account(name, account),
        Edit::RenameAccount(old, new) => editor.rename_account(old, new)?,
        Edit::RemoveAccount(name) => editor.remove_account(name),
        Edit::SetCalendar(name, calendar) => editor.set_calendar(name, calendar),
        Edit::RemoveCalendar(name) => editor.remove_calendar(name),
        Edit::SetSync(sync) => editor.set_sync(sync),
        Edit::SetLogLevel(level) => editor.set_log_level(level),
        Edit::RenameSync(old, new) => editor.rename_sync(old, new)?,
        Edit::RemoveSync(name) => editor.remove_sync(name),
    }
    Ok(())
}

pub fn other_desktop_app(path: &Path) -> Option<capycal::LockHolder> {
    let config = Config::load(path).ok()?;
    capycal::lock_holder(&config, path)
        .ok()
        .flatten()
        .filter(|holder| holder.is_desktop_app() && holder.pid != std::process::id().to_string())
}

/// Watch the config file. Its directory is created first, so a config that
/// does not exist yet is picked up once written.
fn watch_config(path: &Path) -> Option<(ConfigWatcher, mpsc::Receiver<()>)> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty())
        && let Err(e) = std::fs::create_dir_all(dir)
    {
        warn!(dir = %dir.display(), error = %e, "cannot create the config directory");
    }
    ConfigWatcher::new(path)
        .inspect_err(|e| warn!(error = %e, "config changes are not picked up automatically"))
        .ok()
}

fn lock_problem(e: Error) -> Problem {
    match e {
        Error::Locked { pid, .. } => Problem::Locked { pid },
        e => Problem::Config(e.to_string()),
    }
}

/// Write the starter config to `path` unless a file with settings exists
/// there. A file of only blank and comment lines gets the starter config
/// put in front, unless it has it already. Returns whether it was written.
pub fn ensure_config_file(path: &Path) -> std::io::Result<bool> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(mut file) => file.write_all(STARTER_CONFIG.as_bytes()).map(|()| true),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let content = std::fs::read_to_string(path)?;
            let blank = content.lines().all(|l| {
                let l = l.trim();
                l.is_empty() || l.starts_with('#')
            });
            let header = STARTER_CONFIG.lines().next().unwrap_or_default();
            if !blank || content.lines().any(|l| l == header) {
                return Ok(false);
            }
            let rest = content.trim_start_matches('\n');
            let separator = if rest.is_empty() { "" } else { "\n" };
            std::fs::write(path, format!("{STARTER_CONFIG}{separator}{rest}")).map(|()| true)
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use wiremock::MockServer;

    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("capycal-{}-ui-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A config whose only sync polls `server` once an hour. The server
    /// answers 404, so every cycle fails fast.
    fn write_config(dir: &Path, sync: &str, server: &MockServer) -> PathBuf {
        let path = dir.join("capycal.toml");
        std::fs::write(
            &path,
            format!(
                r#"
[daemon]
state_dir = "{}"

[accounts.acc]
url = "https://caldav.example.com/"
username = "u"
password = "p"

[calendars.a]
url = "{uri}/a/"
account = "acc"

[calendars.b]
url = "{uri}/b/"
account = "acc"

[syncs."{sync}"]
source = "a"
target = "b"
interval = "1h"
fields = ["DTSTART"]
"#,
                dir.join("state").display(),
                uri = server.uri(),
            ),
        )
        .unwrap();
        path
    }

    /// Run a backend on `path`, send `commands`, then shut down. Returns the
    /// snapshots it published.
    async fn run_backend(path: PathBuf, commands: Vec<Command>) -> Vec<Snapshot> {
        run_backend_updates(path, commands)
            .await
            .into_iter()
            .filter_map(|u| match u {
                Update::Snapshot(s) => Some(s),
                _ => None,
            })
            .collect()
    }

    async fn run_backend_updates(path: PathBuf, commands: Vec<Command>) -> Vec<Update> {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        let backend = Backend::new(path, move |u| sink.lock().unwrap().push(u));
        let (tx, rx) = mpsc::unbounded_channel();
        for command in commands {
            tx.send(command).unwrap();
        }
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        tx.send(Command::Shutdown(done_tx)).unwrap();
        backend.run(rx).await;
        done_rx.recv().unwrap();
        Arc::try_unwrap(seen).unwrap().into_inner().unwrap()
    }

    #[tokio::test]
    async fn test_missing_config_is_reported() {
        let dir = test_dir("missing");
        let updates = run_backend_updates(dir.join("capycal.toml"), vec![]).await;
        assert!(!updates.contains(&Update::ReloadFailed));
        let snapshots = run_backend(dir.join("capycal.toml"), vec![]).await;
        assert_eq!(snapshots[0].problem, Some(Problem::NoConfig));
        assert!(snapshots[0].loaded);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_invalid_config_is_reported() {
        let dir = test_dir("invalid");
        let path = dir.join("capycal.toml");
        std::fs::write(&path, "not toml [").unwrap();
        let updates = run_backend_updates(path.clone(), vec![]).await;
        assert!(updates.contains(&Update::ReloadFailed));
        let snapshots = run_backend(path, vec![]).await;
        assert!(
            matches!(&snapshots[0].problem, Some(Problem::Config(e)) if e.contains("parse")),
            "{:?}",
            snapshots[0].problem
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_locked_config_is_reported() {
        let dir = test_dir("locked");
        let server = MockServer::start().await;
        let path = write_config(&dir, "a-to-b", &server);
        let _other = ConfigLock::acquire(&Config::load(&path).unwrap(), &path).unwrap();
        let snapshots = run_backend(path, vec![]).await;
        let pid = std::process::id().to_string();
        assert_eq!(snapshots[0].problem, Some(Problem::Locked { pid }));
        assert!(snapshots[0].syncs.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_commands_reach_the_running_config() {
        let dir = test_dir("commands");
        let path = dir.join("capycal.toml");
        let (snap_tx, mut snapshots) = mpsc::unbounded_channel();
        let backend = Backend::new(path.clone(), move |u| {
            if let Update::Snapshot(s) = u {
                let _ = snap_tx.send(s);
            }
        });
        let (tx, rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(backend.run(rx));
        let mut next = async || snapshots.recv().await.unwrap();
        assert_eq!(next().await.problem, Some(Problem::NoConfig));

        let server = MockServer::start().await;
        write_config(&dir, "a-to-b", &server);
        tx.send(Command::Reload).unwrap();
        let snapshot = next().await;
        assert_eq!(snapshot.problem, None);
        assert_eq!(snapshot.syncs[0].name, "a-to-b");
        tx.send(Command::SyncNow).unwrap();
        tx.send(Command::FullSync(None)).unwrap();
        drop(tx);
        task.await.unwrap();
        while let Ok(snapshot) = snapshots.try_recv() {
            assert_eq!(snapshot.problem, None);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_reload_keeps_the_lock_and_records_new_syncs() {
        let dir = test_dir("reload");
        let server = MockServer::start().await;
        let path = write_config(&dir, "first", &server);
        let mut backend = Backend::new(path.clone(), |_| {});

        backend.reload().await;
        assert_eq!(backend.problem, None);
        write_config(&dir, "second", &server);
        backend.reload().await;
        assert_eq!(backend.problem, None);
        let lock_dir = dir.join("state/locks");
        let lock_file = std::fs::read_dir(&lock_dir)
            .unwrap()
            .next()
            .unwrap()
            .unwrap();
        let content = std::fs::read_to_string(lock_file.path()).unwrap();
        assert!(content.contains("sync=second") && !content.contains("sync=first"));
        // Another process still cannot take the config
        assert!(ConfigLock::acquire(&Config::load(&path).unwrap(), &path).is_err());

        // A broken config leaves the running syncs alone
        std::fs::write(&path, "not toml [").unwrap();
        backend.reload().await;
        assert!(matches!(backend.problem, Some(Problem::Config(_))));
        assert_eq!(backend.config.as_ref().unwrap().syncs[0].name, "second");

        std::mem::take(&mut backend.coordinator).shutdown().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_config_change_reloads() {
        let dir = test_dir("watch");
        let path = dir.join("conf/capycal.toml");
        let (snap_tx, mut snapshots) = mpsc::unbounded_channel();
        let backend = Backend::new(path.clone(), move |u| {
            if let Update::Snapshot(s) = u {
                let _ = snap_tx.send(s);
            }
        });
        let (tx, rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(backend.run(rx));
        let mut next = async || {
            tokio::time::timeout(Duration::from_secs(5), snapshots.recv())
                .await
                .expect("no snapshot")
                .unwrap()
        };
        assert_eq!(next().await.problem, Some(Problem::NoConfig));

        // The missing directory was created, so a new config is seen
        let server = MockServer::start().await;
        write_config(&dir.join("conf"), "a-to-b", &server);
        let snapshot = loop {
            let snapshot = next().await;
            if snapshot.problem.is_none() {
                break snapshot;
            }
        };
        assert_eq!(snapshot.syncs[0].name, "a-to-b");

        std::fs::write(&path, "not toml [").unwrap();
        while !matches!(next().await.problem, Some(Problem::Config(_))) {}
        drop(tx);
        task.await.unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_config_without_syncs_is_not_a_problem() {
        let dir = test_dir("unconfigured");
        let path = dir.join("capycal.toml");
        let toml = format!(
            "[daemon]\nstate_dir = \"{}\"\n",
            dir.join("state").display()
        );
        std::fs::write(&path, toml).unwrap();
        let snapshots = run_backend(path, vec![Command::SyncNow, Command::FullSync(None)]).await;
        for snapshot in snapshots {
            assert_eq!(snapshot.problem, None);
            assert!(snapshot.syncs.is_empty());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_count_purge_disable_and_enable() {
        use wiremock::matchers::{body_string_contains, method, path, path_regex};
        use wiremock::{Mock, ResponseTemplate};

        let dir = test_dir("purge");
        // The sink holds one event of this sync and one of another
        let server = MockServer::start().await;
        let multistatus = |inner: &str| {
            ResponseTemplate::new(207).set_body_string(format!(
                r#"<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">{inner}</d:multistatus>"#
            ))
        };
        let event = |href: &str, sync: &str| {
            format!(
                "<d:response><d:href>{href}</d:href><d:propstat><d:prop><d:getetag>\"e\"</d:getetag>\
                 <c:calendar-data>BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:x\nX-CAPYCAL-SYNC:{sync}\n\
                 END:VEVENT\nEND:VCALENDAR</c:calendar-data></d:prop></d:propstat></d:response>"
            )
        };
        Mock::given(method("PROPFIND"))
            .and(path_regex("^/b/?$"))
            .respond_with(multistatus(
                &(event("/b/1.ics", "a-to-b") + &event("/b/2.ics", "other")),
            ))
            .mount(&server)
            .await;
        Mock::given(method("REPORT"))
            .and(body_string_contains("calendar-multiget"))
            .respond_with(multistatus(
                &(event("/b/1.ics", "a-to-b") + &event("/b/2.ics", "other")),
            ))
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/b/1.ics"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        let path = write_config(&dir, "a-to-b", &server);
        let (tx_updates, mut updates) = mpsc::unbounded_channel();
        let backend = Backend::new(path.clone(), move |u| {
            let _ = tx_updates.send(u);
        });
        let (tx, rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(backend.run(rx));
        let mut next_matching = async |wanted: &dyn Fn(&Update) -> bool| loop {
            let update = tokio::time::timeout(Duration::from_secs(5), updates.recv())
                .await
                .expect("no update")
                .unwrap();
            if wanted(&update) {
                return update;
            }
        };
        let disabled = |u: &Update| matches!(u, Update::Snapshot(s) if s.syncs.first().is_some_and(|s| s.disabled));

        tx.send(Command::CountPurge).unwrap();
        let Update::PurgePreview(preview) =
            next_matching(&|u| matches!(u, Update::PurgePreview(_))).await
        else {
            unreachable!()
        };
        assert_eq!(preview.purgeable(), ["a-to-b"]);
        assert_eq!(
            preview.lines(),
            [("a-to-b".to_string(), "1 event in calendar b".to_string())]
        );

        tx.send(Command::Purge(preview.purgeable())).unwrap();
        let purged = next_matching(&|u| matches!(u, Update::Purged(_))).await;
        let outcome = PurgeOutcome {
            sync: "a-to-b".to_string(),
            result: Ok((1, 0)),
        };
        assert_eq!(purged, Update::Purged(vec![outcome]));
        let Update::Snapshot(snapshot) = next_matching(&disabled).await else {
            unreachable!()
        };
        assert_eq!(snapshot.problem, None);
        assert_eq!(snapshot.activity, None);

        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("enabled = false")
        );
        drop(tx);
        task.await.unwrap();
        server.verify().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_count_and_purge_a_calendar_disables_its_syncs() {
        use wiremock::matchers::{method, path, path_regex};
        use wiremock::{Mock, ResponseTemplate};

        let dir = test_dir("purge-calendar");
        // The target holds a copy and an event capycal did not create
        let server = MockServer::start().await;
        let reference = |href: &str| {
            format!(
                "<d:response><d:href>{href}</d:href><d:propstat><d:prop>\
                 <d:getetag>\"e\"</d:getetag></d:prop></d:propstat></d:response>"
            )
        };
        Mock::given(method("PROPFIND"))
            .and(path_regex("^/b/?$"))
            .respond_with(ResponseTemplate::new(207).set_body_string(format!(
                r#"<d:multistatus xmlns:d="DAV:">{}{}</d:multistatus>"#,
                reference("/b/1.ics"),
                reference("/b/2.ics")
            )))
            .mount(&server)
            .await;
        for href in ["/b/1.ics", "/b/2.ics"] {
            Mock::given(method("DELETE"))
                .and(path(href))
                .respond_with(ResponseTemplate::new(204))
                .expect(1)
                .mount(&server)
                .await;
        }
        let path = write_config(&dir, "a-to-b", &server);
        let (tx_updates, mut updates) = mpsc::unbounded_channel();
        let backend = Backend::new(path.clone(), move |u| {
            let _ = tx_updates.send(u);
        });
        let (tx, rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(backend.run(rx));
        let mut next_matching = async |wanted: &dyn Fn(&Update) -> bool| loop {
            let update = tokio::time::timeout(Duration::from_secs(5), updates.recv())
                .await
                .expect("no update")
                .unwrap();
            if wanted(&update) {
                return update;
            }
        };

        tx.send(Command::CountCalendar("b".into())).unwrap();
        let counted = next_matching(&|u| matches!(u, Update::CalendarCounted { .. })).await;
        assert_eq!(
            counted,
            Update::CalendarCounted {
                calendar: "b".into(),
                events: Ok(2)
            }
        );
        tx.send(Command::PurgeCalendar("b".into())).unwrap();
        let purged = next_matching(&|u| matches!(u, Update::CalendarPurged { .. })).await;
        assert_eq!(
            purged,
            Update::CalendarPurged {
                calendar: "b".into(),
                result: Ok((2, 0))
            }
        );
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("enabled = false")
        );
        drop(tx);
        task.await.unwrap();
        server.verify().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_other_desktop_app_ignores_this_process_and_missing_configs() {
        let dir = test_dir("other-app");
        let path = dir.join("capycal.toml");
        assert_eq!(other_desktop_app(&path), None);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            &path,
            format!("[daemon]\nstate_dir = \"{}\"\n", dir.display()),
        )
        .unwrap();
        let _lock = ConfigLock::acquire(&Config::load(&path).unwrap(), &path).unwrap();
        // Held by this test binary, which is no desktop app
        assert_eq!(other_desktop_app(&path), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_purge_preview_texts() {
        let preview = PurgePreview {
            counts: vec![
                PurgeCount {
                    sync: "a".to_string(),
                    target: "home".to_string(),
                    events: Ok(3),
                },
                PurgeCount {
                    sync: "b".to_string(),
                    target: "work".to_string(),
                    events: Err("offline".to_string()),
                },
            ],
        };
        assert_eq!(preview.purgeable(), ["a"]);
        let line = |sync: &str, text: &str| (sync.to_string(), text.to_string());
        assert_eq!(
            preview.lines(),
            [
                line("a", "3 events in calendar home"),
                line("b", "cannot read calendar work, skipped: offline")
            ]
        );
    }

    #[test]
    fn test_ensure_config_file_writes_starter_once() {
        let dir = test_dir("ensure");
        let path = dir.join("nested/capycal.toml");
        assert!(ensure_config_file(&path).unwrap());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), STARTER_CONFIG);
        std::fs::write(&path, "mine").unwrap();
        assert!(!ensure_config_file(&path).unwrap());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "mine");

        // An empty file, or one of only comments, gets the starter config
        std::fs::write(&path, "").unwrap();
        assert!(ensure_config_file(&path).unwrap());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), STARTER_CONFIG);
        std::fs::write(&path, "\n# [calendars.x]\n").unwrap();
        assert!(ensure_config_file(&path).unwrap());
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, format!("{STARTER_CONFIG}\n# [calendars.x]\n"));
        assert!(!ensure_config_file(&path).unwrap());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), content);
        let _ = std::fs::remove_dir_all(&dir);
    }

    static TEST_KEYCHAIN: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

    fn test_keychain() -> Keychain {
        Keychain {
            password: |name| {
                TEST_KEYCHAIN
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|(n, _)| n == name)
                    .map(|(_, p)| p.clone())
                    .ok_or_else(|| format!("{name} not found"))
            },
            set: |name, password| {
                let mut kc = TEST_KEYCHAIN.lock().unwrap();
                kc.retain(|(n, _)| n != name);
                kc.push((name.to_string(), password.to_string()));
                Ok(())
            },
            delete: |name| {
                TEST_KEYCHAIN.lock().unwrap().retain(|(n, _)| n != name);
                Ok(())
            },
            prompts: |_| false,
        }
    }

    fn in_keychain(name: &str) -> bool {
        TEST_KEYCHAIN.lock().unwrap().iter().any(|(n, _)| n == name)
    }

    /// A backend running on `path` with the test keychain.
    struct Running {
        tx: mpsc::UnboundedSender<Command>,
        updates: mpsc::UnboundedReceiver<Update>,
        task: tokio::task::JoinHandle<()>,
    }

    impl Running {
        fn start(path: PathBuf) -> Self {
            let (tx_updates, updates) = mpsc::unbounded_channel();
            let backend = Backend::new(path, move |u| {
                let _ = tx_updates.send(u);
            })
            .with_keychain(test_keychain());
            let (tx, rx) = mpsc::unbounded_channel();
            let task = tokio::spawn(backend.run(rx));
            Self { tx, updates, task }
        }

        async fn next(&mut self, wanted: impl Fn(&Update) -> bool) -> Update {
            loop {
                let update = tokio::time::timeout(Duration::from_secs(5), self.updates.recv())
                    .await
                    .expect("no update")
                    .unwrap();
                if wanted(&update) {
                    return update;
                }
            }
        }

        async fn save(&mut self, change: SettingsChange) -> Result<(), String> {
            self.tx.send(Command::SaveSettings(change)).unwrap();
            let Update::SettingsSaved(result) =
                self.next(|u| matches!(u, Update::SettingsSaved(_))).await
            else {
                unreachable!()
            };
            result
        }

        /// Wait for a snapshot whose syncs are `names`, none paused.
        async fn running(&mut self, names: &[&str]) {
            let names: Vec<String> = names.iter().map(|n| n.to_string()).collect();
            self.next(|u| {
                matches!(u, Update::Snapshot(s)
                    if s.syncs.iter().map(|s| s.name.clone()).collect::<Vec<_>>() == names
                        && s.syncs.iter().all(|s| !s.paused))
            })
            .await;
        }

        async fn stop(self) {
            drop(self.tx);
            self.task.await.unwrap();
        }
    }

    fn sync(name: &str, target: &str, interval: u64) -> capycal::SyncConfig {
        let mut s = capycal::SyncConfig::new(name, "a", target);
        s.interval = Duration::from_secs(interval);
        s.fields = vec!["DTSTART".to_string()];
        s
    }

    /// The sink `b` holds one event of `a-to-b`, which a purge deletes.
    async fn sink_with_one_copy() -> MockServer {
        use wiremock::matchers::{body_string_contains, method, path, path_regex};
        use wiremock::{Mock, ResponseTemplate};
        let server = MockServer::start().await;
        let body = "<d:multistatus xmlns:d=\"DAV:\" xmlns:c=\"urn:ietf:params:xml:ns:caldav\">\
            <d:response><d:href>/b/1.ics</d:href><d:propstat><d:prop><d:getetag>\"e\"</d:getetag>\
            <c:calendar-data>BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:x\nX-CAPYCAL-SYNC:a-to-b\n\
            END:VEVENT\nEND:VCALENDAR</c:calendar-data></d:prop></d:propstat></d:response>\
            </d:multistatus>";
        Mock::given(method("PROPFIND"))
            .and(path_regex("^/b/?$"))
            .respond_with(ResponseTemplate::new(207).set_body_string(body))
            .mount(&server)
            .await;
        Mock::given(method("REPORT"))
            .and(body_string_contains("calendar-multiget"))
            .respond_with(ResponseTemplate::new(207).set_body_string(body))
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/b/1.ics"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        server
    }

    #[tokio::test]
    async fn test_syncs_wait_for_the_keychain_notice() {
        let server = MockServer::start().await;
        let dir = test_dir("keychain-notice");
        let path = write_config(&dir, "a-to-b", &server);
        let toml = std::fs::read_to_string(&path).unwrap().replace(
            "password = \"p\"",
            "password = { keychain = \"kc-notice\" }",
        );
        std::fs::write(&path, &toml).unwrap();
        let (tx_updates, updates) = mpsc::unbounded_channel();
        let backend = Backend::new(path.clone(), move |u| {
            let _ = tx_updates.send(u);
        })
        .with_keychain(Keychain {
            prompts: |name| name == "kc-notice",
            ..test_keychain()
        });
        let (tx, rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(backend.run(rx));
        let mut backend = Running { tx, updates, task };
        assert_eq!(
            backend.next(|_| true).await,
            Update::KeychainNotice(vec!["acc".to_string()])
        );
        // Nothing loads while the note is open
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(backend.updates.try_recv().is_err());
        // The load reads the config again: without the Keychain entry the
        // test never reaches the real store
        std::fs::write(&path, toml.replace("{ keychain = \"kc-notice\" }", "\"p\"")).unwrap();
        backend.tx.send(Command::KeychainNoticeClosed).unwrap();
        backend.running(&["a-to-b"]).await;
        backend.stop().await;
    }

    #[tokio::test]
    async fn test_shutdown_while_the_keychain_notice_is_open() {
        let server = MockServer::start().await;
        let dir = test_dir("keychain-notice-quit");
        let path = write_config(&dir, "a-to-b", &server);
        let toml = std::fs::read_to_string(&path).unwrap().replace(
            "password = \"p\"",
            "password = { keychain = \"kc-notice-quit\" }",
        );
        std::fs::write(&path, toml).unwrap();
        let (tx_updates, mut updates) = mpsc::unbounded_channel();
        let backend = Backend::new(path, move |u| {
            let _ = tx_updates.send(u);
        })
        .with_keychain(Keychain {
            prompts: |_| true,
            ..test_keychain()
        });
        let (tx, rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(backend.run(rx));
        assert!(matches!(
            updates.recv().await,
            Some(Update::KeychainNotice(_))
        ));
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        tx.send(Command::Shutdown(done_tx)).unwrap();
        task.await.unwrap();
        done_rx.recv().unwrap();
        // The config was never loaded
        assert!(updates.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_save_settings_writes_the_config_and_reloads() {
        let server = MockServer::start().await;
        let dir = test_dir("save-settings");
        let path = write_config(&dir, "a-to-b", &server);
        let mut backend = Running::start(path.clone());
        backend.running(&["a-to-b"]).await;
        let change = SettingsChange {
            edits: vec![Edit::SetSync(sync("a-to-b", "b", 3600 * 2))],
            ..SettingsChange::default()
        };
        assert_eq!(backend.save(change).await, Ok(()));
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("interval = \"2h\"")
        );
        backend.running(&["a-to-b"]).await;
        backend.stop().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_failed_save_leaves_file_and_keychain_alone() {
        let server = MockServer::start().await;
        let dir = test_dir("failed-save");
        let path = write_config(&dir, "a-to-b", &server);
        let before = std::fs::read_to_string(&path).unwrap();
        (test_keychain().set)("kc-old", "pw").unwrap();
        let mut backend = Running::start(path.clone());
        let change = SettingsChange {
            keychain: vec![KeychainOp::Rename {
                old: "kc-old".into(),
                new: "kc-new".into(),
            }],
            edits: vec![Edit::RemoveCalendar("a".into())],
            ..SettingsChange::default()
        };
        let err = backend.save(change).await.unwrap_err();
        assert!(err.contains("'a'"), "{err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        assert!(in_keychain("kc-old"));
        assert!(!in_keychain("kc-new"));
        backend.stop().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_rename_purges_first_and_does_not_stay_paused() {
        let server = sink_with_one_copy().await;
        let dir = test_dir("rename-purge");
        let path = write_config(&dir, "a-to-b", &server);
        let mut backend = Running::start(path);
        backend.running(&["a-to-b"]).await;
        let change = SettingsChange {
            purge_first: vec!["a-to-b".into()],
            edits: vec![
                Edit::RenameSync("a-to-b".into(), "renamed".into()),
                Edit::SetSync(sync("renamed", "b", 3600)),
            ],
            ..SettingsChange::default()
        };
        assert_eq!(backend.save(change).await, Ok(()));
        backend.running(&["renamed"]).await;
        backend.stop().await;
        server.verify().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_target_change_resumes_the_purged_sync() {
        let server = sink_with_one_copy().await;
        let dir = test_dir("retarget");
        let path = write_config(&dir, "a-to-b", &server);
        let mut backend = Running::start(path);
        backend.running(&["a-to-b"]).await;
        let c = capycal::CalendarConfig {
            account: Some("acc".into()),
            url: Some(format!("{}/c/", server.uri())),
            eventkit_calendar: None,
            ics_url: None,
            concurrency: capycal::DEFAULT_CONCURRENCY,
        };
        let change = SettingsChange {
            purge_first: vec!["a-to-b".into()],
            resume: vec!["a-to-b".into()],
            edits: vec![
                Edit::SetCalendar("c".into(), c),
                Edit::SetSync(sync("a-to-b", "c", 3600)),
            ],
            ..SettingsChange::default()
        };
        backend.tx.send(Command::SaveSettings(change)).unwrap();
        // Counted first, for the progress in Settings
        let counted = backend
            .next(|u| matches!(u, Update::PurgeProgress { .. }))
            .await;
        let expected = Update::PurgeProgress {
            sync: "a-to-b".into(),
            done: 0,
            total: Some(1),
        };
        assert_eq!(counted, expected);
        let saved = backend
            .next(|u| matches!(u, Update::SettingsSaved(_)))
            .await;
        assert_eq!(saved, Update::SettingsSaved(Ok(())));
        backend.running(&["a-to-b"]).await;
        backend.stop().await;
        server.verify().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_discover_account_uses_the_stored_password() {
        use wiremock::matchers::{body_string_contains, header, method, path, path_regex};
        use wiremock::{Mock, ResponseTemplate};
        let server = MockServer::start().await;
        let multistatus = |inner: &str| {
            ResponseTemplate::new(207).set_body_string(format!(
                r#"<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">{inner}</d:multistatus>"#
            ))
        };
        let auth = || header("Authorization", "Basic dTpwa2M="); // u:pkc
        Mock::given(method("PROPFIND"))
            .and(path_regex("^/?$"))
            .and(body_string_contains("current-user-principal"))
            .and(auth())
            .respond_with(multistatus(
                "<d:response><d:href>/</d:href><d:propstat><d:prop><d:current-user-principal>\
                 <d:href>/p/</d:href></d:current-user-principal></d:prop></d:propstat></d:response>",
            ))
            .mount(&server)
            .await;
        Mock::given(method("PROPFIND"))
            .and(path("/p/"))
            .and(auth())
            .respond_with(multistatus(
                "<d:response><d:href>/p/</d:href><d:propstat><d:prop><c:calendar-home-set>\
                 <d:href>/cal/</d:href></c:calendar-home-set></d:prop></d:propstat></d:response>",
            ))
            .mount(&server)
            .await;
        Mock::given(method("PROPFIND"))
            .and(path("/cal/"))
            .and(auth())
            .respond_with(multistatus(
                "<d:response><d:href>/cal/work/</d:href><d:propstat><d:prop><d:resourcetype>\
                 <d:collection/><c:calendar/></d:resourcetype></d:prop></d:propstat></d:response>",
            ))
            .mount(&server)
            .await;
        let dir = test_dir("discover-account");
        let path = dir.join("capycal.toml");
        std::fs::write(
            &path,
            format!(
                "[daemon]\nstate_dir = \"{}\"\n\n[accounts.work]\nurl = \"{}\"\nusername = \"u\"\n\
                 password = {{ keychain = \"kc-disc\" }}\n",
                dir.join("state").display(),
                server.uri()
            ),
        )
        .unwrap();
        (test_keychain().set)("kc-disc", "pkc").unwrap();
        let mut backend = Running::start(path);
        backend
            .tx
            .send(Command::DiscoverAccount("work".into()))
            .unwrap();
        let Update::AccountCalendars { account, result } = backend
            .next(|u| matches!(u, Update::AccountCalendars { .. }))
            .await
        else {
            unreachable!()
        };
        assert_eq!(account, "work");
        assert_eq!(result.unwrap().len(), 1);
        backend.stop().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_forget_state_of_a_deleted_sync() {
        let server = MockServer::start().await;
        let dir = test_dir("forget-state");
        let path = write_config(&dir, "a-to-b", &server);
        let mut backend = Running::start(path);
        backend.running(&["a-to-b"]).await;
        let state = dir.join("state").join("a-to-b.json");
        std::fs::create_dir_all(state.parent().unwrap()).unwrap();
        std::fs::write(&state, "{\"uid_mapping\":{}}").unwrap();
        let change = SettingsChange {
            forget_state: vec!["a-to-b".into()],
            edits: vec![Edit::RemoveSync("a-to-b".into())],
            ..SettingsChange::default()
        };
        assert_eq!(backend.save(change).await, Ok(()));
        assert!(!state.exists());
        backend.stop().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_test_account_and_calendar_app_answer() {
        // Without mocks every request gets 404, so the test fails
        let server = MockServer::start().await;
        let dir = test_dir("test-account");
        let mut backend = Running::start(dir.join("capycal.toml"));
        backend
            .tx
            .send(Command::TestAccount {
                account: None,
                url: server.uri(),
                username: "u".into(),
                password: Some("p".into()),
            })
            .unwrap();
        let Update::AccountTested(result) = backend
            .next(|u| matches!(u, Update::AccountTested(_)))
            .await
        else {
            unreachable!()
        };
        assert!(result.is_err());
        backend
            .tx
            .send(Command::TestAccount {
                account: None,
                url: server.uri(),
                username: "u".into(),
                password: None,
            })
            .unwrap();
        let tested = backend
            .next(|u| matches!(u, Update::AccountTested(_)))
            .await;
        assert_eq!(
            tested,
            Update::AccountTested(Err("Enter the password".into()))
        );

        // On macOS this asks for calendar access, which nobody answers in CI
        #[cfg(not(target_os = "macos"))]
        {
            backend.tx.send(Command::ListCalendarApp).unwrap();
            let listed = backend
                .next(|u| matches!(u, Update::CalendarAppCalendars(_)))
                .await;
            assert!(matches!(listed, Update::CalendarAppCalendars(Err(_))));
        }
        backend.stop().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_failed_deletes_stop_the_save_and_the_sync_runs_on() {
        use wiremock::matchers::{body_string_contains, method, path, path_regex};
        use wiremock::{Mock, ResponseTemplate};
        let server = MockServer::start().await;
        let body = "<d:multistatus xmlns:d=\"DAV:\" xmlns:c=\"urn:ietf:params:xml:ns:caldav\">\
            <d:response><d:href>/b/1.ics</d:href><d:propstat><d:prop><d:getetag>\"e\"</d:getetag>\
            <c:calendar-data>BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:x\nX-CAPYCAL-SYNC:a-to-b\n\
            END:VEVENT\nEND:VCALENDAR</c:calendar-data></d:prop></d:propstat></d:response>\
            </d:multistatus>";
        for m in ["PROPFIND", "REPORT"] {
            let mock = Mock::given(method(m));
            let mock = if m == "PROPFIND" {
                mock.and(path_regex("^/b/?$"))
            } else {
                mock.and(body_string_contains("calendar-multiget"))
            };
            mock.respond_with(ResponseTemplate::new(207).set_body_string(body))
                .mount(&server)
                .await;
        }
        Mock::given(method("DELETE"))
            .and(path("/b/1.ics"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        let dir = test_dir("failed-deletes");
        let path = write_config(&dir, "a-to-b", &server);
        let before = std::fs::read_to_string(&path).unwrap();
        let mut backend = Running::start(path.clone());
        backend.running(&["a-to-b"]).await;
        let change = SettingsChange {
            purge_first: vec!["a-to-b".into()],
            edits: vec![
                Edit::RenameSync("a-to-b".into(), "renamed".into()),
                Edit::SetSync(sync("renamed", "b", 3600)),
            ],
            ..SettingsChange::default()
        };
        let err = backend.save(change).await.unwrap_err();
        assert!(err.contains("could not be removed"), "{err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        backend.running(&["a-to-b"]).await;
        backend.stop().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_a_failed_save_after_the_purge_resumes_the_sync() {
        let server = sink_with_one_copy().await;
        let dir = test_dir("save-after-purge");
        let path = write_config(&dir, "a-to-b", &server);
        let mut backend = Running::start(path);
        backend.running(&["a-to-b"]).await;
        let change = SettingsChange {
            purge_first: vec!["a-to-b".into()],
            resume: vec!["a-to-b".into()],
            edits: vec![Edit::SetSync(sync("a-to-b", "missing", 3600))],
            ..SettingsChange::default()
        };
        assert!(backend.save(change).await.is_err());
        backend.running(&["a-to-b"]).await;
        backend.stop().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_reload_reports_config_changed() {
        let server = MockServer::start().await;
        let dir = test_dir("config-changed");
        let path = write_config(&dir, "a-to-b", &server);
        let mut backend = Running::start(path);
        backend.next(|u| *u == Update::ConfigChanged).await;
        backend.tx.send(Command::Reload).unwrap();
        backend.next(|u| *u == Update::ConfigChanged).await;
        backend.stop().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_edit_config_writes_the_starter_config() {
        let dir = test_dir("edit");
        let path = dir.join("conf/capycal.toml");
        let mut backend = Running::start(path.clone());
        backend.tx.send(Command::EditConfig).unwrap();
        let ready = backend.next(|u| matches!(u, Update::ConfigReady(_))).await;
        assert_eq!(ready, Update::ConfigReady(Ok(())));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), STARTER_CONFIG);
        backend.stop().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The config cannot be written after a purge: the sync stays paused,
    /// and Enable starts it once the config can be written again.
    #[cfg(unix)]
    #[tokio::test]
    async fn test_enable_starts_a_purged_sync_whose_disable_failed() {
        use std::os::unix::fs::PermissionsExt;
        let server = sink_with_one_copy().await;
        let dir = test_dir("purge-readonly");
        let path = write_config(&dir, "a-to-b", &server);
        std::fs::create_dir_all(dir.join("state")).unwrap();
        let mut backend = Running::start(path);
        backend.running(&["a-to-b"]).await;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        backend
            .tx
            .send(Command::Purge(vec!["a-to-b".into()]))
            .unwrap();
        backend.next(|u| matches!(u, Update::Purged(_))).await;
        let problem = backend
            .next(|u| matches!(u, Update::Snapshot(s) if s.problem.is_some()))
            .await;
        let Update::Snapshot(snapshot) = problem else {
            unreachable!()
        };
        assert!(
            matches!(&snapshot.problem, Some(Problem::Purge(p)) if p.contains("not disabled")),
            "{:?}",
            snapshot.problem
        );
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        backend.stop().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_command_debug_hides_the_password() {
        let command = Command::TestAccount {
            account: None,
            url: "https://x/".into(),
            username: "u".into(),
            password: Some("hunter2".into()),
        };
        assert!(!format!("{command:?}").contains("hunter2"));
    }
}
