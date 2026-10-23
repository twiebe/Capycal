#![allow(clippy::undocumented_unsafe_blocks)]

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use objc2::AllocAnyThread;
use objc2::rc::Retained;
use objc2_event_kit::{
    EKAlarm, EKCalendar, EKCalendarEventAvailabilityMask, EKEntityType, EKEvent,
    EKEventAvailability, EKEventStore, EKRecurrenceDayOfWeek, EKRecurrenceEnd,
    EKRecurrenceFrequency, EKRecurrenceRule, EKSpan, EKWeekday,
};
use objc2_foundation::{
    NSArray, NSCalendar, NSCalendarUnit, NSDate, NSNumber, NSString, NSTimeZone, NSURL,
};
use tracing::{debug, error, warn};

use crate::error::{Error, Result};
use crate::eventkit::CalendarInfo;
use crate::eventkit::convert::{
    Alarm, Availability, EkEventData, EkTimes, Frequency, IcalAlarm, IcalTime, NotesMarker,
    Recurrence, RecurrenceEnd, Series, SinkUpdate, StoredEvent, StoredTimes, etag, find_marker,
    is_unchanged, occurrence_href, parse_ical, parse_series, query_chunks,
};
use crate::source::{EventSink, EventSource, PutResult, SinkEvent, SourceEvent};
use crate::state::SyncState;
use crate::sync::diff::EtagDiff;

/// Range queried when `all_events` is set (and for full deletes): 20 years
/// back, 4 years forward.
const ALL_EVENTS_DAYS_PAST: i64 = 20 * 365;
const ALL_EVENTS_DAYS_FUTURE: i64 = 4 * 365;

/// How long to wait for the user to answer the calendar access (TCC) prompt.
const ACCESS_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

const DATE_UNITS: NSCalendarUnit = NSCalendarUnit::Year
    .union(NSCalendarUnit::Month)
    .union(NSCalendarUnit::Day);
const DATE_TIME_UNITS: NSCalendarUnit = DATE_UNITS
    .union(NSCalendarUnit::Hour)
    .union(NSCalendarUnit::Minute)
    .union(NSCalendarUnit::Second);

/// EventKit client - reads/writes events via Apple Calendar.
/// Creates a fresh EKEventStore per operation (ObjC objects are not Send+Sync).
#[derive(Clone)]
pub struct EventKitClient {
    calendar_name: String,
    days_past: u64,
    days_future: u64,
    all_events: bool,
}

impl EventKitClient {
    pub fn new(
        calendar_name: &str,
        days_past: u64,
        days_future: u64,
        all_events: bool,
    ) -> Result<Self> {
        // Validate access and calendar on construction
        objc2::rc::autoreleasepool(|_| {
            let store = Self::create_store();
            Self::request_access(&store)?;
            Self::find_calendar(&store, calendar_name)?;
            Ok::<_, crate::error::Error>(())
        })?;

        Ok(Self {
            calendar_name: calendar_name.to_string(),
            days_past,
            days_future,
            all_events,
        })
    }

    fn create_store() -> Retained<EKEventStore> {
        unsafe { EKEventStore::new() }
    }

    fn request_access(store: &EKEventStore) -> Result<()> {
        let (tx, rx) = std::sync::mpsc::channel();
        let block = block2::RcBlock::new(
            move |granted: objc2::runtime::Bool, _error: *mut objc2_foundation::NSError| {
                let _ = tx.send(granted.as_bool());
            },
        );
        unsafe {
            store.requestFullAccessToEventsWithCompletion((&*block) as *const _ as *mut _);
        }

        let granted = rx
            .recv_timeout(ACCESS_REQUEST_TIMEOUT)
            .map_err(|_| Error::EventKit("calendar access request timed out".to_string()))?;

        if !granted {
            return Err(Error::EventKit(
                "calendar access denied - grant Capycal access in System Settings > Privacy > Calendars"
                    .to_string(),
            ));
        }

        Ok(())
    }

    /// Find a calendar by identifier (UUID), source/name, or plain name.
    fn find_calendar(store: &EKEventStore, query: &str) -> Result<Retained<EKCalendar>> {
        let calendars = unsafe { store.calendarsForEntityType(EKEntityType::Event) };

        // Try matching by calendarIdentifier first
        for cal in calendars.iter() {
            let id = unsafe { cal.calendarIdentifier().to_string() };
            if id == query {
                return Ok(cal.clone());
            }
        }

        // Try matching by "source/name"
        if let Some((source_query, name_query)) = query.split_once('/') {
            for cal in calendars.iter() {
                unsafe {
                    let title = cal.title().to_string();
                    let source = cal.source().map(|s| s.title().to_string());
                    if title == name_query && source.as_deref() == Some(source_query) {
                        return Ok(cal.clone());
                    }
                }
            }
        }

        // Fall back to matching by title
        for cal in calendars.iter() {
            let title = unsafe { cal.title().to_string() };
            if title == query {
                return Ok(cal.clone());
            }
        }

        Err(Error::EventKit(format!(
            "calendar '{query}' not found in Calendar.app. Use --discover-eventkit to list available calendars",
        )))
    }

    fn nsdate_from_offset(days_offset: i64) -> Retained<NSDate> {
        let secs = days_offset as f64 * 86400.0;
        NSDate::dateWithTimeIntervalSinceNow(secs)
    }

    /// List all calendars visible in Calendar.app.
    pub fn list_calendars() -> Result<Vec<CalendarInfo>> {
        objc2::rc::autoreleasepool(|_| {
            let store = unsafe { EKEventStore::new() };
            Self::request_access(&store)?;

            let mut result = Vec::new();
            let calendars = unsafe { store.calendarsForEntityType(EKEntityType::Event) };
            for cal in calendars.iter() {
                unsafe {
                    let title = cal.title().to_string();
                    let source = cal.source().map(|s| s.title().to_string());
                    let identifier = cal.calendarIdentifier().to_string();
                    let writable = cal.allowsContentModifications();
                    result.push(CalendarInfo {
                        title,
                        source,
                        identifier,
                        writable,
                    });
                }
            }
            Ok(result)
        })
    }
}

/// Delete all events from an EventKit calendar. Returns (deleted, errors).
pub fn delete_all_events(calendar_name: &str, progress: &dyn Fn(usize)) -> Result<(usize, usize)> {
    delete_matching_events(calendar_name, |_| true, progress)
}

/// Count the events `delete_all_events` would delete, a series once.
pub fn count_all_events(calendar_name: &str) -> Result<usize> {
    let mut count = 0;
    let mut series = HashSet::new();
    for_each_event_in(calendar_name, |_, event| unsafe {
        if event.isDetached()
            || (event.hasRecurrenceRules()
                && !series.insert(event.calendarItemIdentifier().to_string()))
        {
            return;
        }
        count += 1;
    })?;
    Ok(count)
}

/// Delete the events `sync` (reading `source`) created in an EventKit
/// calendar, found by their notes marker. Used by purge.
pub fn delete_events_by_marker(
    calendar_name: &str,
    sync: &str,
    source: &str,
    progress: &dyn Fn(usize),
) -> Result<(usize, usize)> {
    delete_matching_events(
        calendar_name,
        |event| belongs_to(event, sync, source),
        progress,
    )
}

/// Count the events `delete_events_by_marker` would delete.
pub fn count_events_by_marker(calendar_name: &str, sync: &str, source: &str) -> Result<usize> {
    Ok(synced_events(calendar_name, sync, source)?.len())
}

