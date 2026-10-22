pub mod lock;
pub mod mapping;
pub mod store;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::state::mapping::UidMapping;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SyncState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ctag: Option<String>,
    pub uid_mapping: UidMapping,
    /// Source hrefs whose sink delete failed. Sources with a change log
    /// (RFC 6578) report a removal only once, so the runner retries these.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_removals: Vec<String>,
    /// Start of the last completed full sync. None in a state saved before
    /// full syncs existed; the Unix epoch for a sync that never had one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_full_sync: Option<DateTime<Utc>>,
    /// A full sync started but did not complete (cancelled or failed), so
    /// the next cycle is a full sync again even if it was only requested.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub full_sync_pending: bool,
    /// The last cycle found most synced events gone from the source and
    /// waited; the next cycle that finds them gone again removes them.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub removals_deferred: bool,
    /// `RunnerSettings::output_key` of the last completed full sync. A
    /// different key makes the next cycle a full sync.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_key: Option<String>,
    /// The sink's sync token after this sync's last writes. Changes to its
    /// events after it were made elsewhere (another device running the same
    /// sync, or by hand) and make the next cycle a full sync.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sink_token: Option<String>,
}

/// Port for persisting per-sync state. The state is a cache: a sync without
/// one rebuilds it from the markers in its sink.
pub trait StateStore: Send + Sync {
    /// Load the state for a sync. None if none was saved yet, or if it could
    /// not be read and was moved aside.
    fn load(&self, sync_name: &str) -> Result<Option<SyncState>>;

    fn save(&self, sync_name: &str, state: &SyncState) -> Result<()>;

    /// Forget the state for a sync. Succeeds if there was none.
    fn remove(&self, sync_name: &str) -> Result<()>;
}
