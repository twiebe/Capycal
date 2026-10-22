//! capycal sync engine, shared by the `capycal` CLI and other front ends.

mod caldav;
pub mod cli;
mod config;
mod config_edit;
mod config_watch;
mod error;
mod eventkit;
mod ical;
mod ics;
mod keychain;
mod logging;
mod source;
mod state;
mod sync;

pub use caldav::client::DiscoveredCalendar;
pub use cli::discover::discover_caldav_calendars;
pub use cli::generate::STARTER_CONFIG;
pub use config::{
    AccountConfig, CalendarConfig, ConditionConfig, Config, Credential, DEFAULT_CONCURRENCY,
    SyncConfig, TransformAction, TransformConfig, calendar_key, default_config_path,
};
pub use config_edit::{ConfigEditor, duration_text};
pub use config_watch::ConfigWatcher;
pub use error::{Error, Result};
pub use eventkit::{CalendarInfo, list_calendars as list_eventkit_calendars};
pub use keychain::{
    delete_password as delete_keychain_password, password as keychain_password,
    rename_entry as rename_keychain_entry, set_password as set_keychain_password,
    would_prompt as keychain_would_prompt,
};
pub use logging::{
    DEFAULT_LOG_CAPACITY, LOG_FILE_MAX_SIZE, LOG_FILES_KEPT, LogBuffer, LogFile, LogLevel, LogLine,
    default_log_file, init_with_buffer,
};
pub use state::lock::{ConfigLock, LockHolder, lock_holder};
pub use sync::coordinator::Coordinator;
pub use sync::maintenance::{
    count_calendar_events, count_synced_events, forget_sync_state, next_source_event,
    purge_calendar, purge_sync,
};
pub use sync::status::{CycleStats, FullSyncProgress, SyncStatus};