/// The events `sync` (reading `source`) created in an EventKit calendar,
/// over the full range of `for_each_event_in`.
pub fn synced_events(
    calendar_name: &str,
    sync: &str,
    source: &str,
) -> Result<Vec<SinkEvent<StoredEvent>>> {
    let mut seen = HashSet::new();
    let mut events = Vec::new();
    for_each_event_in(calendar_name, |_, event| {
        let Some(marker) = notes_marker(event).filter(|m| m.belongs_to(sync, source)) else {
            return;
        };
        // A changed occurrence of a series has its own identifier but belongs
        // to the series, which is listed by its own identifier
        if unsafe { event.isDetached() } {
            return;
        }
        // Events spanning two query chunks are returned twice
        let href = unsafe { event.calendarItemIdentifier() }.to_string();
        if seen.insert(href.clone()) {
            events.push(SinkEvent {
                href,
                uid: None,
                source_uid: marker.source_uid().map(str::to_string),
                stored: read_stored(event),
            });
        }
    })?;
    Ok(events)
}

fn notes_marker(event: &EKEvent) -> Option<NotesMarker> {
    unsafe { event.notes() }.and_then(|n| find_marker(&n.to_string()))
}

fn belongs_to(event: &EKEvent, sync: &str, source: &str) -> bool {
    notes_marker(event).is_some_and(|m| m.belongs_to(sync, source))
}

/// Delete the events `should_delete` picks, reporting the number handled
/// so far to `progress`.
fn delete_matching_events(
    calendar_name: &str,
    should_delete: impl Fn(&EKEvent) -> bool,
    progress: &dyn Fn(usize),
) -> Result<(usize, usize)> {
    let mut deleted = 0usize;
    let mut errors = 0usize;
    let mut series = HashSet::new();
    for_each_event_in(calendar_name, |store, event| {
        // Changed occurrences go with their series
        if !should_delete(event) || unsafe { event.isDetached() } {
            return;
        }
        unsafe {
            // A series is removed once, from its first occurrence on
            let first = if event.hasRecurrenceRules() {
                let id = event.calendarItemIdentifier();
                if !series.insert(id.to_string()) {
                    return;
                }
                store
                    .calendarItemWithIdentifier(&id)
                    .map(|item| Retained::cast_unchecked::<EKEvent>(item))
            } else {
                None
            };
            let event = first.as_deref().unwrap_or(event);
            match store.removeEvent_span_error(event, series_span(event)) {
                Ok(()) => deleted += 1,
                Err(e) => {
                    errors += 1;
                    tracing::error!(error = %e, "failed to delete event");
                }
            }
            progress(deleted + errors);
        }
    })?;
    Ok((deleted, errors))
}

/// Run `f` for every event of the named calendar over the full range
/// (`ALL_EVENTS_DAYS_PAST` back, `ALL_EVENTS_DAYS_FUTURE` ahead).
fn for_each_event_in(
    calendar_name: &str,
    mut f: impl FnMut(&EKEventStore, &EKEvent),
) -> Result<()> {
    objc2::rc::autoreleasepool(|_| {
        let store = EventKitClient::create_store();
        EventKitClient::request_access(&store)?;

        let calendar = EventKitClient::find_calendar(&store, calendar_name)?;
        let cal_id = unsafe { calendar.calendarIdentifier().to_string() };

        let chunks = query_chunks(-ALL_EVENTS_DAYS_PAST, ALL_EVENTS_DAYS_FUTURE);
        for_each_calendar_event(&store, &cal_id, &chunks, |event| f(&store, event));
        Ok(())
    })
}

impl EventSource for EventKitClient {
    /// Runs the query on a blocking thread, so it neither stalls other
    /// syncs nor keeps a shutdown from cancelling the cycle while it waits.
    async fn fetch_changes(
        &self,
        state: &mut SyncState,
    ) -> Result<(Vec<SourceEvent>, Vec<String>)> {
        let client = self.clone();
        let known: HashMap<String, String> = state
            .uid_mapping
            .source_etags()
            .into_iter()
            .map(|(href, etag)| (href.to_string(), etag.to_string()))
            .collect();
        tokio::task::spawn_blocking(move || {
            objc2::rc::autoreleasepool(|_| {
                let known = known
                    .iter()
                    .map(|(h, e)| (h.as_str(), e.as_str()))
                    .collect();
                client.fetch_blocking(known)
            })
        })
        .await
        .map_err(|e| Error::EventKit(format!("query task failed: {e}")))?
    }
}

impl EventKitClient {
    fn fetch_blocking(
        &self,
        known: HashMap<&str, &str>,
    ) -> Result<(Vec<SourceEvent>, Vec<String>)> {
        let store = Self::create_store();

        debug!(
            calendar = %self.calendar_name,
            days_past = self.days_past,
            days_future = self.days_future,
            all_events = self.all_events,
            "querying eventkit"
        );

        let chunks = if self.all_events {
            query_chunks(-ALL_EVENTS_DAYS_PAST, ALL_EVENTS_DAYS_FUTURE)
        } else {
            vec![(-(self.days_past as i64), self.days_future as i64)]
        };

        // Resolve calendar once to get its stable identifier for filtering
        let cal_id = {
            let cal = Self::find_calendar(&store, &self.calendar_name)?;
            unsafe { cal.calendarIdentifier().to_string() }
        };
        let mut diff = EtagDiff::new(known);
        let mut changed = Vec::new();
        let mut total_events = 0usize;

        // Each occurrence becomes its own event (no RRULE).
        // This ensures deleted occurrences of recurring events are not synced.
        // Unique key per occurrence: base href + start timestamp.
        for_each_calendar_event(&store, &cal_id, &chunks, |event| unsafe {
            // Skip cancelled events
            if event.status() == objc2_event_kit::EKEventStatus::Canceled {
                return;
            }

            total_events += 1;

            let href = occurrence_href(
                &external_id(event),
                event.startDate().timeIntervalSince1970(),
            );

            // Deduplicate (same occurrence across overlapping chunks)
            if diff.contains(&href) {
                return;
            }

            let mod_ts = event
                .lastModifiedDate()
                .map(|d| d.timeIntervalSince1970())
                .unwrap_or(0.0);
            let etag = etag(
                mod_ts,
                event.endDate().timeIntervalSince1970(),
                &read_alarms(event),
                read_availability(event),
            );

            if diff.observe(&href, &etag) {
                let ical = read_event(event).to_ical(chrono::Utc::now());
                changed.push(SourceEvent {
                    uid: href.clone(),
                    href,
                    etag,
                    calendar_data: ical,
                });
            }
        });

        debug!(
            event_count = total_events,
            unique = diff.observed(),
            chunks = chunks.len(),
            calendar = %self.calendar_name,
            "eventkit query complete"
        );

        let removed = diff.removed();

        debug!(
            calendar = %self.calendar_name,
            changed = changed.len(),
            removed = removed.len(),
            total = diff.observed(),
            "eventkit fetch complete"
        );

        Ok((changed, removed))
    }
}

impl EventSink for EventKitClient {
    type Stored = StoredEvent;

