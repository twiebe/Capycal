use crate::error::Result;
use crate::state::SyncState;

/// A calendar event from a source, in iCal format.
#[derive(Debug, Clone)]
pub struct SourceEvent {
    /// Event UID (from iCal or source-specific identifier)
    #[allow(dead_code)]
    pub uid: String,
    /// Unique resource identifier (CalDAV href or EventKit calendarItemIdentifier)
    pub href: String,
    /// Change token (CalDAV etag or lastModifiedDate hash)
    pub etag: String,
    /// Full iCal VCALENDAR data
    pub calendar_data: String,
}

/// Trait for reading events from a calendar source.
pub trait EventSource: Send + Sync {
    /// Fetch changed and removed events since last sync.
    /// Returns (changed_events, removed_hrefs).
    fn fetch_changes(
        &self,
        state: &mut SyncState,
    ) -> impl std::future::Future<Output = Result<(Vec<SourceEvent>, Vec<String>)>> + Send;
}

/// Result of a put_event operation.
pub struct PutResult {
    /// Change token (CalDAV etag or lastModifiedDate)
    #[allow(dead_code)]
    pub etag: String,
    /// Actual identifier for this event in the sink. For CalDAV this is the
    /// same href that was passed in. For EventKit it is the calendarItemIdentifier
    /// assigned by the system, which may differ from the requested href.
    pub href: String,
}

/// An event a sync created in its sink, found by its marker. `S` is what the
/// sink keeps of the stored event to tell whether a put would change it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SinkEvent<S> {
    /// Identifier for put and delete (CalDAV href or EventKit calendarItemIdentifier).
    pub href: String,
    /// The event's UID in the sink, if the sink keeps it (CalDAV).
    pub uid: Option<String>,
    /// UID of the source event, None for a legacy EventKit marker.
    pub source_uid: Option<String>,
    /// The event as stored (CalDAV: its iCal, EventKit: the fields a put sets).
    pub stored: S,
}

/// What changed in a sink since a token, see `EventSink::changes_since`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SinkChanges {
    /// Hrefs of events changed or removed since the token.
    pub hrefs: Vec<String>,
    /// The token to ask with next time.
    pub token: String,
}

/// Trait for writing events to a calendar sink.
pub trait EventSink: Send + Sync {
    /// What `list_synced` captures of a stored event, see `is_unchanged`.
    type Stored: Send;

    /// Create or update an event. Returns etag and actual href.
    fn put_event(
        &self,
        href: &str,
        ical_data: &str,
    ) -> impl std::future::Future<Output = Result<PutResult>> + Send;

    /// Delete an event by its href.
    fn delete_event(&self, href: &str) -> impl std::future::Future<Output = Result<()>> + Send;

    /// The events `sync_name` created in this sink. `source_name` is the sync's
    /// source calendar, which legacy EventKit markers name instead of the sync.
    fn list_synced(
        &self,
        sync_name: &str,
        source_name: &str,
    ) -> impl std::future::Future<Output = Result<Vec<SinkEvent<Self::Stored>>>> + Send;

    /// Whether putting `ical_data` would leave the `stored` event as it is.
    fn is_unchanged(&self, stored: &Self::Stored, ical_data: &str) -> bool;

    /// The events changed or removed in the sink since `token`, by anyone
    /// (all events without a token). None if the sink cannot tell; a token
    /// the sink no longer accepts is `Error::SyncTokenExpired`.
    fn changes_since(
        &self,
        _token: Option<&str>,
    ) -> impl std::future::Future<Output = Result<Option<SinkChanges>>> + Send {
        std::future::ready(Ok(None))
    }

    /// Whether an in-flight put may be abandoned on shutdown. Only true if a
    /// repeated put to the requested href updates the same event, so a put
    /// whose result was never recorded cannot cause a duplicate.
    fn cancel_safe(&self) -> bool {
        false
    }
}
