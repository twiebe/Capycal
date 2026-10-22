use std::path::PathBuf;

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("configuration error: {0}")]
    Config(String),

    #[error("credential resolution failed for {context}: {source}")]
    Credential {
        context: String,
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    #[error("CalDAV request failed: {0}")]
    CalDav(String),

    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("feed request failed: {0}")]
    Feed(String),

    #[error("EventKit error: {0}")]
    EventKit(String),

    #[error("iCal parsing error: {0}")]
    ICalParse(String),

    #[error("state persistence error for {path}: {source}")]
    State {
        path: PathBuf,
        source: std::io::Error,
    },

    /// The events of a sink could not be read (purge, full sync).
    #[error("error {action}: {source}")]
    SinkEvents {
        action: &'static str,
        source: Box<Error>,
    },

    #[error("sync-token expired, full resync required")]
    SyncTokenExpired,

    #[error("sync cycle cancelled by shutdown")]
    Cancelled,

    #[error("logging setup failed: {0}")]
    Logging(String),

    #[error("cannot watch {}: {source}", path.display())]
    Watch {
        path: PathBuf,
        source: notify::Error,
    },

    #[error("config {} is already in use by another Capycal process (pid {pid})", config.display())]
    Locked { config: PathBuf, pid: String },

    #[error(
        "sync '{sync}' is also run by config {other_config} (pid {pid}) with the same state \
         directory; rename the sync or use a different state_dir"
    )]
    StateCollision {
        sync: String,
        other_config: String,
        pid: String,
    },
}

pub type Result<T> = std::result::Result<T, Error>;