    async fn put_event(&self, href: &str, ical_data: &str) -> Result<PutResult> {
        objc2::rc::autoreleasepool(|_| {
            let store = Self::create_store();
            let calendar = Self::find_calendar(&store, &self.calendar_name)?;

            let series = match parse_series(ical_data) {
                Ok(series) => series,
                Err(e) => {
                    warn!(href, error = %e, "writing only the first occurrence of a series");
                    None
                }
            };
            if let Some(series) = &series
                && series.ignored_rdates > 0
            {
                warn!(
                    href,
                    count = series.ignored_rdates,
                    "Calendar.app has no extra dates (RDATE) in a series, leaving them out"
                );
            }

            let existing: Option<Retained<EKEvent>> = unsafe {
                store
                    .calendarItemWithIdentifier(&NSString::from_str(href))
                    .map(|item| Retained::cast_unchecked(item))
            };

            unsafe {
                // A series is rewritten as a whole, since EventKit keeps the
                // exceptions of an existing one
                let event = match existing {
                    Some(old) if series.is_some() || old.hasRecurrenceRules() => {
                        store
                            .removeEvent_span_error(&old, EKSpan::FutureEvents)
                            .map_err(|e| {
                                Error::EventKit(format!("failed to replace series: {e}"))
                            })?;
                        EKEvent::eventWithEventStore(&store)
                    }
                    Some(event) => event,
                    None => EKEvent::eventWithEventStore(&store),
                };
                event.setCalendar(Some(&calendar));

                apply_update(&event, &calendar, &parse_ical(ical_data));
                let rule = series.as_ref().and_then(|s| recurrence_rule(&s.rule));
                if let Some(rule) = &rule {
                    event.setRecurrenceRules(Some(&NSArray::from_retained_slice(
                        std::slice::from_ref(rule),
                    )));
                }

                store
                    .saveEvent_span_error(&event, EKSpan::ThisEvent)
                    .map_err(|e| Error::EventKit(format!("failed to save event: {e}")))?;

                if let (Some(series), Some(_)) = (&series, &rule) {
                    apply_exceptions(&store, &calendar, &event, series);
                }

                let etag = event
                    .lastModifiedDate()
                    .map(|d| format!("{:.0}", d.timeIntervalSince1970()))
                    .unwrap_or_else(|| "0".to_string());

                // Return the actual EventKit identifier so subsequent updates
                // can find this event again (the input href is a capycal-generated
                // ID that EventKit doesn't know about).
                let actual_href = event.calendarItemIdentifier().to_string();

                Ok(PutResult {
                    etag,
                    href: actual_href,
                })
            }
        })
    }

    async fn delete_event(&self, href: &str) -> Result<()> {
        objc2::rc::autoreleasepool(|_| {
            let store = Self::create_store();

            let existing = unsafe { store.calendarItemWithIdentifier(&NSString::from_str(href)) };

            if let Some(item) = existing {
                unsafe {
                    let event: Retained<EKEvent> = Retained::cast_unchecked(item);
                    store
                        .removeEvent_span_error(&event, series_span(&event))
                        .map_err(|e| Error::EventKit(format!("failed to delete event: {e}")))?;
                }
            }

            Ok(())
        })
    }

    async fn list_synced(
        &self,
        sync_name: &str,
        source_name: &str,
    ) -> Result<Vec<SinkEvent<StoredEvent>>> {
        synced_events(&self.calendar_name, sync_name, source_name)
    }

    fn is_unchanged(&self, stored: &StoredEvent, ical_data: &str) -> bool {
        if matches!(parse_series(ical_data), Ok(Some(_))) {
            return false;
        }
        objc2::rc::autoreleasepool(|_| {
            is_unchanged(&parse_ical(ical_data), stored, |value, tzid| {
                ical_to_nsdate(value, tzid).map(|date| date.timeIntervalSince1970())
            })
        })
    }

    /// A new EKEvent gets its identifier from EventKit on save. A put dropped
    /// before its mapping is recorded would create a duplicate on the next put.
    fn cancel_safe(&self) -> bool {
        false
    }
}

/// Run `f` for every event of the calendar `cal_id` in the given day-offset
/// chunks. Each chunk gets its own autorelease pool.
fn for_each_calendar_event(
    store: &EKEventStore,
    cal_id: &str,
    chunks: &[(i64, i64)],
    mut f: impl FnMut(&EKEvent),
) {
    for &(start_days, end_days) in chunks {
        objc2::rc::autoreleasepool(|_| {
            let start = EventKitClient::nsdate_from_offset(start_days);
            let end = EventKitClient::nsdate_from_offset(end_days);

            let events = unsafe {
                let predicate =
                    store.predicateForEventsWithStartDate_endDate_calendars(&start, &end, None);
                store.eventsMatchingPredicate(&predicate)
            };

            for event in events.iter() {
                let matches = unsafe {
                    event
                        .calendar()
                        .map(|c| c.calendarIdentifier().to_string() == cal_id)
                        .unwrap_or(false)
                };
                if matches {
                    f(&event);
                }
            }
        });
    }
}

fn external_id(event: &EKEvent) -> String {
    unsafe { event.calendarItemExternalIdentifier() }
        .map(|s| s.to_string())
        .unwrap_or_default()
}

fn read_availability(event: &EKEvent) -> Availability {
    match unsafe { event.availability() } {
        EKEventAvailability::Free => Availability::Free,
        EKEventAvailability::NotSupported => Availability::NotSupported,
        _ => Availability::Busy,
    }
}

fn read_alarms(event: &EKEvent) -> Vec<Alarm> {
    let Some(alarms) = (unsafe { event.alarms() }) else {
        return Vec::new();
    };
    alarms
        .iter()
        .map(|alarm| match unsafe { alarm.absoluteDate() } {
            Some(date) => Alarm::Absolute(date.timeIntervalSince1970()),
            None => Alarm::Relative(unsafe { alarm.relativeOffset() }),
        })
        .collect()
}

/// Read an EKEvent occurrence into plain data, converting its boundaries to
/// local values.
fn read_event(event: &EKEvent) -> EkEventData {
    unsafe {
        let start = event.startDate();
        let end = event.endDate();
        let times = if event.isAllDay() {
            // EventKit stores all-day event boundaries as local instants,
            // not UTC. Extracting UTC date components here would shift the
            // date by a day whenever the local UTC offset is nonzero.
            EkTimes::AllDay {
                start: nsdate_to_ical_date(&start),
                end: all_day_end(&end),
            }
        } else {
            let tzid = event.timeZone().map(|tz| tz.name().to_string());
            EkTimes::Timed {
                start_local: tzid
                    .as_deref()
                    .and_then(|tz| nsdate_to_ical_local(&start, tz)),
                end_local: tzid
                    .as_deref()
                    .and_then(|tz| nsdate_to_ical_local(&end, tz)),
                tzid,
            }
        };
        EkEventData {
            base_uid: external_id(event),
            start_ts: start.timeIntervalSince1970(),
            end_ts: end.timeIntervalSince1970(),
            times,
            title: event.title().to_string(),
            location: event.location().map(|s| s.to_string()),
            url: read_url(event),
            notes: event.notes().map(|s| s.to_string()),
            availability: read_availability(event),
            alarms: read_alarms(event),
        }
    }
}

fn read_url(event: &EKEvent) -> Option<String> {
    unsafe { event.URL() }
        .and_then(|url| url.absoluteString())
        .map(|s| s.to_string())
}

