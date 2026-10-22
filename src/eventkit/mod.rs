pub mod convert;
mod tzid;

#[cfg(target_os = "macos")]
mod client;

#[cfg(target_os = "macos")]
pub use client::{
    EventKitClient, count_all_events, count_events_by_marker, delete_all_events,
    delete_events_by_marker,
};

/// A calendar of Calendar.app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarInfo {
    pub title: String,
    /// The account, e.g. "iCloud".
    pub source: Option<String>,
    pub identifier: String,
    /// Events can be added and changed (allowsContentModifications).
    pub writable: bool,
}

/// List the calendars of Calendar.app, asking for calendar access if needed.
#[cfg(target_os = "macos")]
pub fn list_calendars() -> crate::error::Result<Vec<CalendarInfo>> {
    EventKitClient::list_calendars()
}

#[cfg(not(target_os = "macos"))]
pub fn list_calendars() -> crate::error::Result<Vec<CalendarInfo>> {
    Err(crate::error::Error::EventKit(
        "Calendar.app is only available on macOS".to_string(),
    ))
}
