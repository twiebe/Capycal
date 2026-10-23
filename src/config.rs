use chrono::{NaiveTime, Weekday};
use regex::Regex;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

use crate::error::{Error, Result};
use crate::ical::transform::{Action, Condition, Test, Transform};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The config format version, see `config_migrate`.
    #[serde(default)]
    pub version: Option<i64>,
    #[serde(default)]
    pub daemon: DaemonConfig,
    #[serde(default)]
    pub accounts: HashMap<String, AccountConfig>,
    #[serde(default)]
    pub calendars: HashMap<String, CalendarConfig>,
    /// `[syncs.<name>]` tables, in file order.
    #[serde(default, deserialize_with = "deserialize_syncs")]
    pub syncs: Vec<SyncConfig>,
    /// Directory the config file was loaded from (for resolving relative paths).
    #[serde(skip)]
    pub config_dir: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DaemonConfig {
    pub state_dir: Option<PathBuf>,
    pub log_level: String,
    pub log_format: LogFormat,
    /// Reload automatically when the config file changes (daemon mode).
    pub watch_config: bool,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            state_dir: None,
            log_level: "info".to_string(),
            log_format: LogFormat::Pretty,
            watch_config: false,
        }
    }
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LogFormat {
    Pretty,
    Json,
}

/// A CalDAV account: the server its calendars are discovered on and the
/// credentials they use.
#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AccountConfig {
    pub url: String,
    pub username: String,
    pub password: Credential,
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CalendarConfig {
    // CalDAV fields
    #[serde(default)]
    pub account: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    // EventKit fields (macOS only)
    #[serde(default)]
    pub eventkit_calendar: Option<String>,
    // A read-only iCalendar feed. A Credential, since the address of a
    // private feed is as secret as a password
    #[serde(default)]
    pub ics_url: Option<Credential>,
    // Concurrent event writes to this calendar as a sink, and deletes of a purge
    #[serde(default = "default_concurrency")]
    pub concurrency: usize,
}

impl CalendarConfig {
    /// Infer the calendar type from the fields present.
    pub fn is_eventkit(&self) -> bool {
        self.eventkit_calendar.is_some()
    }

    /// A feed calendar (`ics_url`), only usable as a source.
    pub fn is_ics(&self) -> bool {
        self.ics_url.is_some()
    }

    pub fn url_or_err(&self, name: &str) -> Result<&str> {
        self.url
            .as_deref()
            .ok_or_else(|| Error::Config(format!("calendar '{name}': missing 'url'")))
    }
}

#[derive(Clone, PartialEq)]
pub enum Credential {
    Env {
        env: String,
    },
    File {
        file: PathBuf,
    },
    /// A shell command printing the password, e.g. of a password manager.
    Command {
        command: String,
    },
    /// An entry in the macOS Keychain or the Secret Service (see `keychain`).
    Keychain {
        keychain: String,
    },
    Plain(String),
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Credential::Env { env } => f.debug_struct("Env").field("env", env).finish(),
            Credential::File { file } => f.debug_struct("File").field("file", file).finish(),
            Credential::Command { command } => {
                f.debug_struct("Command").field("command", command).finish()
            }
            Credential::Keychain { keychain } => f
                .debug_struct("Keychain")
                .field("keychain", keychain)
                .finish(),
            Credential::Plain(_) => f.write_str("Plain(***)"),
        }
    }
}

/// Parsed by hand, since an untagged enum would accept unknown keys or
/// report only that no variant matched.
impl<'de> Deserialize<'de> for Credential {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Table {
            env: Option<String>,
            file: Option<PathBuf>,
            command: Option<String>,
            keychain: Option<String>,
        }

        struct CredentialVisitor;

        impl<'de> serde::de::Visitor<'de> for CredentialVisitor {
            type Value = Credential;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str(
                    r#"a password string, { env = "VAR" }, { file = "path" }, { command = "..." } or { keychain = "name" }"#,
                )
            }

            fn visit_str<E: serde::de::Error>(self, s: &str) -> std::result::Result<Credential, E> {
                Ok(Credential::Plain(s.to_string()))
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                map: A,
            ) -> std::result::Result<Credential, A::Error> {
                let table = Table::deserialize(serde::de::value::MapAccessDeserializer::new(map))?;
                match (table.env, table.file, table.command, table.keychain) {
                    (Some(env), None, None, None) => Ok(Credential::Env { env }),
                    (None, Some(file), None, None) => Ok(Credential::File { file }),
                    (None, None, Some(command), None) => Ok(Credential::Command { command }),
                    (None, None, None, Some(keychain)) => Ok(Credential::Keychain { keychain }),
                    _ => Err(serde::de::Error::custom(
                        "expected exactly one of `env`, `file`, `command` or `keychain`",
                    )),
                }
            }
        }

        deserializer.deserialize_any(CredentialVisitor)
    }
}

/// Run `command` with `sh -c` in `config_dir` and return its output,
/// trimmed. Its stderr passes through to capycal's; a failure names only the
/// exit status, never the output.
fn run_credential_command(
    command: &str,
    config_dir: Option<&std::path::Path>,
) -> std::result::Result<String, String> {
    let mut cmd = std::process::Command::new("sh");
    cmd.arg("-c")
        .arg(command)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit());
    if let Some(dir) = config_dir.filter(|d| !d.as_os_str().is_empty()) {
        cmd.current_dir(dir);
    }
    let output = cmd
        .output()
        .map_err(|e| format!("cannot run `{command}`: {e}"))?;
    if !output.status.success() {
        return Err(format!("`{command}` failed with {}", output.status));
    }
    let password = String::from_utf8(output.stdout)
        .map_err(|_| format!("`{command}` printed no valid UTF-8"))?;
    let password = password.trim();
    if password.is_empty() {
        return Err(format!("`{command}` printed nothing"));
    }
    Ok(password.to_string())
}

/// Why the keychain entry `name` could not be read, with a way out when there
/// is no keychain at all.
fn keychain_error(name: &str, error: keyring_core::Error) -> String {
    match error {
        keyring_core::Error::NoEntry => format!("keychain entry '{name}' not found"),
        keyring_core::Error::NoDefaultStore
        | keyring_core::Error::NoStorageAccess(_)
        | keyring_core::Error::PlatformFailure(_) => format!(
            "keychain entry '{name}': {error} (no usable keychain or Secret Service here; \
             use `env` or `file` for the password instead)"
        ),
        other => format!("keychain entry '{name}': {other}"),
    }
}

impl Credential {
    /// Resolve the credential value.
    /// `config_dir` is used to resolve relative file paths.
    pub fn resolve(&self, context: &str, config_dir: Option<&std::path::Path>) -> Result<String> {
        match self {
            Credential::Plain(s) => Ok(s.clone()),
            Credential::Env { env } => std::env::var(env).map_err(|e| Error::Credential {
                context: context.to_string(),
                source: Box::new(e),
            }),
            Credential::File { file } => {
                let path = if file.is_relative() {
                    if let Some(dir) = config_dir {
                        dir.join(file)
                    } else {
                        file.clone()
                    }
                } else {
                    file.clone()
                };
                std::fs::read_to_string(&path)
                    .map(|s| s.trim().to_string())
                    .map_err(|e| Error::Credential {
                        context: context.to_string(),
                        source: Box::new(e),
                    })
            }
            Credential::Command { command } => {
                run_credential_command(command, config_dir).map_err(|e| Error::Credential {
                    context: context.to_string(),
                    source: e.into(),
                })
            }
            Credential::Keychain { keychain } => {
                crate::keychain::password(keychain).map_err(|e| Error::Credential {
                    context: context.to_string(),
                    source: keychain_error(keychain, e).into(),
                })
            }
        }
    }
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SyncConfig {
    /// The key of the `[syncs.<name>]` table.
    #[serde(skip)]
    pub name: String,
    pub source: String,
    pub target: String,
    #[serde(deserialize_with = "deserialize_duration")]
    pub interval: Duration,
    /// Content properties copied on top of `ical::filter::STRUCTURAL_FIELDS`.
    #[serde(default)]
    pub fields: Vec<String>,
    #[serde(default)]
    pub transforms: Vec<TransformConfig>,
    // EventKit source options (macOS only, but parsed on all platforms for config portability)
    #[serde(default = "default_eventkit_days_past")]
    pub eventkit_days_past: u64,
    #[serde(default = "default_eventkit_days_future")]
    pub eventkit_days_future: u64,
    #[serde(default)]
    pub eventkit_all_events: bool,
    #[serde(
        default = "default_full_sync_interval",
        deserialize_with = "deserialize_duration"
    )]
    pub full_sync_interval: Duration,
    /// A disabled sync is validated but not run.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_enabled() -> bool {
    true
}

pub(crate) fn default_full_sync_interval() -> Duration {
    Duration::from_secs(7 * 86400)
}

pub(crate) fn default_eventkit_days_past() -> u64 {
    30
}

pub(crate) fn default_eventkit_days_future() -> u64 {
    365
}

/// Events written to a CalDAV target at the same time, unless the
/// calendar sets `concurrency`.
pub const DEFAULT_CONCURRENCY: usize = 2;

fn default_concurrency() -> usize {
    DEFAULT_CONCURRENCY
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TransformConfig {
    #[serde(default)]
    pub field: String,
    pub action: TransformAction,
    #[serde(default)]
    pub pattern: Option<String>,
    #[serde(default)]
    pub replacement: Option<String>,
    #[serde(default)]
    pub search: Option<String>,
    #[serde(default)]
    pub count: Option<usize>,
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_duration")]
    pub pre_event: Option<Duration>,
    #[serde(default, deserialize_with = "deserialize_optional_duration")]
    pub post_event: Option<Duration>,
    #[serde(default, deserialize_with = "deserialize_optional_duration")]
    pub trigger: Option<Duration>,
    /// The action runs only when all of these match.
    #[serde(default)]
    pub when: Vec<ConditionConfig>,
}

/// One condition of a transform: exactly one test, optionally negated, or
/// `any`, a group of tests of which one must match.
#[derive(Debug, Deserialize, Clone, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct ConditionConfig {
    #[serde(default)]
    pub any: Vec<ConditionConfig>,
    #[serde(default)]
    pub field: String,
    #[serde(default)]
    pub contains: Option<String>,
    #[serde(default)]
    pub matches: Option<String>,
    #[serde(default)]
    pub empty: Option<bool>,
    #[serde(default)]
    pub days: Option<Vec<String>>,
    #[serde(default)]
    pub start_time: Option<String>,
    #[serde(default)]
    pub end_time: Option<String>,
    #[serde(default)]
    pub all_day: Option<bool>,
    #[serde(default)]
    pub not: bool,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TransformAction {
    SetField,
    ReplaceField,
    ReplaceFieldRegex,
    RemoveField,
    AddBuffer,
    AddAlarm,
    Skip,
    /// Old names, rejected with a hint (see `TransformAction::renamed_to`).
    Set,
    RegexReplace,
    Remove,
    FilterTimeRange,
    FilterAllDay,
    ExcludeTimeRange,
    ExcludeAllDay,
    ExcludeField,
    ExcludeFieldRegex,
}

impl TransformAction {
    /// The name in the config, the inverse of deserializing.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SetField => "set_field",
            Self::ReplaceField => "replace_field",
            Self::ReplaceFieldRegex => "replace_field_regex",
            Self::RemoveField => "remove_field",
            Self::AddBuffer => "add_buffer",
            Self::AddAlarm => "add_alarm",
            Self::Skip => "skip",
            Self::Set => "set",
            Self::RegexReplace => "regex_replace",
            Self::Remove => "remove",
            Self::FilterTimeRange => "filter_time_range",
            Self::FilterAllDay => "filter_all_day",
            Self::ExcludeTimeRange => "exclude_time_range",
            Self::ExcludeAllDay => "exclude_all_day",
            Self::ExcludeField => "exclude_field",
            Self::ExcludeFieldRegex => "exclude_field_regex",
        }
    }
    /// The old name and the new one of a renamed action.
    fn renamed_to(&self) -> Option<(&'static str, &'static str)> {
        match self {
            Self::Set => Some(("set", "set_field")),
            Self::RegexReplace => Some(("regex_replace", "replace_field_regex")),
            Self::Remove => Some(("remove", "remove_field")),
            _ => None,
        }
    }
}