/// Read the fields `apply_update` sets from a sink event.
fn read_stored(event: &EKEvent) -> StoredEvent {
    unsafe {
        let start = event.startDate();
        let end = event.endDate();
        let times = if event.isAllDay() {
            StoredTimes::AllDay {
                start: nsdate_to_ical_date(&start),
                end: all_day_end(&end),
            }
        } else {
            StoredTimes::Timed {
                start: start.timeIntervalSince1970(),
                end: end.timeIntervalSince1970(),
            }
        };
        let supports_free = event.calendar().is_some_and(|calendar| {
            calendar
                .supportedEventAvailabilities()
                .contains(EKCalendarEventAvailabilityMask::Free)
        });
        StoredEvent {
            title: event.title().to_string(),
            location: event.location().map(|s| s.to_string()),
            url: read_url(event),
            notes: event.notes().map(|s| s.to_string()),
            times,
            alarms: read_alarms(event),
            free: supports_free.then(|| event.availability() == EKEventAvailability::Free),
            recurring: event.hasRecurrenceRules(),
            floating: !event.isAllDay() && event.timeZone().is_none(),
        }
    }
}

/// Apply parsed iCal to a sink event. Texts absent from the iCal are
/// cleared, times keep their current value.
fn apply_update(event: &EKEvent, calendar: &EKCalendar, update: &SinkUpdate) {
    let text = |value: &Option<String>| {
        value
            .as_deref()
            .filter(|v| !v.is_empty())
            .map(NSString::from_str)
    };
    unsafe {
        event.setTitle(Some(&NSString::from_str(
            update.title.as_deref().unwrap_or(""),
        )));
        event.setLocation(text(&update.location).as_deref());
        // An unreadable URL removes it too
        let url = text(&update.url).and_then(|u| NSURL::URLWithString(&u));
        event.setURL(url.as_deref());
        let notes = update.notes_with_marker();
        event.setNotes(
            (!notes.is_empty())
                .then(|| NSString::from_str(&notes))
                .as_deref(),
        );
        // Set before the dates: the zone the event belongs to, so it keeps
        // its time when the Mac changes zones. All-day and floating events
        // have none.
        if let Some(start) = &update.start {
            let zone = match start {
                IcalTime::Date(_) => None,
                IcalTime::DateTime { value, tzid } => ical_time_zone(value, tzid.as_deref()),
            };
            event.setTimeZone(zone.as_deref());
        }
        if let Some(date) = update.start.as_ref().and_then(ical_time_to_nsdate) {
            event.setStartDate(Some(&date));
        }
        if let Some(all_day) = update.all_day() {
            event.setAllDay(all_day);
        }
        // EventKit ends an all-day event at the last second of its last
        // day; iCal's end is the following midnight
        let inclusive = if update.all_day() == Some(true) {
            -1.0
        } else {
            0.0
        };
        if let Some(date) = update.end.as_ref().and_then(ical_time_to_nsdate) {
            event.setEndDate(Some(&date.dateByAddingTimeInterval(inclusive)));
        }

        // DURATION is the alternative to DTEND (RFC 5545 section 3.6.1)
        if let (None, Some(secs)) = (&update.end, update.duration) {
            let end = event
                .startDate()
                .dateByAddingTimeInterval(secs as f64 + inclusive);
            event.setEndDate(Some(&end));
        }

        // RFC 5545 default is OPAQUE, so a missing TRANSP means busy
        if calendar
            .supportedEventAvailabilities()
            .contains(EKCalendarEventAvailabilityMask::Free)
        {
            event.setAvailability(if update.transparent {
                EKEventAvailability::Free
            } else {
                EKEventAvailability::Busy
            });
        }

        let alarms: Vec<Retained<EKAlarm>> = update
            .alarms
            .iter()
            .filter_map(|alarm| match alarm {
                IcalAlarm::Absolute(value) => {
                    ical_to_nsdate(value, None).map(|date| EKAlarm::alarmWithAbsoluteDate(&date))
                }
                IcalAlarm::Relative(secs) => Some(EKAlarm::alarmWithRelativeOffset(*secs as f64)),
            })
            .collect();
        if alarms.is_empty() {
            event.setAlarms(None);
        } else {
            event.setAlarms(Some(&NSArray::from_retained_slice(&alarms)));
        }
    }
}

/// Removing an occurrence of a series removes the whole series.
fn series_span(event: &EKEvent) -> EKSpan {
    if unsafe { event.hasRecurrenceRules() } {
        EKSpan::FutureEvents
    } else {
        EKSpan::ThisEvent
    }
}

fn recurrence_rule(rule: &Recurrence) -> Option<Retained<EKRecurrenceRule>> {
    let numbers = |values: &[i64]| {
        (!values.is_empty()).then(|| {
            let numbers: Vec<_> = values
                .iter()
                .map(|n| NSNumber::new_isize(*n as isize))
                .collect();
            NSArray::from_retained_slice(&numbers)
        })
    };
    let frequency = match rule.frequency {
        Frequency::Daily => EKRecurrenceFrequency::Daily,
        Frequency::Weekly => EKRecurrenceFrequency::Weekly,
        Frequency::Monthly => EKRecurrenceFrequency::Monthly,
        Frequency::Yearly => EKRecurrenceFrequency::Yearly,
    };
    unsafe {
        let days = (!rule.days_of_week.is_empty()).then(|| {
            let days: Vec<_> = rule
                .days_of_week
                .iter()
                .map(|(day, week)| {
                    EKRecurrenceDayOfWeek::dayOfWeek_weekNumber(
                        EKWeekday(*day as isize),
                        *week as isize,
                    )
                })
                .collect();
            NSArray::from_retained_slice(&days)
        });
        let end = match &rule.end {
            Some(RecurrenceEnd::Count(count)) => Some(
                EKRecurrenceEnd::recurrenceEndWithOccurrenceCount(*count as usize),
            ),
            // UNTIL as a date includes that whole day
            Some(RecurrenceEnd::Until(IcalTime::Date(value))) => {
                let date = ical_date_to_nsdate(value)?.dateByAddingTimeInterval(86399.0);
                Some(EKRecurrenceEnd::recurrenceEndWithEndDate(&date))
            }
            Some(RecurrenceEnd::Until(until)) => {
                let date = ical_time_to_nsdate(until)?;
                Some(EKRecurrenceEnd::recurrenceEndWithEndDate(&date))
            }
            None => None,
        };
        Some(EKRecurrenceRule::initRecurrenceWithFrequency_interval_daysOfTheWeek_daysOfTheMonth_monthsOfTheYear_weeksOfTheYear_daysOfTheYear_setPositions_end(
            EKRecurrenceRule::alloc(),
            frequency,
            rule.interval as isize,
            days.as_deref(),
            numbers(&rule.days_of_month).as_deref(),
            numbers(&rule.months_of_year).as_deref(),
            numbers(&rule.weeks_of_year).as_deref(),
            numbers(&rule.days_of_year).as_deref(),
            numbers(&rule.set_positions).as_deref(),
            end.as_deref(),
        ))
    }
}

/// Remove and change single occurrences of the saved `series` event. A
/// failure leaves that occurrence as the rule creates it and is logged, since
/// the series itself is saved.
fn apply_exceptions(store: &EKEventStore, calendar: &EKCalendar, series: &EKEvent, data: &Series) {
    let id = unsafe { series.calendarItemIdentifier() }.to_string();
    for original in &data.removed {
        let Some(occurrence) = find_occurrence(store, calendar, &id, original) else {
            debug!(?original, "excluded occurrence not found in the series");
            continue;
        };
        if let Err(e) = unsafe { store.removeEvent_span_error(&occurrence, EKSpan::ThisEvent) } {
            error!(?original, error = %e, "failed to remove an occurrence of a series");
        }
    }
    for (original, update) in &data.changed {
        let Some(occurrence) = find_occurrence(store, calendar, &id, original) else {
            debug!(?original, "changed occurrence not found in the series");
            continue;
        };
        apply_update(&occurrence, calendar, update);
        if let Err(e) = unsafe { store.saveEvent_span_error(&occurrence, EKSpan::ThisEvent) } {
            error!(?original, error = %e, "failed to change an occurrence of a series");
        }
    }
}

