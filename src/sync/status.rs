use chrono::{DateTime, Utc};

/// Event counts of one successful sync cycle.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CycleStats {
    pub created: usize,
    pub updated: usize,
    pub deleted: usize,
    /// Events that failed to push or delete; the cycle itself still succeeded.
    pub errors: usize,
    /// Synced events gone from the source whose removal waits for the next
    /// cycle, because they were most of the synced events.
    pub deferred: usize,
}

/// How far a running full sync got: `done` of `total` changes written to the
/// sink. `total` is unknown while source and sink are read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FullSyncProgress {
    pub done: usize,
    pub total: Option<usize>,
}

/// What a front end can show about one sync.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncStatus {
    pub name: String,
    pub running: bool,
    /// Start time of the last finished cycle.
    pub last_run: Option<DateTime<Utc>>,
    pub last_result: Option<Result<CycleStats, String>>,
    /// Stopped by `Coordinator::pause` until resumed.
    pub paused: bool,
    /// `enabled = false` in the config: the sync has no runner.
    pub disabled: bool,
    /// Set while the running cycle is a full sync.
    pub full_sync: Option<FullSyncProgress>,
    /// Full syncs finished since the sync started, successful or not.
    pub full_syncs: u64,
}

impl SyncStatus {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            running: false,
            last_run: None,
            last_result: None,
            paused: false,
            disabled: false,
            full_sync: None,
            full_syncs: 0,
        }
    }
}

/// Port through which a running sync publishes its status.
pub trait StatusReporter: Send + Sync {
    fn report(&self, status: &SyncStatus);
}