/// The `[syncs.<name>]` tables in file order, each named by its key.
fn deserialize_syncs<'de, D>(deserializer: D) -> std::result::Result<Vec<SyncConfig>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct Syncs;

    impl<'de> serde::de::Visitor<'de> for Syncs {
        type Value = Vec<SyncConfig>;

        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("[syncs.<name>] tables")
        }

        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut map: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            let mut syncs = Vec::new();
            while let Some((name, mut sync)) = map.next_entry::<String, SyncConfig>()? {
                sync.name = name;
                syncs.push(sync);
            }
            Ok(syncs)
        }

        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            _: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            Err(serde::de::Error::custom(
                "syncs are named tables now: replace `[[syncs]]` and its `name = \"x\"` \
                 with `[syncs.x]`, and `[[syncs.transforms]]` with `[[syncs.x.transforms]]` \
                 or a `transforms = [...]` list",
            ))
        }
    }

    deserializer.deserialize_map(Syncs)
}

fn deserialize_duration<'de, D>(deserializer: D) -> std::result::Result<Duration, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    humantime::parse_duration(&s).map_err(serde::de::Error::custom)
}

fn deserialize_optional_duration<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<Duration>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let opt: Option<String> = Option::deserialize(deserializer)?;
    match opt {
        Some(s) => humantime::parse_duration(&s)
            .map(Some)
            .map_err(serde::de::Error::custom),
        None => Ok(None),
    }
}

/// Turn a calendar display name into a calendar name for `[calendars.<key>]`:
/// letters of any script, digits, `-` and `_` stay, whitespace becomes `_`,
/// everything else is dropped (the name also ends up in the event markers,
/// where `/` and `]` are separators).
pub fn calendar_key(display_name: &str) -> String {
    let kept: String = display_name
        .to_lowercase()
        .chars()
        .filter_map(|c| match c {
            c if c.is_whitespace() => Some('_'),
            c if c.is_alphanumeric() || c == '-' || c == '_' => Some(c),
            _ => None,
        })
        .collect();
    let name = kept
        .split('_')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("_");
    if name.is_empty() {
        "calendar".to_string()
    } else {
        name
    }
}

/// Returns the platform-specific default config path.
pub fn default_config_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "capycal")
        .map(|dirs| dirs.config_dir().join("capycal.toml"))
}

/// Returns the platform-specific default state directory.
pub fn default_state_dir() -> Option<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "capycal")?;
    // state_dir() is available on Linux (XDG_STATE_HOME), returns None on macOS/Windows
    dirs.state_dir()
        .map(|p| p.to_path_buf())
        .or_else(|| Some(dirs.data_local_dir().join("state")))
}

/// Environment variable for the state directory when the config sets none.
pub const STATE_DIR_ENV: &str = "CAPYCAL_STATE_DIR";

/// Calendar.app returns nothing for a single query over about four years.
const EVENTKIT_MAX_WINDOW_DAYS: u64 = 1460;

impl Config {
    pub fn load(path: &std::path::Path) -> Result<Self> {
        let content =
            std::fs::read_to_string(path).map_err(|e| Error::Config(format!("{path:?}: {e}")))?;
        let parse_error = |e| Error::Config(format!("failed to parse {}: {e}", path.display()));
        // A file toml_edit cannot read gets the error of the config parser
        let mut doc = match content.parse::<toml_edit::DocumentMut>() {
            Ok(doc) => doc,
            Err(_) => return Err(parse_error(parse(&content).err().unwrap_or_default())),
        };
        let notes = crate::config_migrate::migrate(&mut doc)
            .map_err(|e| Error::Config(format!("{}: {e}", path.display())))?;
        let text = if notes.is_empty() {
            content.clone()
        } else {
            doc.to_string()
        };
        let mut config = parse(&text).map_err(parse_error)?;
        config.config_dir = path.parent().map(|p| p.to_path_buf());
        config.validate()?;
        if !notes.is_empty() {
            crate::config_migrate::write_back(path, &content, &text, &notes);
        }
        Ok(config)
    }

    /// Resolve the state directory: `state_dir` in the config, then the
    /// `CAPYCAL_STATE_DIR` environment variable, then the platform default.
    pub fn resolve_state_dir(&self) -> Result<PathBuf> {
        self.state_dir_with_env(std::env::var_os(STATE_DIR_ENV))
    }

    fn state_dir_with_env(&self, env: Option<std::ffi::OsString>) -> Result<PathBuf> {
        self.daemon
            .state_dir
            .clone()
            .or_else(|| env.filter(|v| !v.is_empty()).map(PathBuf::from))
            .or_else(default_state_dir)
            .ok_or_else(|| {
                Error::Config(format!(
                    "could not determine state directory - set state_dir in [daemon] section or {STATE_DIR_ENV}"
                ))
            })
    }

    /// Check references and values beyond what parsing does. A config without
    /// syncs is valid; front ends that need syncs check that themselves.
    pub fn validate(&self) -> Result<()> {
        for sync in &self.syncs {
            if sync.name.is_empty() {
                return Err(Error::Config("sync name must not be empty".to_string()));
            }
            if sync.name.contains('/') || sync.name.contains('\\') || sync.name.starts_with('.') {
                return Err(Error::Config(format!(
                    "sync name '{}' contains invalid characters (no slashes or leading dots)",
                    sync.name
                )));
            }
            if !self.calendars.contains_key(&sync.source) {
                return Err(Error::Config(format!(
                    "sync '{}' references unknown source calendar '{}'",
                    sync.name, sync.source
                )));
            }
            if !self.calendars.contains_key(&sync.target) {
                return Err(Error::Config(format!(
                    "sync '{}' references unknown target calendar '{}'",
                    sync.name, sync.target
                )));
            }
            if self.calendars[&sync.target].is_ics() {
                return Err(Error::Config(format!(
                    "sync '{}': calendar '{}' is a read-only feed (ics_url) and cannot be a target",
                    sync.name, sync.target
                )));
            }
            if sync.source == sync.target {
                return Err(Error::Config(format!(
                    "sync '{}' has the same source and target calendar '{}'",
                    sync.name, sync.source
                )));
            }
            if sync.full_sync_interval.is_zero() {
                return Err(Error::Config(format!(
                    "sync '{}': full_sync_interval must be positive",
                    sync.name
                )));
            }
            let window = sync
                .eventkit_days_past
                .saturating_add(sync.eventkit_days_future);
            if self.calendars[&sync.source].is_eventkit()
                && !sync.eventkit_all_events
                && window > EVENTKIT_MAX_WINDOW_DAYS
            {
                return Err(Error::Config(format!(
                    "sync '{}': eventkit_days_past + eventkit_days_future is {window} days, but \
                     Calendar.app returns nothing for a query over about 4 years ({EVENTKIT_MAX_WINDOW_DAYS} days). \
                     Use smaller values or set eventkit_all_events = true",
                    sync.name
                )));
            }
            sync.compile_transforms()?;
        }

        for (name, cal) in &self.calendars {
            if cal.is_ics() {
                if cal.account.is_some() || cal.url.is_some() || cal.is_eventkit() {
                    return Err(Error::Config(format!(
                        "calendar '{name}': ics_url takes no 'account', 'url' or 'eventkit_calendar'"
                    )));
                }
            } else if cal.is_eventkit() {
                if cal.account.is_some() {
                    return Err(Error::Config(format!(
                        "calendar '{name}': 'account' is only for CalDAV calendars"
                    )));
                }
                #[cfg(not(target_os = "macos"))]
                return Err(Error::Config(format!(
                    "calendar '{name}': eventkit_calendar is only supported on macOS"
                )));
            } else {
                if cal.url.is_none() {
                    return Err(Error::Config(format!(
                        "calendar '{name}': missing 'url' (or set 'eventkit_calendar' for macOS Calendar.app)"
                    )));
                }
                match cal.account.as_deref() {
                    None => {
                        return Err(Error::Config(format!(
                            "calendar '{name}': requires 'account'"
                        )));
                    }
                    Some(account) if !self.accounts.contains_key(account) => {
                        return Err(Error::Config(format!(
                            "calendar '{name}' references unknown account '{account}'"
                        )));
                    }
                    Some(_) => {}
                }
            }
        }

        Ok(())
    }
}

impl SyncConfig {
    /// A sync with the defaults of a new one in the desktop apps: every 5
    /// minutes, everything else as in a config that leaves it out.
    pub fn new(name: &str, source: &str, target: &str) -> Self {
        Self {
            name: name.to_string(),
            source: source.to_string(),
            target: target.to_string(),
            interval: Duration::from_secs(300),
            fields: Vec::new(),
            transforms: Vec::new(),
            eventkit_days_past: default_eventkit_days_past(),
            eventkit_days_future: default_eventkit_days_future(),
            eventkit_all_events: false,
            full_sync_interval: default_full_sync_interval(),
            enabled: true,
        }
    }