/// The occurrence of the series `id` that originally started at `original`.
fn find_occurrence(
    store: &EKEventStore,
    calendar: &EKCalendar,
    id: &str,
    original: &IcalTime,
) -> Option<Retained<EKEvent>> {
    let original = ical_time_to_nsdate(original)?.timeIntervalSince1970();
    unsafe {
        let start = NSDate::dateWithTimeIntervalSince1970(original - 86400.0);
        let end = NSDate::dateWithTimeIntervalSince1970(original + 86400.0);
        let calendars = NSArray::from_slice(&[calendar]);
        let predicate =
            store.predicateForEventsWithStartDate_endDate_calendars(&start, &end, Some(&calendars));
        store
            .eventsMatchingPredicate(&predicate)
            .iter()
            .find(|event| {
                event.calendarItemIdentifier().to_string() == id
                    && event
                        .occurrenceDate()
                        .is_some_and(|d| (d.timeIntervalSince1970() - original).abs() < 1.0)
            })
    }
}

fn ical_time_to_nsdate(time: &IcalTime) -> Option<Retained<NSDate>> {
    match time {
        IcalTime::Date(value) => ical_date_to_nsdate(value),
        IcalTime::DateTime { value, tzid } => ical_to_nsdate(value, tzid.as_deref()),
    }
}

/// Convert NSDate to local datetime string "YYYYMMDDTHHMMSS" (no Z) in the given timezone.
fn nsdate_to_ical_local(date: &Retained<NSDate>, tz_name: &str) -> Option<String> {
    {
        let calendar = NSCalendar::currentCalendar();
        let tz = NSTimeZone::timeZoneWithName(&NSString::from_str(tz_name))?;
        calendar.setTimeZone(&tz);

        let components = calendar.components_fromDate(DATE_TIME_UNITS, date);

        let y = components.year();
        let m = components.month();
        let d = components.day();
        let h = components.hour();
        let min = components.minute();
        let sec = components.second();

        Some(format!(
            "{:04}{:02}{:02}T{:02}{:02}{:02}",
            y, m, d, h, min, sec
        ))
    }
}

/// Convert an all-day NSDate boundary to a date-only iCal string "YYYYMMDD",
/// using the system's local timezone (matches how EventKit computes the
/// local calendar day for all-day events).
/// The iCal end date (the day after the last day) of an all-day event.
/// EventKit stores its end as the last second of the last day, some
/// calendars as the following midnight; one second later is the following
/// day in both cases.
fn all_day_end(end: &NSDate) -> String {
    nsdate_to_ical_date(&end.dateByAddingTimeInterval(1.0))
}

fn nsdate_to_ical_date(date: &Retained<NSDate>) -> String {
    let calendar = NSCalendar::currentCalendar();
    let components = calendar.components_fromDate(DATE_UNITS, date);
    format!(
        "{:04}{:02}{:02}",
        components.year(),
        components.month(),
        components.day()
    )
}

/// The time zone of an iCal date-time value: UTC for a value with Z,
/// `tz_name` if macOS knows it, else None (floating, or unknown with a
/// warning), which means local time.
fn ical_time_zone(value: &str, tz_name: Option<&str>) -> Option<Retained<NSTimeZone>> {
    match (value.ends_with('Z'), tz_name) {
        (true, _) => NSTimeZone::timeZoneWithName(&NSString::from_str("UTC")),
        (false, Some(name)) => {
            let tz = NSTimeZone::timeZoneWithName(&NSString::from_str(name));
            if tz.is_none() {
                warn!(tzid = name, "unknown time zone, using local time");
            }
            tz
        }
        (false, None) => None,
    }
}

/// Parse an iCal datetime (YYYYMMDDTHHMMSS with optional Z suffix) into NSDate.
/// A value with Z is UTC, one with `tz_name` is in that timezone, and a
/// floating one (neither) is local time (RFC 5545 section 3.3.5). A TZID
/// unknown to macOS also falls back to local time; `parse_ical` already
/// resolved what it could (see `tzid::resolve_timezones`).
fn ical_to_nsdate(s: &str, tz_name: Option<&str>) -> Option<Retained<NSDate>> {
    let bare = s.strip_suffix('Z').unwrap_or(s);

    if bare.len() < 15 {
        return None;
    }

    let y: isize = bare[0..4].parse().ok()?;
    let m: isize = bare[4..6].parse().ok()?;
    let d: isize = bare[6..8].parse().ok()?;
    let h: isize = bare[9..11].parse().ok()?;
    let min: isize = bare[11..13].parse().ok()?;
    let sec: isize = bare[13..15].parse().ok()?;

    let calendar = NSCalendar::currentCalendar();

    let tz = ical_time_zone(s, tz_name).unwrap_or_else(NSTimeZone::localTimeZone);
    calendar.setTimeZone(&tz);

    let components = calendar.components_fromDate(DATE_TIME_UNITS, &NSDate::now());
    components.setYear(y);
    components.setMonth(m);
    components.setDay(d);
    components.setHour(h);
    components.setMinute(min);
    components.setSecond(sec);

    calendar.dateFromComponents(&components)
}

/// Build the local-midnight NSDate for an iCal date-only value "YYYYMMDD".
/// Must stay in the system's local timezone (not UTC) to match how
/// nsdate_to_ical_date reads all-day boundaries back out.
fn ical_date_to_nsdate(s: &str) -> Option<Retained<NSDate>> {
    if s.len() < 8 {
        return None;
    }

    let y: isize = s[0..4].parse().ok()?;
    let m: isize = s[4..6].parse().ok()?;
    let d: isize = s[6..8].parse().ok()?;

    let calendar = NSCalendar::currentCalendar();
    let components = calendar.components_fromDate(DATE_TIME_UNITS, &NSDate::now());
    components.setYear(y);
    components.setMonth(m);
    components.setDay(d);
    components.setHour(0);
    components.setMinute(0);
    components.setSecond(0);

    calendar.dateFromComponents(&components)
}

/// Tests against the real Calendar.app database. Ignored by default; run
/// them on a Mac with `make test-eventkit` from Terminal (macOS asks Terminal
/// for calendar access once). Every test works in its own new calendar named
/// `capycal-test-<pid>-<test>` and removes it afterwards, also when it fails.
/// Calendars with that prefix left over by an aborted earlier run are
/// removed too. No other calendar is touched.
#[cfg(test)]
mod live_tests {
    use super::*;
    use objc2_event_kit::{EKSource, EKSourceType};

    const PREFIX: &str = "capycal-test-";
    const SYNC: &str = "capycal-test-sync";

    /// A new calendar, removed on drop.
    struct TestCalendar {
        id: String,
        title: String,
    }

    impl TestCalendar {
        fn new(test: &str) -> Self {
            let title = format!("{PREFIX}{}-{test}", std::process::id());
            let id = objc2::rc::autoreleasepool(|_| unsafe {
                let store = EventKitClient::create_store();
                EventKitClient::request_access(&store).expect("calendar access");
                remove_leftovers(&store);
                // "On My Mac" if there is one, else where new events go
                let sources = store.sources();
                let local = sources
                    .iter()
                    .find(|s| s.sourceType() == EKSourceType::Local);
                let source = local
                    .clone()
                    .or_else(|| store.defaultCalendarForNewEvents()?.source())
                    .expect("no calendar source");
                static REPORTED: std::sync::Once = std::sync::Once::new();
                REPORTED.call_once(|| {
                    let describe =
                        |s: &EKSource| format!("{} (type {})", s.title(), s.sourceType().0);
                    report(&format!("test calendars go to {}", describe(&source)));
                    if local.is_none() {
                        let all: Vec<String> = sources.iter().map(|s| describe(&s)).collect();
                        report(&format!("no local source; sources: {}", all.join(", ")));
                    }
                });
                let calendar =
                    EKCalendar::calendarForEntityType_eventStore(EKEntityType::Event, &store);
                calendar.setTitle(&NSString::from_str(&title));
                calendar.setSource(Some(&source));
                store
                    .saveCalendar_commit_error(&calendar, true)
                    .expect("cannot create the test calendar");
                calendar.calendarIdentifier().to_string()
            });
            Self { id, title }
        }

        fn sink(&self) -> EventKitClient {
            EventKitClient::new(&self.id, 30, 365, true).unwrap()
        }

        /// (calendar item id, start, end, title, all day, part of a series)
        /// of every occurrence, by start. A changed occurrence of a series
        /// has its own id.
        fn occurrences(&self) -> Vec<(String, f64, f64, String, bool, bool)> {
            let mut found = Vec::new();
            for_each_event_in(&self.id, |_, event| unsafe {
                found.push((
                    event.calendarItemIdentifier().to_string(),
                    event.startDate().timeIntervalSince1970(),
                    event.endDate().timeIntervalSince1970(),
                    event.title().to_string(),
                    event.isAllDay(),
                    event.hasRecurrenceRules() || event.isDetached(),
                ));
            })
            .unwrap();
            // Events spanning two query chunks come twice
            found.sort_by(|a, b| a.1.total_cmp(&b.1));
            found.dedup();
            found
        }

        /// The time zone name of each event, sorted.
        fn time_zones(&self) -> Vec<Option<String>> {
            let mut zones = Vec::new();
            for_each_event_in(&self.id, |_, event| unsafe {
                zones.push(event.timeZone().map(|tz| tz.name().to_string()));
            })
            .unwrap();
            zones.sort();
            zones
        }

        fn starts(&self) -> Vec<f64> {
            self.occurrences().iter().map(|o| o.1).collect()
        }
    }

    impl Drop for TestCalendar {
        /// Remove the calendar by id or by its unique title: an account that
        /// syncs (CalDAV, iCloud) may give it a new id after creating it. It
        /// may also bring the calendar back when it syncs the creation after
        /// the removal, so the calendar is removed again until it stays away.
        fn drop(&mut self) {
            let mut removals = 0;
            let mut absent = 0;
            let deadline = std::time::Instant::now() + SETTLE_TIMEOUT;
            while absent < SETTLE_CHECKS {
                if std::time::Instant::now() > deadline {
                    report(&format!(
                        "test calendar {} keeps coming back; remove it by hand",
                        self.title
                    ));
                    return;
                }
                if self.remove() {
                    removals += 1;
                    absent = 0;
                } else {
                    absent += 1;
                }
                std::thread::sleep(SETTLE_INTERVAL);
            }
            if removals == 0 {
                report(&format!(
                    "test calendar {} not found for removal",
                    self.title
                ));
            } else if removals > 1 {
                report(&format!(
                    "test calendar {} came back after removal, removed {removals} times",
                    self.title
                ));
            }
        }
    }

    /// Checks in a row without the calendar before it counts as removed.
    const SETTLE_CHECKS: u32 = 3;
    const SETTLE_INTERVAL: Duration = Duration::from_secs(1);
    const SETTLE_TIMEOUT: Duration = Duration::from_secs(30);

    impl TestCalendar {
        /// Remove the calendar if it exists; whether it did.
        fn remove(&self) -> bool {
            objc2::rc::autoreleasepool(|_| unsafe {
                let store = EventKitClient::create_store();
                let mut found = false;
                for calendar in store.calendarsForEntityType(EKEntityType::Event).iter() {
                    let title = calendar.title().to_string();
                    let id = calendar.calendarIdentifier().to_string();
                    if !title.starts_with(PREFIX) || (title != self.title && id != self.id) {
                        continue;
                    }
                    found = true;
                    if let Err(e) = store.removeCalendar_commit_error(&calendar, true) {
                        report(&format!("cannot remove test calendar {title}: {e}"));
                    }
                }
                found
            })
        }
    }

    /// Print past the test output capture, so it shows for passing tests too.
    fn report(message: &str) {
        use std::io::Write;
        let _ = writeln!(std::io::stderr(), "{message}");
    }

    /// Remove `capycal-test-` calendars of earlier, aborted runs.
    unsafe fn remove_leftovers(store: &EKEventStore) {
        let own = format!("{PREFIX}{}-", std::process::id());
        for calendar in unsafe { store.calendarsForEntityType(EKEntityType::Event) }.iter() {
            let title = unsafe { calendar.title() }.to_string();
            if title.starts_with(PREFIX) && !title.starts_with(&own) {
                report(&format!("removing leftover test calendar {title}"));
                let _ = unsafe { store.removeCalendar_commit_error(&calendar, true) };
            }
        }
    }

    /// Unix time of a UTC iCal date-time.
    fn ts(value: &str) -> f64 {
        chrono::NaiveDateTime::parse_from_str(value, "%Y%m%dT%H%M%SZ")
            .unwrap()
            .and_utc()
            .timestamp() as f64
    }

    /// A synced event; `body` holds its properties (and further VEVENTs).
    /// January and February dates avoid daylight saving changes.
    fn ics(uid: &str, body: &str) -> String {
        format!(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//capycal test//EN\r\nBEGIN:VEVENT\r\nUID:capycal-{uid}\r\n\
             DTSTAMP:20260101T000000Z\r\n{}\r\nX-CAPYCAL-SOURCE:src/{uid}\r\nX-CAPYCAL-SYNC:{SYNC}\r\n\
             END:VEVENT\r\nEND:VCALENDAR\r\n",
            body.trim_end().replace('\n', "\r\n")
        )
    }

    const SERIES: &str = "DTSTART:20270104T090000Z
DTEND:20270104T100000Z
SUMMARY:Jour fixe
RRULE:FREQ=WEEKLY;COUNT=6
EXDATE:20270118T090000Z
X-CAPYCAL-SOURCE:src/series
X-CAPYCAL-SYNC:capycal-test-sync
END:VEVENT
BEGIN:VEVENT
UID:capycal-series
DTSTAMP:20260101T000000Z
RECURRENCE-ID:20270111T090000Z
DTSTART:20270111T110000Z
DTEND:20270111T120000Z
SUMMARY:Moved
END:VEVENT
BEGIN:VEVENT
UID:capycal-series
DTSTAMP:20260101T000000Z
RECURRENCE-ID:20270125T090000Z
DTSTART:20270125T090000Z
DTEND:20270125T100000Z
STATUS:CANCELLED";