    /// A key of everything besides the source events that shapes this sync's
    /// copies. A different key makes the next cycle a full sync.
    pub fn output_key(&self, source: &CalendarConfig, target: &CalendarConfig) -> String {
        // Spelled out field by field, not through Debug: a key that changes
        // with the code would full-sync every sync after an upgrade
        let opt = |v: &Option<String>| v.clone().unwrap_or_default();
        let secs = |d: Option<Duration>| d.map(|d| d.as_secs().to_string()).unwrap_or_default();
        let mut fields = self.fields.clone();
        fields.sort();
        // Only appended for a feed, so the keys of other syncs stay as they were
        let feed = match &source.ics_url {
            Some(Credential::Plain(url)) => format!("|{url}"),
            Some(other) => format!("|{other:?}"),
            None => String::new(),
        };
        let mut lines = vec![
            format!(
                "source={}|{}|{}{feed}",
                self.source,
                opt(&source.url),
                opt(&source.eventkit_calendar)
            ),
            format!(
                "target={}|{}|{}",
                self.target,
                opt(&target.url),
                opt(&target.eventkit_calendar)
            ),
            format!("fields={}", fields.join(",")),
            format!(
                "eventkit={}|{}|{}",
                self.eventkit_days_past, self.eventkit_days_future, self.eventkit_all_events
            ),
        ];
        for t in &self.transforms {
            lines.push(format!(
                "transform={}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
                t.action.as_str(),
                t.field,
                opt(&t.pattern),
                opt(&t.replacement),
                opt(&t.search),
                t.count.map(|c| c.to_string()).unwrap_or_default(),
                opt(&t.value),
                secs(t.pre_event),
                secs(t.post_event),
                secs(t.trigger),
                // Once days, start_time and end_time: empty, so the key of a
                // sync without conditions stays
                "",
                "",
                "",
            ));
            let flag = |b: Option<bool>| b.map(|b| b.to_string()).unwrap_or_default();
            for group in &t.when {
                if !group.any.is_empty() {
                    lines.push("any".to_string());
                }
                for c in group.tests() {
                    lines.push(format!(
                        "when={}|{}|{}|{}|{}|{}|{}|{}|{}",
                        c.field,
                        opt(&c.contains),
                        opt(&c.matches),
                        flag(c.empty),
                        c.days.as_deref().unwrap_or_default().join(","),
                        opt(&c.start_time),
                        opt(&c.end_time),
                        flag(c.all_day),
                        c.not,
                    ));
                }
                if !group.any.is_empty() {
                    lines.push("end".to_string());
                }
            }
        }
        uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, lines.join("\n").as_bytes()).to_string()
    }

    /// The copy this sync would write of `source_ical`, or None when it
    /// skips the event: what the desktop apps show as a preview.
    pub fn preview(
        &self,
        source_ical: &str,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<String>> {
        let fields = self.fields.iter().cloned().collect();
        let transforms = self.compile_transforms()?;
        let ctx = crate::ical::transform::TransformContext {
            source_name: self.source.clone(),
            source_url: String::new(),
            sync_name: self.name.clone(),
        };
        Ok(crate::ical::process_event(
            source_ical,
            &fields,
            &transforms,
            &ctx,
            "preview",
            "preview",
            &self.name,
            &self.source,
            now,
        ))
    }

    /// Validate and compile this sync's transforms into their domain form.
    /// A `replace_field` or `replace_field_regex` on a property that `fields`
    /// drops and no earlier `set_field` adds would do nothing, so it is an error.
    /// A condition on a property an earlier unconditional `set_field` or
    /// `remove_field` fixed always gives the same result, so it is an error too.
    pub fn compile_transforms(&self) -> Result<Vec<Transform>> {
        let mut present: HashSet<&str> = self.fields.iter().map(String::as_str).collect();
        present.extend(crate::ical::filter::STRUCTURAL_FIELDS);
        // Properties with a known value, and the transform (from 1) that set it
        let mut fixed: HashMap<&str, usize> = HashMap::new();
        let mut compiled = Vec::new();
        for (i, t) in self.transforms.iter().enumerate() {
            compiled.push(t.compile(&self.name)?);
            for c in t.when.iter().flat_map(ConditionConfig::tests) {
                if let Some(set_by) = fixed.get(c.field.as_str()) {
                    let field = &c.field;
                    return Err(Error::Config(format!(
                        "sync '{}': the condition on {field} in transform {} always sees what \
                         transform {set_by} left in it. Move it above that transform to test \
                         the event's own {field}",
                        self.name,
                        i + 1
                    )));
                }
            }
            if matches!(
                t.action,
                TransformAction::SetField
                    | TransformAction::RemoveField
                    | TransformAction::ReplaceField
                    | TransformAction::ReplaceFieldRegex
            ) {
                fixed.remove(t.field.as_str());
            }
            match t.action {
                TransformAction::SetField => {
                    present.insert(&t.field);
                    if t.when.is_empty() {
                        fixed.insert(&t.field, i + 1);
                    }
                }
                TransformAction::RemoveField if t.when.is_empty() => {
                    present.remove(t.field.as_str());
                    fixed.insert(&t.field, i + 1);
                }
                TransformAction::ReplaceField | TransformAction::ReplaceFieldRegex
                    if !present.contains(t.field.as_str()) =>
                {
                    let field = &t.field;
                    let action = match t.action {
                        TransformAction::ReplaceField => "replace_field",
                        _ => "replace_field_regex",
                    };
                    return Err(Error::Config(format!(
                        "sync '{}': {action} on {field} has no effect because {field} is \
                         not in fields. Add \"{field}\" to fields (events it does not \
                         change keep their {field}), or use set_field to replace it entirely",
                        self.name
                    )));
                }
                _ => {}
            }
        }
        Ok(compiled)
    }
}

impl TransformConfig {
    pub fn new(action: TransformAction) -> Self {
        Self {
            field: String::new(),
            action,
            pattern: None,
            replacement: None,
            search: None,
            count: None,
            value: None,
            pre_event: None,
            post_event: None,
            trigger: None,
            when: Vec::new(),
        }
    }

    fn compile(&self, sync_name: &str) -> Result<Transform> {
        let field = self.field.clone();
        let err = |msg: String| Error::Config(format!("sync '{sync_name}': {msg}"));
        let needs_field = matches!(
            self.action,
            TransformAction::SetField
                | TransformAction::ReplaceField
                | TransformAction::ReplaceFieldRegex
                | TransformAction::RemoveField
        );
        if needs_field && field.is_empty() {
            return Err(err(format!(
                "the transform action '{}' needs a 'field'",
                self.action.as_str()
            )));
        }

        let action = match self.action {
            TransformAction::SetField => Action::Set {
                field,
                value: self.value.clone().ok_or_else(|| {
                    err(format!(
                        "transform on field '{}' with action 'set_field' requires 'value'",
                        self.field
                    ))
                })?,
            },
            TransformAction::ReplaceFieldRegex => {
                let (Some(pattern), Some(replacement)) = (&self.pattern, &self.replacement) else {
                    return Err(err(format!(
                        "transform on field '{}' with action 'replace_field_regex' requires 'pattern' and 'replacement'",
                        self.field
                    )));
                };
                let regex = Regex::new(pattern).map_err(|e| {
                    err(format!(
                        "invalid regex pattern for field '{}': {e}",
                        self.field
                    ))
                })?;
                Action::RegexReplace {
                    field,
                    regex,
                    replacement: replacement.clone(),
                }
            }
            TransformAction::ReplaceField => {
                let (Some(search), Some(replacement)) = (&self.search, &self.replacement) else {
                    return Err(err(format!(
                        "transform on field '{}' with action 'replace_field' requires 'search' and 'replacement'",
                        self.field
                    )));
                };
                if search.is_empty() || self.count == Some(0) {
                    return Err(err(format!(
                        "transform on field '{}' with action 'replace_field' needs a non-empty 'search' and a 'count' above 0",
                        self.field
                    )));
                }
                Action::ReplaceField {
                    field,
                    search: search.clone(),
                    replacement: replacement.clone(),
                    count: self.count,
                }
            }
            TransformAction::RemoveField => Action::Remove { field },
            TransformAction::AddBuffer => {
                if self.pre_event.is_none() && self.post_event.is_none() {
                    return Err(err(
                        "add_buffer transform requires at least one of 'pre_event' or 'post_event'"
                            .to_string(),
                    ));
                }
                Action::AddBuffer {
                    pre: self.pre_event,
                    post: self.post_event,
                }
            }
            TransformAction::AddAlarm => Action::AddAlarm {
                trigger: self
                    .trigger
                    .ok_or_else(|| err("add_alarm transform requires 'trigger'".to_string()))?,
            },
            TransformAction::Skip => {
                if self.when.is_empty() {
                    return Err(err(
                        "skip without a condition in 'when' would skip every event".to_string(),
                    ));
                }
                if !self.field.is_empty() {
                    return Err(err(
                        "skip has no 'field'; name the field in its conditions".to_string()
                    ));
                }
                Action::Skip
            }
            TransformAction::FilterTimeRange
            | TransformAction::FilterAllDay
            | TransformAction::ExcludeTimeRange
            | TransformAction::ExcludeAllDay
            | TransformAction::ExcludeField
            | TransformAction::ExcludeFieldRegex => {
                return Err(err(format!(
                    "the transform action '{}' was replaced by action = \"skip\" with a \
                     condition in 'when', for example {{ action = \"skip\", when = [{{ all_day = true }}] }}",
                    self.action.as_str()
                )));
            }
            TransformAction::Set | TransformAction::RegexReplace | TransformAction::Remove => {
                let (old, new) = self.action.renamed_to().unwrap_or_default();
                return Err(err(format!(
                    "the transform action '{old}' was renamed to '{new}'"
                )));
            }
        };

        for text in [&self.value, &self.replacement].into_iter().flatten() {
            validate_placeholders(text, sync_name, &self.field)?;
        }

        let when = self
            .when
            .iter()
            .map(|c| c.compile().map_err(err))
            .collect::<Result<_>>()?;
        Ok(Transform { action, when })
    }
}

impl ConditionConfig {
    /// The tests of this condition: itself, or the conditions of its `any`.
    pub fn tests(&self) -> &[ConditionConfig] {
        if self.any.is_empty() {
            std::slice::from_ref(self)
        } else {
            &self.any
        }
    }

    /// The group of conditions of which one must match.
    fn compile(&self) -> std::result::Result<Vec<Condition>, String> {
        if self.any.is_empty() {
            return Ok(vec![self.compile_test()?]);
        }
        let only_any = ConditionConfig {
            any: self.any.clone(),
            ..ConditionConfig::default()
        };
        if *self != only_any {
            return Err(
                "a condition with 'any' takes no other keys: put them into one of its conditions"
                    .to_string(),
            );
        }
        if self.any.len() < 2 {
            return Err(
                "'any' needs at least two conditions; write a single one without 'any'".to_string(),
            );
        }
        self.any
            .iter()
            .map(|c| {
                if c.any.is_empty() {
                    c.compile_test()
                } else {
                    Err(
                        "'any' cannot hold another 'any': list all its conditions in one"
                            .to_string(),
                    )
                }
            })
            .collect()
    }