    #[tokio::test]
    #[ignore = "uses Calendar.app, run `make test-eventkit` on a Mac"]
    async fn live_series_with_exceptions() {
        let calendar = TestCalendar::new("series");
        let sink = calendar.sink();
        sink.put_event("capycal-series.ics", &ics("series", SERIES))
            .await
            .unwrap();
        let occurrences = calendar.occurrences();
        let summary: Vec<(f64, &str)> = occurrences.iter().map(|o| (o.1, o.3.as_str())).collect();
        // 6 weekly, minus the excluded and the cancelled one; one moved
        assert_eq!(
            summary,
            [
                (ts("20270104T090000Z"), "Jour fixe"),
                (ts("20270111T110000Z"), "Moved"),
                (ts("20270201T090000Z"), "Jour fixe"),
                (ts("20270208T090000Z"), "Jour fixe"),
            ]
        );
        assert!(occurrences.iter().all(|o| o.5), "{occurrences:?}");
        // Only the moved occurrence has its own id
        let own_ids = occurrences
            .iter()
            .filter(|o| o.0 != occurrences[0].0)
            .count();
        assert_eq!(own_ids, 1, "{occurrences:?}");
    }

    #[tokio::test]
    #[ignore = "uses Calendar.app, run `make test-eventkit` on a Mac"]
    async fn live_until_date_includes_that_day() {
        let calendar = TestCalendar::new("until");
        let body = "DTSTART:20270104T090000Z\nDTEND:20270104T093000Z\nSUMMARY:Daily\nRRULE:FREQ=DAILY;UNTIL=20270106";
        calendar
            .sink()
            .put_event("capycal-until.ics", &ics("until", body))
            .await
            .unwrap();
        assert_eq!(
            calendar.starts(),
            [
                ts("20270104T090000Z"),
                ts("20270105T090000Z"),
                ts("20270106T090000Z")
            ]
        );
    }

    #[tokio::test]
    #[ignore = "uses Calendar.app, run `make test-eventkit` on a Mac"]
    async fn live_monthly_last_friday() {
        let calendar = TestCalendar::new("monthly");
        let body = "DTSTART:20270129T090000Z\nDTEND:20270129T100000Z\nSUMMARY:Review\nRRULE:FREQ=MONTHLY;BYDAY=-1FR;COUNT=2";
        calendar
            .sink()
            .put_event("capycal-monthly.ics", &ics("monthly", body))
            .await
            .unwrap();
        assert_eq!(
            calendar.starts(),
            [ts("20270129T090000Z"), ts("20270226T090000Z")]
        );
    }

    #[tokio::test]
    #[ignore = "uses Calendar.app, run `make test-eventkit` on a Mac"]
    async fn live_all_day_series() {
        let calendar = TestCalendar::new("allday");
        let body = "DTSTART;VALUE=DATE:20270105\nDTEND;VALUE=DATE:20270106\nSUMMARY:Birthday\nRRULE:FREQ=YEARLY;COUNT=2";
        calendar
            .sink()
            .put_event("capycal-allday.ics", &ics("allday", body))
            .await
            .unwrap();
        let occurrences = calendar.occurrences();
        assert_eq!(occurrences.len(), 2, "{occurrences:?}");
        assert!(occurrences.iter().all(|o| o.4 && o.5), "{occurrences:?}");
        // One day long, not two
        assert!(
            occurrences.iter().all(|o| (o.2 - o.1) < 86400.0),
            "{occurrences:?}"
        );
    }

    #[tokio::test]
    #[ignore = "uses Calendar.app, run `make test-eventkit` on a Mac"]
    async fn live_rewriting_a_series_leaves_no_duplicates() {
        let calendar = TestCalendar::new("rewrite");
        let sink = calendar.sink();
        let first = sink
            .put_event("capycal-series.ics", &ics("series", SERIES))
            .await
            .unwrap();
        let renamed = ics("series", &SERIES.replace("Jour fixe", "Renamed"));
        let second = sink.put_event(&first.href, &renamed).await.unwrap();
        let occurrences = calendar.occurrences();
        assert_eq!(occurrences.len(), 4, "{occurrences:?}");
        let series = occurrences.iter().filter(|o| o.0 == second.href).count();
        assert_eq!(series, 3, "{occurrences:?}");
        assert_eq!(occurrences[0].3, "Renamed");
        // The series is never reported unchanged, so full syncs rewrite it
        let synced = sink.list_synced(SYNC, "src").await.unwrap();
        assert_eq!(synced.len(), 1);
        assert!(!sink.is_unchanged(&synced[0].stored, &renamed));
    }

    #[tokio::test]
    #[ignore = "uses Calendar.app, run `make test-eventkit` on a Mac"]
    async fn live_single_event_round_trip() {
        let calendar = TestCalendar::new("single");
        let sink = calendar.sink();
        let body = "DTSTART:20270104T090000Z\nDTEND:20270104T100000Z\nSUMMARY:Single\nURL:https://meet.example.com/single\nTRANSP:TRANSPARENT";
        let ical = ics("single", body);
        let put = sink.put_event("capycal-single.ics", &ical).await.unwrap();
        let occurrences = calendar.occurrences();
        assert_eq!(occurrences.len(), 1);
        assert_eq!(occurrences[0].0, put.href);
        assert!(!occurrences[0].5);
        let synced = sink.list_synced(SYNC, "src").await.unwrap();
        assert_eq!(synced.len(), 1);
        assert_eq!(synced[0].source_uid.as_deref(), Some("single"));
        assert!(sink.is_unchanged(&synced[0].stored, &ical));
        assert!(!sink.is_unchanged(&synced[0].stored, &ical.replace("Single", "Other")));
        assert!(!sink.is_unchanged(&synced[0].stored, &ical.replace("/single", "/other")));

        // The source dropped the link: the copy loses it
        let bare = ical.replace("URL:https://meet.example.com/single\r\n", "");
        assert_ne!(bare, ical);
        sink.put_event(&put.href, &bare).await.unwrap();
        let synced = sink.list_synced(SYNC, "src").await.unwrap();
        assert!(sink.is_unchanged(&synced[0].stored, &bare));
        assert!(!sink.is_unchanged(&synced[0].stored, &ical));
    }

    #[tokio::test]
    #[ignore = "uses Calendar.app, run `make test-eventkit` on a Mac"]
    async fn live_windows_and_prefixed_tzids() {
        let calendar = TestCalendar::new("tzid");
        let sink = calendar.sink();
        for (uid, tzid) in [
            ("windows", "W. Europe Standard Time"),
            (
                "prefixed",
                "/freeassociation.sourceforge.net/Tzfile/Europe/Berlin",
            ),
        ] {
            let body = format!(
                "DTSTART;TZID={tzid}:20270104T090000\nDTEND;TZID={tzid}:20270104T100000\nSUMMARY:{uid}"
            );
            sink.put_event(&format!("capycal-{uid}.ics"), &ics(uid, &body))
                .await
                .unwrap();
        }
        // 09:00 in Berlin in winter is 08:00 UTC
        assert_eq!(calendar.starts(), [1799049600.0, 1799049600.0]);
        let berlin = Some("Europe/Berlin".to_string());
        assert_eq!(calendar.time_zones(), [berlin.clone(), berlin]);
        // Events of older versions had none and are rewritten by a full sync
        let synced = sink.list_synced(SYNC, "src").await.unwrap();
        assert!(synced.iter().all(|e| !e.stored.floating));
    }

    #[tokio::test]
    #[ignore = "uses Calendar.app, run `make test-eventkit` on a Mac"]
    async fn live_unsupported_rule_writes_the_first_occurrence() {
        let calendar = TestCalendar::new("hourly");
        let body = "DTSTART:20270104T090000Z\nDTEND:20270104T091500Z\nSUMMARY:Hourly\nRRULE:FREQ=HOURLY;COUNT=3";
        calendar
            .sink()
            .put_event("capycal-hourly.ics", &ics("hourly", body))
            .await
            .unwrap();
        let occurrences = calendar.occurrences();
        assert_eq!(occurrences.len(), 1, "{occurrences:?}");
        assert!(!occurrences[0].5);
    }