    fn compile_test(&self) -> std::result::Result<Condition, String> {
        let tests = [
            ("contains", self.contains.is_some()),
            ("matches", self.matches.is_some()),
            ("empty", self.empty.is_some()),
            ("days", self.days.is_some()),
            ("all_day", self.all_day.is_some()),
        ];
        let named: Vec<&str> = tests
            .iter()
            .filter(|(_, on)| *on)
            .map(|(n, _)| *n)
            .collect();
        let name = match named[..] {
            [name] => name,
            [] if self.start_time.is_some() || self.end_time.is_some() => {
                return Err("a condition with 'start_time' or 'end_time' needs 'days'".to_string());
            }
            [] => {
                return Err("a condition needs one of 'contains', 'matches', 'empty', \
                            'days', 'all_day' or 'any'"
                    .to_string());
            }
            _ => {
                return Err(format!(
                    "a condition takes only one of {}; write one condition per test",
                    named.join(", ")
                ));
            }
        };
        let field = self.field.clone();
        let needs_field = matches!(name, "contains" | "matches" | "empty");
        if needs_field && field.is_empty() {
            return Err(format!("the condition '{name}' needs a 'field'"));
        }
        if !needs_field && !field.is_empty() {
            return Err(format!("'field' has no effect on the condition '{name}'"));
        }
        // Property names are compared as stored, in upper case
        if field != field.to_uppercase() {
            return Err(format!(
                "write the field name in upper case: {}",
                field.to_uppercase()
            ));
        }
        if name != "days" {
            for (key, set) in [
                ("start_time", self.start_time.is_some()),
                ("end_time", self.end_time.is_some()),
            ] {
                if set {
                    return Err(format!("'{key}' only goes with 'days', not with '{name}'"));
                }
            }
        }
        if self.not && matches!(name, "empty" | "all_day") {
            return Err(format!(
                "instead of not = true, use {name} = {}",
                !(self.empty.or(self.all_day).unwrap_or(true))
            ));
        }
        let test = match name {
            "contains" => {
                let text = self.contains.clone().unwrap_or_default();
                if text.is_empty() {
                    return Err("the condition 'contains' needs a text".to_string());
                }
                Test::Contains { field, text }
            }
            "matches" => {
                let pattern = self.matches.as_deref().unwrap_or_default();
                if pattern.is_empty() {
                    return Err("the condition 'matches' needs a regex".to_string());
                }
                let regex = Regex::new(pattern)
                    .map_err(|e| format!("invalid regex pattern for field '{field}': {e}"))?;
                Test::Matches { field, regex }
            }
            "empty" => Test::Empty { field },
            "days" => {
                if self.days.as_ref().is_some_and(Vec::is_empty) {
                    return Err("the condition 'days' needs at least one day".to_string());
                }
                let parse_time = |t: &Option<String>| t.as_deref().map(parse_hhmm).transpose();
                Test::TimeRange {
                    days: self
                        .days
                        .iter()
                        .flatten()
                        .map(|d| parse_weekday(d))
                        .collect::<std::result::Result<_, _>>()?,
                    start: parse_time(&self.start_time)?,
                    end: parse_time(&self.end_time)?,
                }
            }
            _ => Test::AllDay,
        };
        // empty = false and all_day = false test for the opposite
        let negate = self.not || self.empty == Some(false) || self.all_day == Some(false);
        Ok(Condition { test, negate })
    }
}

fn parse(content: &str) -> std::result::Result<Config, String> {
    toml::from_str(content).map_err(|e| {
        let msg = e.to_string();
        // Keys of the old exclude_time_range / filter_time_range actions
        let old_key = ["days", "start_time", "end_time"]
            .iter()
            .any(|k| msg.contains(&format!("unknown field `{k}`")));
        if old_key {
            format!(
                "{msg}days, start_time and end_time moved into a condition: \
                 {{ action = \"skip\", when = [{{ days = [\"sat\", \"sun\"] }}] }}"
            )
        } else {
            msg
        }
    })
}

fn parse_weekday(day: &str) -> std::result::Result<Weekday, String> {
    Ok(match day {
        "mon" => Weekday::Mon,
        "tue" => Weekday::Tue,
        "wed" => Weekday::Wed,
        "thu" => Weekday::Thu,
        "fri" => Weekday::Fri,
        "sat" => Weekday::Sat,
        "sun" => Weekday::Sun,
        _ => {
            return Err(format!(
                "invalid day '{day}'. valid: mon, tue, wed, thu, fri, sat, sun"
            ));
        }
    })
}

fn parse_hhmm(time: &str) -> std::result::Result<NaiveTime, String> {
    let Some((h, m)) = time.split_once(':') else {
        return Err(format!("invalid time format '{time}', expected HH:MM"));
    };
    let h: u32 = h
        .parse()
        .map_err(|_| format!("invalid hour in time '{time}'"))?;
    let m: u32 = m
        .parse()
        .map_err(|_| format!("invalid minute in time '{time}'"))?;
    NaiveTime::from_hms_opt(h, m, 0).ok_or_else(|| format!("time '{time}' out of range"))
}

const KNOWN_PLACEHOLDERS: &[&str] = &["{{source_name}}", "{{source_url}}", "{{sync_name}}"];