    #[tokio::test]
    #[ignore = "uses Calendar.app, run `make test-eventkit` on a Mac"]
    async fn live_delete_and_purge_remove_whole_series() {
        let calendar = TestCalendar::new("delete");
        let sink = calendar.sink();
        let series = sink
            .put_event("capycal-series.ics", &ics("series", SERIES))
            .await
            .unwrap();
        sink.delete_event(&series.href).await.unwrap();
        assert!(calendar.occurrences().is_empty());

        sink.put_event("capycal-series.ics", &ics("series", SERIES))
            .await
            .unwrap();
        let single = "DTSTART:20270104T130000Z\nDTEND:20270104T140000Z\nSUMMARY:Single";
        sink.put_event("capycal-single.ics", &ics("single", single))
            .await
            .unwrap();
        assert_eq!(
            count_events_by_marker(&calendar.id, SYNC, "src").unwrap(),
            2
        );
        let (deleted, errors) =
            delete_events_by_marker(&calendar.id, SYNC, "src", &|_| {}).unwrap();
        assert_eq!((deleted, errors), (2, 0));
        assert!(calendar.occurrences().is_empty());
    }

    /// Create an event directly in `calendar`, as a user would.
    fn add_event(
        calendar: &TestCalendar,
        title: &str,
        start: &str,
        end: &str,
        weekly_count: Option<usize>,
    ) {
        objc2::rc::autoreleasepool(|_| unsafe {
            let store = EventKitClient::create_store();
            let ek_calendar = EventKitClient::find_calendar(&store, &calendar.id).unwrap();
            let event = EKEvent::eventWithEventStore(&store);
            event.setCalendar(Some(&ek_calendar));
            event.setTitle(Some(&NSString::from_str(title)));
            let time = |value: &str| {
                if value.len() == 8 {
                    IcalTime::Date(value.to_string())
                } else {
                    IcalTime::DateTime {
                        value: value.to_string(),
                        tzid: None,
                    }
                }
            };
            // Calendar.app ends an all-day event at the last second of its
            // last day; `end` is the iCal end
            let all_day = start.len() == 8;
            event.setAllDay(all_day);
            event.setStartDate(Some(&ical_time_to_nsdate(&time(start)).unwrap()));
            let end = ical_time_to_nsdate(&time(end)).unwrap();
            let end = end.dateByAddingTimeInterval(if all_day { -1.0 } else { 0.0 });
            event.setEndDate(Some(&end));
            event.setAlarms(Some(&NSArray::from_retained_slice(&[
                EKAlarm::alarmWithRelativeOffset(-900.0),
            ])));
            if let Some(count) = weekly_count {
                let rule = Recurrence {
                    frequency: Frequency::Weekly,
                    interval: 1,
                    days_of_week: vec![],
                    days_of_month: vec![],
                    months_of_year: vec![],
                    weeks_of_year: vec![],
                    days_of_year: vec![],
                    set_positions: vec![],
                    end: Some(RecurrenceEnd::Count(count as u64)),
                };
                let rule = recurrence_rule(&rule).unwrap();
                event.setRecurrenceRules(Some(&NSArray::from_retained_slice(&[rule])));
            }
            store
                .saveEvent_span_error(&event, EKSpan::FutureEvents)
                .unwrap();
        });
    }

    /// Calendar.app to Calendar.app: what the source reads, what the sink
    /// writes, and that a full sync finds nothing to change afterwards.
    #[tokio::test]
    #[ignore = "uses Calendar.app, run `make test-eventkit` on a Mac"]
    async fn live_calendar_app_source_to_sink() {
        let source_calendar = TestCalendar::new("source");
        let sink_calendar = TestCalendar::new("sink");
        add_event(&source_calendar, "One day", "20270105", "20270106", None);
        add_event(&source_calendar, "Two days", "20270110", "20270112", None);
        add_event(
            &source_calendar,
            "Timed",
            "20270104T090000Z",
            "20270104T103000Z",
            None,
        );
        add_event(
            &source_calendar,
            "Weekly",
            "20270111T140000Z",
            "20270111T150000Z",
            Some(3),
        );

        let source = EventKitClient::new(&source_calendar.id, 30, 365, true).unwrap();
        let (events, removed) = source
            .fetch_changes(&mut SyncState::default())
            .await
            .unwrap();
        assert!(removed.is_empty());
        let icals: Vec<&str> = events.iter().map(|e| e.calendar_data.as_str()).collect();
        let find = |title: &str| -> Vec<&str> {
            icals
                .iter()
                .copied()
                .filter(|i| i.contains(&format!("SUMMARY:{title}\r\n")))
                .collect()
        };
        // All-day events end at the following midnight, also when EventKit
        // reports the last second of the last day
        let one_day = find("One day");
        assert_eq!(one_day.len(), 1, "{icals:#?}");
        assert!(
            one_day[0].contains("DTSTART;VALUE=DATE:20270105"),
            "{}",
            one_day[0]
        );
        assert!(
            one_day[0].contains("DTEND;VALUE=DATE:20270106"),
            "{}",
            one_day[0]
        );
        let two_days = find("Two days");
        assert!(
            two_days[0].contains("DTEND;VALUE=DATE:20270112"),
            "{}",
            two_days[0]
        );
        let timed = find("Timed");
        assert!(timed[0].contains("BEGIN:VALARM"), "{}", timed[0]);
        // A source series comes as single occurrences
        let weekly = find("Weekly");
        assert_eq!(weekly.len(), 3, "{icals:#?}");
        assert!(weekly.iter().all(|i| !i.contains("RRULE")), "{weekly:#?}");

        let sink = sink_calendar.sink();
        let fields: std::collections::HashSet<String> =
            ["SUMMARY".to_string(), "VALARM".to_string()].into();
        let now = chrono::Utc::now();
        let mut copies = Vec::new();
        for event in &events {
            let copy = crate::ical::process_event(
                &event.calendar_data,
                &fields,
                &[],
                &crate::ical::transform::TransformContext {
                    source_name: "src".to_string(),
                    source_url: String::new(),
                    sync_name: SYNC.to_string(),
                },
                &event.uid,
                &format!("capycal-{}", event.uid),
                SYNC,
                "src",
                now,
            )
            .unwrap();
            sink.put_event(&format!("capycal-{}.ics", event.uid), &copy)
                .await
                .unwrap();
            copies.push((event.uid.clone(), copy));
        }

        let occurrences = sink_calendar.occurrences();
        assert_eq!(occurrences.len(), 6, "{occurrences:?}");
        let all_day: Vec<(&str, f64)> = occurrences
            .iter()
            .filter(|o| o.4)
            .map(|o| (o.3.as_str(), ((o.2 - o.1) / 86400.0).round()))
            .collect();
        assert_eq!(all_day, [("One day", 1.0), ("Two days", 2.0)]);

        // A full sync right after the copy changes nothing
        let synced = sink.list_synced(SYNC, "src").await.unwrap();
        assert_eq!(synced.len(), 6);
        for stored in &synced {
            let uid = stored.source_uid.as_deref().unwrap();
            let (_, copy) = copies.iter().find(|(u, _)| u == uid).unwrap();
            assert!(
                sink.is_unchanged(&stored.stored, copy),
                "{uid} would be rewritten: {:?}\n{copy}",
                stored.stored
            );
        }
    }
}