fn validate_placeholders(text: &str, sync_name: &str, field: &str) -> Result<()> {
    let mut pos = 0;
    while let Some(start) = text[pos..].find("{{") {
        let abs_start = pos + start;
        if let Some(end) = text[abs_start..].find("}}") {
            let placeholder = &text[abs_start..abs_start + end + 2];
            if !KNOWN_PLACEHOLDERS.contains(&placeholder) {
                return Err(Error::Config(format!(
                    "sync '{sync_name}': unknown placeholder '{placeholder}' in transform for field '{field}'. \
                     known placeholders: {{{{source_name}}}}, {{{{source_url}}}}, {{{{sync_name}}}}"
                )));
            }
            pos = abs_start + end + 2;
        } else {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    const PREVIEW_EVENT: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\n\
        UID:e1\r\nDTSTAMP:20261015T000000Z\r\nDTSTART:20261020T090000\r\n\
        DTEND:20261020T100000\r\nSUMMARY:Design review\r\nLOCATION:Room 3\r\n\
        END:VEVENT\r\nEND:VCALENDAR\r\n";

    #[test]
    fn test_preview_runs_fields_and_transforms() {
        let mut sync = SyncConfig::new("s", "a", "b");
        let mut busy = TransformConfig::new(TransformAction::SetField);
        busy.field = "SUMMARY".into();
        busy.value = Some("Busy".into());
        let mut buffer = TransformConfig::new(TransformAction::AddBuffer);
        buffer.pre_event = Some(Duration::from_secs(900));
        sync.transforms = vec![busy, buffer];
        let copy = sync
            .preview(PREVIEW_EVENT, chrono::Utc::now())
            .unwrap()
            .unwrap();
        assert!(copy.contains("SUMMARY:Busy"));
        assert!(copy.contains("DTSTART:20261020T084500"), "{copy}");
        assert!(!copy.contains("LOCATION"));

        // 2026-10-20 is a Tuesday
        let mut skip = TransformConfig::new(TransformAction::Skip);
        skip.when = vec![ConditionConfig {
            days: Some(vec!["tue".into()]),
            ..ConditionConfig::default()
        }];
        sync.transforms = vec![skip];
        assert_eq!(
            sync.preview(PREVIEW_EVENT, chrono::Utc::now()).unwrap(),
            None
        );
    }

    #[test]
    fn test_parse_minimal_config() {
        let toml = r#"
[daemon]
state_dir = "/tmp/capycal"

[accounts.acc]
url = "https://caldav.example.com/"
username = "alice"
password = "secret"

[accounts.acc2]
url = "https://caldav.example.com/"
username = "alice"
password = { env = "CAPYCAL_PASSWORD" }

[calendars.work]
url = "https://caldav.example.com/cal/"
account = "acc"

[calendars.target]
url = "https://caldav.example.com/target/"
account = "acc2"

[syncs.work-to-target]
source = "work"
target = "target"
interval = "5m"
fields = ["DTSTART", "DTEND"]

transforms = [
  { field = "SUMMARY", action = "set_field", value = "Busy ({{source_name}})" },
]
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.syncs.len(), 1);
        assert_eq!(config.syncs[0].name, "work-to-target");
        assert_eq!(config.syncs[0].interval, Duration::from_secs(300));
        assert_eq!(config.syncs[0].transforms.len(), 1);
        assert_eq!(
            config.syncs[0].transforms[0].action,
            TransformAction::SetField
        );
        assert!(!config.daemon.watch_config);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_parse_watch_config() {
        let toml = r#"
[daemon]
watch_config = true

[calendars]
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!(config.daemon.watch_config);
    }

    #[test]
    fn test_validate_unknown_source() {
        let toml = r#"
[daemon]
state_dir = "/tmp/capycal"

[accounts.acc]
url = "https://caldav.example.com/"
username = "alice"
password = "secret"

[calendars.work]
url = "https://caldav.example.com/cal/"
account = "acc"

[syncs.bad]
source = "nonexistent"
target = "work"
interval = "5m"
fields = ["DTSTART"]
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_validate_same_source_and_sink() {
        let toml = r#"
[daemon]
state_dir = "/tmp/capycal"

[accounts.acc]
url = "https://caldav.example.com/"
username = "alice"
password = "secret"

[calendars.work]
url = "https://caldav.example.com/cal/"
account = "acc"

[syncs.self-sync]
source = "work"
target = "work"
interval = "5m"
fields = ["DTSTART"]
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_validate_regex_transform() {
        let toml = r#"
[daemon]
state_dir = "/tmp/capycal"

[accounts.acc]
url = "https://caldav.example.com/"
username = "u"
password = "p"

[calendars.a]
url = "https://a.example.com/"
account = "acc"

[calendars.b]
url = "https://b.example.com/"
account = "acc"

[syncs.test]
source = "a"
target = "b"
interval = "1m"
fields = ["SUMMARY"]

transforms = [
  { field = "SUMMARY", action = "replace_field_regex", pattern = "(.*)", replacement = "Busy: $1" },
]
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_validate_sync_name_path_traversal() {
        let toml = r#"
[daemon]
state_dir = "/tmp/capycal"

[accounts.acc]
url = "https://caldav.example.com/"
username = "u"
password = "p"

[calendars.a]
url = "https://a.example.com/"
account = "acc"

[calendars.b]
url = "https://b.example.com/"
account = "acc"

[syncs."../../etc/evil"]
source = "a"
target = "b"
interval = "1m"
fields = ["DTSTART"]
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_credential_debug_hides_password() {
        let cred = Credential::Plain("super-secret".to_string());
        let debug_str = format!("{:?}", cred);
        assert!(!debug_str.contains("super-secret"));
        assert!(debug_str.contains("***"));
    }

    #[test]
    fn test_credential_env() {
        // SAFETY: test runs single-threaded, no other threads reading this var
        unsafe { std::env::set_var("CAPYCAL_TEST_PW", "test123") };
        let cred = Credential::Env {
            env: "CAPYCAL_TEST_PW".to_string(),
        };
        assert_eq!(cred.resolve("test", None).unwrap(), "test123");
        unsafe { std::env::remove_var("CAPYCAL_TEST_PW") };
    }

    #[test]
    fn test_credential_env_missing() {
        let cred = Credential::Env {
            env: "CAPYCAL_DEFINITELY_NOT_SET_12345".to_string(),
        };
        assert!(cred.resolve("test", None).is_err());
    }

    #[test]
    fn test_credential_file() {
        let dir = std::env::temp_dir().join("capycal-test-cred");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("pw.txt");
        std::fs::write(&path, "  file-password\n  ").unwrap();

        let cred = Credential::File { file: path };
        assert_eq!(cred.resolve("test", None).unwrap(), "file-password");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_credential_command() {
        let resolve = |command: &str| {
            Credential::Command {
                command: command.to_string(),
            }
            .resolve("test", None)
        };
        assert_eq!(resolve("printf '  s3cret\\n'").unwrap(), "s3cret");
        // The output (here "LEAKED") never shows up in the error
        let failed = resolve("echo leaked | tr a-z A-Z; exit 3")
            .unwrap_err()
            .to_string();
        assert!(failed.contains("failed with exit status: 3"), "{failed}");
        assert!(!failed.contains("LEAKED"), "{failed}");
        assert!(
            resolve("true")
                .unwrap_err()
                .to_string()
                .contains("printed nothing")
        );
        let debug = format!(
            "{:?}",
            Credential::Command {
                command: "pass x".into()
            }
        );
        assert_eq!(debug, r#"Command { command: "pass x" }"#);
    }

    #[test]
    fn test_credential_file_missing() {
        let cred = Credential::File {
            file: "/nonexistent/path/pw.txt".into(),
        };
        assert!(cred.resolve("test", None).is_err());
    }

    #[test]
    fn test_validate_multiple_syncs() {
        let toml = r#"
[daemon]
state_dir = "/tmp/capycal"

[accounts.acc]
url = "https://caldav.example.com/"
username = "alice"
password = "p"

[calendars.work]
url = "https://work.example.com/cal/"
account = "acc"

[calendars.personal]
url = "https://personal.example.com/cal/"
account = "acc"

[calendars.work_blockers]
url = "https://work.example.com/blockers/"
account = "acc"

[calendars.personal_blockers]
url = "https://personal.example.com/blockers/"
account = "acc"

[syncs.work-to-personal]
source = "work"
target = "personal_blockers"
interval = "5m"
fields = ["DTSTART", "DTEND"]

transforms = [
  { field = "SUMMARY", action = "set_field", value = "Busy (work)" },
]

[syncs.personal-to-work]
source = "personal"
target = "work_blockers"
interval = "10m"
fields = ["DTSTART", "DTEND"]

transforms = [
  { field = "SUMMARY", action = "set_field", value = "Busy (personal)" },
]
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!(config.validate().is_ok());
        assert_eq!(config.syncs.len(), 2);
        assert_eq!(config.calendars.len(), 4);
    }

    #[test]
    fn test_validate_duplicate_sync_names() {
        let toml = r#"
[daemon]
state_dir = "/tmp/capycal"

[accounts.acc]
url = "https://caldav.example.com/"
username = "u"
password = "p"

[calendars.a]
url = "https://a.example.com/"
account = "acc"

[calendars.b]
url = "https://b.example.com/"
account = "acc"

[syncs.same-name]
source = "a"
target = "b"
interval = "5m"
fields = ["DTSTART"]

[syncs.same-name]
source = "b"
target = "a"
interval = "5m"
fields = ["DTSTART"]
"#;
        let err = toml::from_str::<Config>(toml).unwrap_err().to_string();
        assert!(err.contains("duplicate key"), "{err}");
    }

    #[test]
    fn test_validate_empty_sync_name() {
        let toml = r#"
[daemon]
state_dir = "/tmp/capycal"

[accounts.acc]
url = "https://caldav.example.com/"
username = "u"
password = "p"

[calendars.a]
url = "https://a.example.com/"
account = "acc"

[calendars.b]
url = "https://b.example.com/"
account = "acc"

[syncs.""]
source = "a"
target = "b"
interval = "5m"
fields = ["DTSTART"]
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_validate_set_transform_missing_value() {
        let toml = r#"
[daemon]
state_dir = "/tmp/capycal"

[accounts.acc]
url = "https://caldav.example.com/"
username = "u"
password = "p"

[calendars.a]
url = "https://a.example.com/"
account = "acc"

[calendars.b]
url = "https://b.example.com/"
account = "acc"

[syncs.test]
source = "a"
target = "b"
interval = "1m"
fields = ["DTSTART"]

transforms = [
  { field = "SUMMARY", action = "set_field" },
]
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_validate_regex_transform_missing_replacement() {
        let toml = r#"
[daemon]
state_dir = "/tmp/capycal"

[accounts.acc]
url = "https://caldav.example.com/"
username = "u"
password = "p"

[calendars.a]
url = "https://a.example.com/"
account = "acc"

[calendars.b]
url = "https://b.example.com/"
account = "acc"

[syncs.test]
source = "a"
target = "b"
interval = "1m"
fields = ["DTSTART"]

transforms = [
  { field = "SUMMARY", action = "replace_field_regex", pattern = ".*" },
]
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_validate_invalid_regex() {
        let toml = r#"
[daemon]
state_dir = "/tmp/capycal"

[accounts.acc]
url = "https://caldav.example.com/"
username = "u"
password = "p"

[calendars.a]
url = "https://a.example.com/"
account = "acc"

[calendars.b]
url = "https://b.example.com/"
account = "acc"

[syncs.test]
source = "a"
target = "b"
interval = "1m"
fields = ["DTSTART"]

transforms = [
  { field = "SUMMARY", action = "replace_field_regex", pattern = "[invalid(", replacement = "x" },
]
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_validate_sync_name_leading_dot() {
        let toml = r#"
[daemon]
state_dir = "/tmp/capycal"

[accounts.acc]
url = "https://caldav.example.com/"
username = "u"
password = "p"

[calendars.a]
url = "https://a.example.com/"
account = "acc"

[calendars.b]
url = "https://b.example.com/"
account = "acc"

[syncs.".hidden"]
source = "a"
target = "b"
interval = "1m"
fields = ["DTSTART"]
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_validate_unknown_placeholder() {
        let toml = r#"
[daemon]
state_dir = "/tmp/capycal"

[accounts.acc]
url = "https://caldav.example.com/"
username = "u"
password = "p"

[calendars.a]
url = "https://a.example.com/"
account = "acc"

[calendars.b]
url = "https://b.example.com/"
account = "acc"

[syncs.test]
source = "a"
target = "b"
interval = "1m"
fields = ["DTSTART"]

transforms = [
  { field = "SUMMARY", action = "set_field", value = "Busy ({{personal_name}})" },
]
"#;
        let config: Config = toml::from_str(toml).unwrap();
        let err = config.validate().unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("unknown placeholder"), "error was: {msg}");
        assert!(msg.contains("personal_name"), "error was: {msg}");
    }

    #[test]
    fn test_validate_known_placeholders_ok() {
        let toml = r#"
[daemon]
state_dir = "/tmp/capycal"

[accounts.acc]
url = "https://caldav.example.com/"
username = "u"
password = "p"

[calendars.a]
url = "https://a.example.com/"
account = "acc"

[calendars.b]
url = "https://b.example.com/"
account = "acc"

[syncs.test]
source = "a"
target = "b"
interval = "1m"
fields = ["DTSTART"]

transforms = [
  { field = "SUMMARY", action = "set_field", value = "{{source_name}} via {{sync_name}} ({{source_url}})" },
]
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_parse_duration_formats() {
        let toml = r#"
[daemon]
state_dir = "/tmp/capycal"

[accounts.acc]
url = "https://caldav.example.com/"
username = "u"
password = "p"

[calendars.a]
url = "https://a.example.com/"
account = "acc"

[calendars.b]
url = "https://b.example.com/"
account = "acc"

[syncs.test]
source = "a"
target = "b"
interval = "2h 30m"
fields = ["DTSTART"]
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(
            config.syncs[0].interval,
            Duration::from_secs(2 * 3600 + 30 * 60)
        );
    }

    fn compile_one(transform: &str) -> Result<Transform> {
        let mut sync: SyncConfig = toml::from_str(&format!(
            r#"
source = "a"
target = "b"
interval = "1m"
fields = ["SUMMARY"]

[[transforms]]
{transform}
"#
        ))
        .unwrap();
        sync.name = "test".to_string();
        sync.compile_transforms().map(|mut ts| ts.remove(0))
    }

    fn compile_err(transform: &str) -> String {
        compile_one(transform).unwrap_err().to_string()
    }

    #[test]
    fn test_compile_skip_with_a_time_condition() {
        let t = compile_one(
            r#"action = "skip"
[[transforms.when]]
days = ["sat", "sun"]
start_time = "08:30"
not = true
"#,
        )
        .unwrap();
        assert!(matches!(t.action, Action::Skip));
        let when = t.when.concat();
        let [Condition { test, negate: true }] = &when[..] else {
            panic!("unexpected transform: {t:?}");
        };
        let Test::TimeRange { days, start, end } = test.clone() else {
            panic!("unexpected test: {test:?}");
        };
        assert_eq!(days, vec![Weekday::Sat, Weekday::Sun]);
        assert_eq!(start, NaiveTime::from_hms_opt(8, 30, 0));
        assert_eq!(end, None);
    }

    fn compile_all(fields: &str, transforms: &str) -> Result<Vec<Transform>> {
        let mut sync: SyncConfig = toml::from_str(&format!(
            "source = \"a\"\ntarget = \"b\"\ninterval = \"1m\"\nfields = {fields}\ntransforms = {transforms}\n"
        ))
        .unwrap();
        sync.name = "test".to_string();
        sync.compile_transforms()
    }

    #[test]
    fn test_compile_replace_and_skip_by_field() {
        let t = compile_all(
            r#"["SUMMARY"]"#,
            r#"[{ field = "SUMMARY", action = "replace_field", search = "Zoom", replacement = "Call", count = 1 },
                { action = "skip", when = [{ field = "DESCRIPTION", contains = "private" }] }]"#,
        )
        .unwrap();
        assert!(matches!(
            &t[0].action,
            Action::ReplaceField { count: Some(1), .. }
        ));
        assert!(matches!(&t[1].action, Action::Skip));
        assert!(
            matches!(&t[1].when[0][0].test, Test::Contains { field, .. } if field == "DESCRIPTION")
        );
        for (transform, expected) in [
            (
                r#"action = "replace_field"
field = "SUMMARY"
search = "a""#,
                "requires 'search' and 'replacement'",
            ),
            (
                r#"action = "replace_field"
field = "SUMMARY"
search = ""
replacement = "b""#,
                "non-empty 'search'",
            ),
            (
                r#"action = "replace_field"
field = "SUMMARY"
search = "a"
replacement = "b"
count = 0"#,
                "'count' above 0",
            ),
            (
                r#"action = "skip"
[[transforms.when]]
contains = "a""#,
                "'contains' needs a 'field'",
            ),
            (
                r#"action = "skip"
[[transforms.when]]
field = "SUMMARY"
contains = """#,
                "'contains' needs a text",
            ),
        ] {
            let err = compile_err(transform);
            assert!(err.contains(expected), "{err}");
        }
        let err = compile_all(
            "[]",
            r#"[{ field = "SUMMARY", action = "replace_field", search = "a", replacement = "b" }]"#,
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("replace_field on SUMMARY has no effect"),
            "{err}"
        );
    }

    #[test]
    fn test_compile_matches_condition() {
        // Conditions see the whole event, so the field need not be in fields
        let t = compile_all(
            "[]",
            r#"[{ action = "skip", when = [{ field = "DESCRIPTION", matches = "(?i)private" }] }]"#,
        )
        .unwrap();
        assert!(
            matches!(&t[0].when[0][0].test, Test::Matches { field, .. } if field == "DESCRIPTION")
        );
        let err = compile_err(
            r#"action = "skip"
[[transforms.when]]
matches = "x""#,
        );
        assert!(err.contains("'matches' needs a 'field'"), "{err}");
        let err = compile_err(
            r#"action = "skip"
[[transforms.when]]
field = "SUMMARY"
matches = "(""#,
        );
        assert!(err.contains("invalid regex pattern"), "{err}");
    }

    #[test]
    fn test_any_action_takes_conditions() {
        let t = compile_all(
            "[]",
            r#"[{ action = "add_buffer", pre_event = "45m", when = [
                   { field = "LOCATION", empty = false },
                   { field = "LOCATION", contains = "Zoom", not = true },
                 ] }]"#,
        )
        .unwrap();
        assert!(matches!(t[0].action, Action::AddBuffer { .. }));
        assert_eq!(t[0].when.len(), 2, "two groups of one");
        assert!(matches!(
            &t[0].when.concat()[..],
            [
                Condition {
                    test: Test::Empty { .. },
                    negate: true
                },
                Condition {
                    test: Test::Contains { .. },
                    negate: true
                }
            ]
        ));
    }

    #[test]
    fn test_compile_any_into_one_group() {
        let t = compile_one(
            r#"action = "skip"
when = [
  { any = [{ all_day = true }, { field = "SUMMARY", contains = "OOO" }] },
  { days = ["mon"] },
]"#,
        )
        .unwrap();
        assert_eq!(t.when.len(), 2);
        assert!(matches!(
            &t.when[0][..],
            [
                Condition {
                    test: Test::AllDay,
                    ..
                },
                Condition {
                    test: Test::Contains { .. },
                    ..
                }
            ]
        ));
        assert!(matches!(
            &t.when[1][..],
            [Condition {
                test: Test::TimeRange { .. },
                ..
            }]
        ));
    }

    #[test]
    fn test_compile_rejects_unclear_conditions() {
        for (condition, expected) in [
            ("", "needs one of"),
            (
                r#"field = "SUMMARY"
contains = "a"
matches = "b""#,
                "only one of",
            ),
            (
                r#"field = "SUMMARY"
empty = true
not = true"#,
                "use empty = false",
            ),
            (
                r#"all_day = true
not = true"#,
                "use all_day = false",
            ),
            (r#"start_time = "08:00""#, "needs 'days'"),
            (
                r#"field = "SUMMARY"
all_day = true"#,
                "'field' has no effect",
            ),
            (
                r#"field = "SUMMARY"
contains = "a"
start_time = "09:00""#,
                "'start_time' only goes with 'days'",
            ),
            (
                r#"all_day = true
end_time = "09:00""#,
                "'end_time' only goes with 'days'",
            ),
            ("days = []", "needs at least one day"),
            (
                r#"field = "SUMMARY"
matches = """#,
                "needs a regex",
            ),
            (
                r#"field = "summary"
contains = "a""#,
                "upper case: SUMMARY",
            ),
        ] {
            let err = compile_err(&format!(
                "action = \"skip\"\n[[transforms.when]]\n{condition}"
            ));
            assert!(err.contains(expected), "{condition}: {err}");
        }
        for (condition, expected) in [
            (
                r#"{ any = [{ all_day = true }] }"#,
                "'any' needs at least two conditions",
            ),
            (
                r#"{ all_day = true, any = [{ all_day = true }, { days = ["sat"] }] }"#,
                "takes no other keys",
            ),
            (
                r#"{ any = [{ all_day = true }, { any = [{ days = ["sat"] }, { days = ["sun"] }] }] }"#,
                "cannot hold another 'any'",
            ),
            (
                r#"{ any = [{ all_day = true }, { field = "SUMMARY" }] }"#,
                "needs one of",
            ),
        ] {
            let err = compile_err(&format!("action = \"skip\"\nwhen = [{condition}]"));
            assert!(err.contains(expected), "{condition}: {err}");
        }
        let err = compile_err(r#"action = "skip""#);
        assert!(err.contains("would skip every event"), "{err}");
        for action in [
            "set_field",
            "replace_field",
            "replace_field_regex",
            "remove_field",
        ] {
            let err = compile_err(&format!(
                "action = \"{action}\"\nvalue = \"Busy\"\nsearch = \"a\"\npattern = \"a\"\nreplacement = \"b\""
            ));
            assert!(err.contains("needs a 'field'"), "{action}: {err}");
        }
    }

    #[test]
    fn test_a_condition_on_a_value_set_above_is_rejected() {
        let busy = r#"{ field = "SUMMARY", action = "set_field", value = "Busy" }"#;
        let skip = r#"{ action = "skip", when = [{ field = "SUMMARY", contains = "Lunch" }] }"#;
        let err = compile_all("[]", &format!("[{busy}, {skip}]"))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("condition on SUMMARY in transform 2 always sees what transform 1 left"),
            "{err}"
        );
        // A removed property is always empty
        let remove = r#"{ field = "SUMMARY", action = "remove_field" }"#;
        assert!(compile_all("[]", &format!("[{remove}, {skip}]")).is_err());
        assert!(compile_all("[]", &format!("[{skip}, {busy}]")).is_ok());
        // A set_field with its own conditions leaves other values possible
        let maybe_busy = r#"{ field = "SUMMARY", action = "set_field", value = "Busy", when = [{ all_day = true }] }"#;
        assert!(compile_all("[]", &format!("[{maybe_busy}, {skip}]")).is_ok());
        // Also inside a group
        let either = r#"{ action = "skip", when = [{ any = [{ all_day = true }, { field = "SUMMARY", contains = "Lunch" }] }] }"#;
        assert!(compile_all("[]", &format!("[{busy}, {either}]")).is_err());
    }

    #[test]
    fn test_old_action_names_name_their_new_name() {
        for (old, new) in [
            ("set", "set_field"),
            ("regex_replace", "replace_field_regex"),
            ("remove", "remove_field"),
        ] {
            let err = compile_err(&format!("action = \"{old}\"\nfield = \"SUMMARY\""));
            assert!(
                err.contains(&format!("'{old}' was renamed to '{new}'")),
                "{err}"
            );
        }
        for old in [
            "filter_all_day",
            "filter_time_range",
            "exclude_all_day",
            "exclude_time_range",
            "exclude_field",
            "exclude_field_regex",
        ] {
            let err = compile_err(&format!("action = \"{old}\""));
            assert!(
                err.contains(&format!("'{old}' was replaced by action = \"skip\"")),
                "{err}"
            );
        }
    }

    #[test]
    fn test_old_time_range_keys_point_to_skip() {
        let toml = "[syncs.s]\nsource = \"a\"\ntarget = \"b\"\n\
            transforms = [{ action = \"exclude_time_range\", days = [\"sat\"] }]\n";
        let err = parse(toml).unwrap_err();
        assert!(err.contains("unknown field `days`"), "{err}");
        assert!(
            err.contains("days, start_time and end_time moved into a condition"),
            "{err}"
        );
    }

    #[test]
    fn test_regex_replace_needs_its_field() {
        let replace = r#"{ field = "SUMMARY", action = "replace_field_regex", pattern = "a", replacement = "b" }"#;
        let set = r#"{ field = "SUMMARY", action = "set_field", value = "Busy" }"#;
        let remove = r#"{ field = "SUMMARY", action = "remove_field" }"#;
        let err = compile_all("[]", &format!("[{replace}]"))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains(
                "replace_field_regex on SUMMARY has no effect because SUMMARY is not in fields"
            ),
            "{err}"
        );
        assert!(err.contains("use set_field"), "{err}");
        assert!(compile_all(r#"["SUMMARY"]"#, &format!("[{replace}]")).is_ok());
        // A field set before is there to replace in
        assert!(compile_all("[]", &format!("[{set}, {replace}]")).is_ok());
        assert!(compile_all(r#"["SUMMARY"]"#, &format!("[{remove}, {replace}]")).is_err());
        // Structural fields are always kept
        let status = r#"{ field = "STATUS", action = "replace_field_regex", pattern = "a", replacement = "b" }"#;
        assert!(compile_all("[]", &format!("[{status}]")).is_ok());
    }

    #[test]
    fn test_compile_regex_replace() {
        let t = compile_one(
            r#"field = "SUMMARY"
action = "replace_field_regex"
pattern = "^(.*)$"
replacement = "[$1]"
"#,
        )
        .unwrap();
        let Action::RegexReplace {
            field,
            regex,
            replacement,
        } = t.action
        else {
            panic!("unexpected transform: {t:?}");
        };
        assert_eq!(field, "SUMMARY");
        assert_eq!(regex.as_str(), "^(.*)$");
        assert_eq!(replacement, "[$1]");
    }

    #[test]
    fn test_compile_rejects_invalid_weekday() {
        let msg = compile_err(
            r#"action = "skip"
[[transforms.when]]
days = ["mon", "monday"]
"#,
        );
        assert!(msg.contains("invalid day 'monday'"), "error was: {msg}");
        assert!(msg.contains("sync 'test'"), "error was: {msg}");
    }

    #[test]
    fn test_compile_rejects_invalid_times() {
        for (time, expected) in [
            ("8", "expected HH:MM"),
            ("xx:00", "invalid hour"),
            ("08:yy", "invalid minute"),
            ("24:00", "out of range"),
            ("12:60", "out of range"),
        ] {
            let msg = compile_err(&format!(
                "action = \"skip\"\n[[transforms.when]]\ndays = [\"mon\"]\nend_time = \"{time}\"\n"
            ));
            assert!(msg.contains(expected), "{time}: error was: {msg}");
        }
    }

    #[test]
    fn test_compile_rejects_missing_required_fields() {
        for (transform, expected) in [
            (r#"action = "add_alarm""#, "requires 'trigger'"),
            (r#"action = "add_buffer""#, "'pre_event' or 'post_event'"),
        ] {
            let msg = compile_err(transform);
            assert!(msg.contains(expected), "{transform}: error was: {msg}");
        }
    }

    const TWO_CALENDARS: &str = r#"
[accounts.acc]
url = "https://caldav.example.com/"
username = "u"
password = "p"

[calendars.a]
url = "https://a.example.com/"
account = "acc"

[calendars.b]
url = "https://b.example.com/"
account = "acc"
"#;

    const SYNC: &str = r#"
[syncs.test]
source = "a"
target = "b"
interval = "1m"
fields = ["DTSTART"]
"#;

    fn parse_err(toml: &str) -> String {
        toml::from_str::<Config>(toml).unwrap_err().to_string()
    }

    #[test]
    fn test_daemon_table_is_optional() {
        let config: Config = toml::from_str(&format!("{TWO_CALENDARS}{SYNC}")).unwrap();
        assert_eq!(config.daemon.log_level, "info");
        assert_eq!(config.daemon.log_format, LogFormat::Pretty);
        assert!(config.daemon.state_dir.is_none());
        assert!(!config.daemon.watch_config);
        assert!(config.validate().is_ok());

        let config: Config = toml::from_str("[daemon]\nlog_format = \"json\"\n").unwrap();
        assert_eq!(config.daemon.log_level, "info");
        assert_eq!(config.daemon.log_format, LogFormat::Json);
    }

    #[test]
    fn test_config_without_syncs_is_valid() {
        let config: Config = toml::from_str("").unwrap();
        assert!(config.syncs.is_empty() && config.calendars.is_empty());
        assert!(config.validate().is_ok());

        let config: Config = toml::from_str(TWO_CALENDARS).unwrap();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_unknown_keys_are_rejected_at_every_level() {
        for (toml, key) in [
            (format!("verbose = true\n{TWO_CALENDARS}"), "verbose"),
            (
                format!("[daemon]\nlog_levl = \"debug\"\n{TWO_CALENDARS}"),
                "log_levl",
            ),
            (format!("{TWO_CALENDARS}concurency = 2\n"), "concurency"),
            (
                format!("{TWO_CALENDARS}{}", SYNC.replace("interval", "intervl")),
                "intervl",
            ),
            (
                format!(
                    "{TWO_CALENDARS}{SYNC}[[syncs.test.transforms]]\naction = \"remove_field\"\nfeild = \"X\"\n"
                ),
                "feild",
            ),
            (
                format!(
                    "{TWO_CALENDARS}[accounts.c]\nurl = \"x\"\nusername = \"u\"\npassword = {{ env = \"X\", fiel = \"y\" }}\n"
                ),
                "fiel",
            ),
        ] {
            let msg = parse_err(&toml);
            assert!(
                msg.contains(&format!("unknown field `{key}`")),
                "{key}: {msg}"
            );
            assert!(msg.contains("line "), "{key}: {msg}");
        }
    }

    #[test]
    fn test_syncs_are_named_tables_in_file_order() {
        let config: Config = toml::from_str(&format!(
            r#"{TWO_CALENDARS}
[syncs.zeta]
source = "a"
target = "b"
interval = "1m"
fields = ["DTSTART"]
transforms = [
  # a comment
  {{ field = "SUMMARY", action = "set_field", value = "Busy" }},
  {{ action = "skip", when = [{{ all_day = true }}] }},
]

[syncs."b to a"]
source = "b"
target = "a"
interval = "1m"
fields = ["DTSTART"]

[[syncs."b to a".transforms]]
action = "add_alarm"
trigger = "5m"
"#
        ))
        .unwrap();
        let names: Vec<&str> = config.syncs.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["zeta", "b to a"]);
        assert_eq!(config.syncs[0].compile_transforms().unwrap().len(), 2);
        assert_eq!(config.syncs[1].compile_transforms().unwrap().len(), 1);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_old_sync_array_explains_the_new_format() {
        let msg = parse_err(&format!(
            "{TWO_CALENDARS}[[syncs]]\nname = \"x\"\nsource = \"a\"\n"
        ));
        assert!(msg.contains("replace `[[syncs]]`"), "{msg}");
    }

    #[test]
    fn test_typo_names_the_key_and_the_expected_ones() {
        let msg = parse_err(&format!(
            "{TWO_CALENDARS}{}",
            SYNC.replace("interval", "intervl")
        ));
        assert!(msg.contains("line 18, column 1"), "{msg}");
        assert!(msg.contains("`interval`"), "{msg}");
    }

    #[test]
    fn test_credential_keychain() {
        crate::keychain::use_test_store();
        keyring_core::Entry::new(crate::keychain::SERVICE, "test-account")
            .unwrap()
            .set_password("kc-secret")
            .unwrap();
        let cred = Credential::Keychain {
            keychain: "test-account".to_string(),
        };
        assert_eq!(cred.resolve("account 'a'", None).unwrap(), "kc-secret");
        assert_eq!(
            format!("{cred:?}"),
            r#"Keychain { keychain: "test-account" }"#
        );

        let missing = Credential::Keychain {
            keychain: "test-none".to_string(),
        };
        let err = missing
            .resolve("account 'a'", None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("account 'a'"), "{err}");
        assert!(
            err.contains("keychain entry 'test-none' not found"),
            "{err}"
        );
    }

    #[test]
    fn test_keychain_unavailable_suggests_env_or_file() {
        let msg = super::keychain_error("x", keyring_core::Error::NoDefaultStore);
        assert!(msg.contains("env"), "{msg}");
        assert!(msg.contains("file"), "{msg}");
    }

    #[test]
    fn test_credential_forms() {
        #[derive(Deserialize)]
        struct C {
            password: Credential,
        }
        let parse = |s: &str| toml::from_str::<C>(s).map(|c| c.password);

        assert!(matches!(parse(r#"password = "x""#).unwrap(), Credential::Plain(p) if p == "x"));
        assert!(
            matches!(parse(r#"password = { env = "V" }"#).unwrap(), Credential::Env { env } if env == "V")
        );
        assert!(
            matches!(parse(r#"password = { file = "f" }"#).unwrap(), Credential::File { file } if file == Path::new("f"))
        );
        assert!(
            matches!(parse(r#"password = { command = "pass show x" }"#).unwrap(), Credential::Command { command } if command == "pass show x")
        );
        assert!(
            matches!(parse(r#"password = { keychain = "nextcloud" }"#).unwrap(), Credential::Keychain { keychain } if keychain == "nextcloud")
        );
        for (bad, expected) in [
            (
                r#"password = { env = "V", file = "f" }"#,
                "exactly one of `env`, `file`, `command` or `keychain`",
            ),
            (
                r#"password = {}"#,
                "exactly one of `env`, `file`, `command` or `keychain`",
            ),
            (r#"password = { var = "V" }"#, "unknown field `var`"),
            (r#"password = 1"#, "a password string"),
        ] {
            let msg = parse(bad).map(|_| ()).unwrap_err().to_string();
            assert!(msg.contains(expected), "{bad}: {msg}");
        }
    }

    #[test]
    fn test_eventkit_window_over_four_years_is_rejected() {
        let config = |options: &str| -> Config {
            toml::from_str(&format!(
                r#"
[accounts.acc]
url = "https://caldav.example.com/"
username = "u"
password = "p"

[calendars.a]
eventkit_calendar = "Work"

[calendars.b]
url = "https://b.example.com/"
account = "acc"
{SYNC}{options}"#
            ))
            .unwrap()
        };

        let msg = config("eventkit_days_past = 1000\neventkit_days_future = 461\n")
            .validate()
            .unwrap_err()
            .to_string();
        assert!(msg.contains("1461 days"), "{msg}");
        assert!(msg.contains("eventkit_all_events = true"), "{msg}");

        // Within the limit or chunked, the check passes (on Linux the
        // EventKit calendar itself is rejected afterwards)
        for options in [
            "eventkit_days_past = 1000\neventkit_days_future = 460\n",
            "eventkit_days_past = 5000\neventkit_all_events = true\n",
        ] {
            let result = config(options).validate();
            if cfg!(target_os = "macos") {
                assert!(result.is_ok(), "{options}: {result:?}");
            } else {
                let msg = result.unwrap_err().to_string();
                assert!(msg.contains("only supported on macOS"), "{options}: {msg}");
            }
        }

        // The window of a CalDAV source does not matter
        let caldav: Config =
            toml::from_str(&format!("{TWO_CALENDARS}{SYNC}eventkit_days_past = 5000\n")).unwrap();
        assert!(caldav.validate().is_ok());
    }

    #[test]
    fn test_full_sync_interval() {
        let config: Config = toml::from_str(&format!("{TWO_CALENDARS}{SYNC}")).unwrap();
        assert_eq!(
            config.syncs[0].full_sync_interval,
            Duration::from_secs(7 * 86400)
        );

        let config: Config = toml::from_str(&format!(
            "{TWO_CALENDARS}{SYNC}full_sync_interval = \"14d\"\n"
        ))
        .unwrap();
        assert_eq!(
            config.syncs[0].full_sync_interval,
            Duration::from_secs(14 * 86400)
        );
        assert!(config.validate().is_ok());

        let config: Config = toml::from_str(&format!(
            "{TWO_CALENDARS}{SYNC}full_sync_interval = \"0s\"\n"
        ))
        .unwrap();
        let msg = config.validate().unwrap_err().to_string();
        assert!(msg.contains("full_sync_interval must be positive"), "{msg}");

        let msg = parse_err(&format!(
            "{TWO_CALENDARS}{SYNC}full_sync_interval = \"monthly\"\n"
        ));
        assert!(msg.contains("full_sync_interval"), "{msg}");
    }

    #[test]
    fn test_state_dir_precedence() {
        let mut config: Config = toml::from_str("").unwrap();
        let env = |v: &str| Some(std::ffi::OsString::from(v));

        assert_eq!(
            config.state_dir_with_env(env("/env")).unwrap(),
            PathBuf::from("/env")
        );
        assert_eq!(config.state_dir_with_env(None).ok(), default_state_dir(),);
        assert_eq!(config.state_dir_with_env(env("")).ok(), default_state_dir());

        config.daemon.state_dir = Some(PathBuf::from("/config"));
        assert_eq!(
            config.state_dir_with_env(env("/env")).unwrap(),
            PathBuf::from("/config")
        );
    }

    const ACCOUNT: &str = r#"
[accounts.acc]
url = "https://caldav.example.com/"
username = "alice"
password = "secret"
"#;

    fn config_with(calendars: &str) -> Config {
        toml::from_str(&format!(
            "{ACCOUNT}\n{calendars}\n[syncs.s]\nsource = \"a\"\ntarget = \"b\"\ninterval = \"5m\"\n"
        ))
        .unwrap()
    }

    #[test]
    fn test_caldav_calendar_requires_a_known_account() {
        let ok = config_with(
            "[calendars.a]\naccount = \"acc\"\nurl = \"https://x/a/\"\n\
             [calendars.b]\naccount = \"acc\"\nurl = \"https://x/b/\"\n",
        );
        ok.validate().unwrap();

        let missing = config_with(
            "[calendars.a]\nurl = \"https://x/a/\"\n\
             [calendars.b]\naccount = \"acc\"\nurl = \"https://x/b/\"\n",
        );
        let err = missing.validate().unwrap_err().to_string();
        assert!(err.contains("calendar 'a': requires 'account'"), "{err}");

        let unknown = config_with(
            "[calendars.a]\naccount = \"nope\"\nurl = \"https://x/a/\"\n\
             [calendars.b]\naccount = \"acc\"\nurl = \"https://x/b/\"\n",
        );
        let err = unknown.validate().unwrap_err().to_string();
        assert!(
            err.contains("calendar 'a' references unknown account 'nope'"),
            "{err}"
        );
    }

    #[test]
    fn test_eventkit_calendar_takes_no_account() {
        let config = config_with(
            "[calendars.a]\naccount = \"acc\"\neventkit_calendar = \"Home\"\n\
             [calendars.b]\naccount = \"acc\"\nurl = \"https://x/b/\"\n",
        );
        let err = config.validate().unwrap_err().to_string();
        assert!(
            err.contains("calendar 'a': 'account' is only for CalDAV calendars"),
            "{err}"
        );
    }

    #[test]
    fn test_ics_calendar_is_a_source_only() {
        let feed = "[calendars.a]\nics_url = \"https://x/basic.ics\"\n\
                    [calendars.b]\naccount = \"acc\"\nurl = \"https://x/b/\"\n";
        let config = config_with(feed);
        config.validate().unwrap();
        assert!(config.calendars["a"].is_ics());
        assert_eq!(
            format!("{:?}", config.calendars["a"].ics_url),
            "Some(Plain(***))"
        );

        let secret =
            config_with(&feed.replace("\"https://x/basic.ics\"", "{ keychain = \"google\" }"));
        secret.validate().unwrap();

        let target = config_with(
            "[calendars.a]\naccount = \"acc\"\nurl = \"https://x/a/\"\n\
             [calendars.b]\nics_url = \"https://x/basic.ics\"\n",
        );
        let err = target.validate().unwrap_err().to_string();
        assert!(
            err.contains("calendar 'b' is a read-only feed (ics_url) and cannot be a target"),
            "{err}"
        );

        let mixed = config_with(&feed.replace("ics_url", "account = \"acc\"\nics_url"));
        let err = mixed.validate().unwrap_err().to_string();
        assert!(
            err.contains("calendar 'a': ics_url takes no 'account', 'url' or 'eventkit_calendar'"),
            "{err}"
        );
    }

    #[test]
    fn test_enabled_defaults_to_true() {
        let config = config_with(
            "[calendars.a]\naccount = \"acc\"\nurl = \"https://x/a/\"\n\
             [calendars.b]\naccount = \"acc\"\nurl = \"https://x/b/\"\n",
        );
        assert!(config.syncs[0].enabled);
        let sync: SyncConfig =
            toml::from_str("source = \"a\"\ntarget = \"b\"\ninterval = \"5m\"\nenabled = false\n")
                .unwrap();
        assert!(!sync.enabled);
    }

    #[test]
    fn test_output_key_follows_what_shapes_the_copies() {
        let sync = |extra: &str| -> SyncConfig {
            toml::from_str(&format!(
                "source = \"a\"\ntarget = \"b\"\ninterval = \"5m\"\nfields = [\"SUMMARY\", \"LOCATION\"]\n{extra}"
            ))
            .unwrap()
        };
        let cal = |url: &str| -> CalendarConfig {
            toml::from_str(&format!("account = \"acc\"\nurl = \"{url}\"")).unwrap()
        };
        let base = sync("").output_key(&cal("https://x/a/"), &cal("https://x/b/"));

        assert_eq!(
            base,
            sync("").output_key(&cal("https://x/a/"), &cal("https://x/b/"))
        );
        // Not part of the output
        let mut slower = sync("");
        slower.interval = Duration::from_secs(3600);
        assert_eq!(
            base,
            slower.output_key(&cal("https://x/a/"), &cal("https://x/b/"))
        );
        assert_eq!(
            base,
            sync("full_sync_interval = \"14d\"")
                .output_key(&cal("https://x/a/"), &cal("https://x/b/"))
        );
        // Order of fields does not matter
        let reordered: SyncConfig = toml::from_str(
            "source = \"a\"\ntarget = \"b\"\ninterval = \"5m\"\nfields = [\"LOCATION\", \"SUMMARY\"]\n",
        )
        .unwrap();
        assert_eq!(
            base,
            reordered.output_key(&cal("https://x/a/"), &cal("https://x/b/"))
        );

        let mut retargeted = sync("");
        retargeted.target = "c".to_string();
        for changed in [
            sync("").output_key(&cal("https://x/a/"), &cal("https://x/other-target/")),
            retargeted.output_key(&cal("https://x/a/"), &cal("https://x/b/")),
            sync("").output_key(&cal("https://x/other/"), &cal("https://x/b/")),
            sync("eventkit_days_past = 7").output_key(&cal("https://x/a/"), &cal("https://x/b/")),
            sync(
                "transforms = [{ field = \"SUMMARY\", action = \"set_field\", value = \"Busy\" }]",
            )
            .output_key(&cal("https://x/a/"), &cal("https://x/b/")),
            sync(
                "transforms = [{ field = \"SUMMARY\", action = \"set_field\", value = \"Busy\", \
                 when = [{ field = \"LOCATION\", empty = true }] }]",
            )
            .output_key(&cal("https://x/a/"), &cal("https://x/b/")),
        ] {
            assert_ne!(base, changed);
        }
        // The same two conditions, once both needed, once either
        let both = sync(
            "transforms = [{ action = \"skip\", when = [{ all_day = true }, { days = [\"sat\"] }] }]",
        )
        .output_key(&cal("https://x/a/"), &cal("https://x/b/"));
        let either = sync(
            "transforms = [{ action = \"skip\", when = [{ any = [{ all_day = true }, { days = [\"sat\"] }] }] }]",
        )
        .output_key(&cal("https://x/a/"), &cal("https://x/b/"));
        assert_ne!(both, either);

        let feed = |url: &str| -> CalendarConfig {
            toml::from_str(&format!("ics_url = \"{url}\"")).unwrap()
        };
        let from_feed = sync("").output_key(&feed("https://x/a.ics"), &cal("https://x/b/"));
        assert_ne!(base, from_feed);
        assert_ne!(
            from_feed,
            sync("").output_key(&feed("https://x/new.ics"), &cal("https://x/b/"))
        );
    }

    #[test]
    fn test_calendar_credentials_are_rejected() {
        let err = toml::from_str::<Config>(
            "[calendars.a]\nurl = \"https://x/a/\"\nusername = \"u\"\npassword = \"p\"\n",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("unknown field `username`"), "{err}");
    }

    #[test]
    fn test_account_fields_are_required() {
        let err =
            toml::from_str::<Config>("[accounts.acc]\nurl = \"https://x/\"\nusername = \"u\"\n")
                .unwrap_err()
                .to_string();
        assert!(err.contains("missing field `password`"), "{err}");
    }

    #[test]
    fn test_transform_action_names_round_trip() {
        for action in [
            TransformAction::SetField,
            TransformAction::ReplaceField,
            TransformAction::ReplaceFieldRegex,
            TransformAction::RemoveField,
            TransformAction::AddBuffer,
            TransformAction::AddAlarm,
            TransformAction::Skip,
        ] {
            let parsed: TransformConfig =
                toml::from_str(&format!("action = \"{}\"", action.as_str())).unwrap();
            assert_eq!(parsed.action, action);
        }
    }

    #[test]
    fn test_new_sync_and_transform_use_the_defaults() {
        let sync = SyncConfig::new("s", "a", "b");
        let parsed: SyncConfig =
            toml::from_str("source = \"a\"\ntarget = \"b\"\ninterval = \"5m\"\n").unwrap();
        assert_eq!(sync.name, "s");
        assert_eq!(sync.interval, parsed.interval);
        assert_eq!(sync.full_sync_interval, parsed.full_sync_interval);
        assert_eq!(sync.eventkit_days_past, parsed.eventkit_days_past);
        assert_eq!(sync.eventkit_days_future, parsed.eventkit_days_future);
        assert!(sync.enabled && sync.fields.is_empty() && sync.transforms.is_empty());

        let t = TransformConfig::new(TransformAction::AddAlarm);
        assert_eq!(t.action, TransformAction::AddAlarm);
        assert!(t.field.is_empty() && t.trigger.is_none() && t.when.is_empty());
    }

    #[test]
    fn test_calendar_key_from_display_names() {
        assert_eq!(calendar_key("Team Work"), "team_work");
        assert_eq!(calendar_key("Büro / Termine!"), "büro_termine");
        assert_eq!(calendar_key("***"), "calendar");
    }

    #[test]
    fn test_output_key_is_pinned() {
        let mut sync = SyncConfig::new("s", "a", "b");
        sync.fields = vec!["SUMMARY".into()];
        let mut t = TransformConfig::new(TransformAction::AddBuffer);
        t.pre_event = Some(Duration::from_secs(900));
        sync.transforms = vec![t];
        let cal = |url: &str| -> CalendarConfig {
            toml::from_str(&format!("account = \"acc\"\nurl = \"{url}\"")).unwrap()
        };
        // A changed key full-syncs every sync after an upgrade: change the
        // key's text only on purpose
        assert_eq!(
            sync.output_key(&cal("https://x/a/"), &cal("https://x/b/")),
            "9a25d1f7-ed04-5ff8-b2c7-fdd5da11ac83"
        );
        sync.transforms[0].when = vec![ConditionConfig {
            field: "LOCATION".into(),
            contains: Some("Zoom".into()),
            not: true,
            ..Default::default()
        }];
        assert_eq!(
            sync.output_key(&cal("https://x/a/"), &cal("https://x/b/")),
            "06f6f50a-335e-5193-b278-2fa1a54116b6"
        );
    }
}
