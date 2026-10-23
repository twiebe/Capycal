//! Pure conversion between EventKit event data and iCal, compiled on all
//! platforms so it is testable on Linux. The ObjC adapter (`client.rs`) only
//! reads `EKEvent` into `EkEventData` and applies `SinkUpdate` to an `EKEvent`,
//! including all NSCalendar/NSTimeZone date conversions.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use chrono::{DateTime, Utc};

use super::tzid::resolve_timezones;
use crate::ical::marker::{MARKER_SOURCE, MARKER_SYNC, extract_source_info};
use crate::ical::transform::{parse_ical_duration, secs_to_ical_trigger};
use crate::ical::{
    PRODID, escape_text, extract_tzid, format_utc, is_date_value, unescape_text, unfold,
    valarm_lines,
};

/// Max predicate range in days (~4 years). Apple EventKit silently returns
/// empty results for ranges larger than this.
pub const MAX_QUERY_RANGE_DAYS: i64 = 1460;

/// Prefix of the notes line that marks a capycal-created event, since EventKit
/// keeps no custom iCal properties. Current format:
/// `[capycal:sync=<sync>;source=<source calendar>;uid=<source uid>]`, values
/// percent-encoded where they contain `%`, `;`, `=`, `]` or line breaks.
/// Older versions wrote `[capycal:<source calendar>]`, which is still read.
const MARKER_PREFIX: &str = "[capycal:";

/// The notes marker of a capycal-created event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotesMarker {
    pub source: String,
    /// Sync name and source UID, None for a legacy `[capycal:<source>]` marker.
    pub ids: Option<(String, String)>,
}

impl NotesMarker {
    pub fn line(&self) -> String {
        match &self.ids {
            Some((sync, uid)) => format!(
                "{MARKER_PREFIX}sync={};source={};uid={}]",
                encode(sync),
                encode(&self.source),
                encode(uid)
            ),
            None => format!("{MARKER_PREFIX}{}]", self.source),
        }
    }

    fn parse(line: &str) -> Option<Self> {
        let rest = line
            .trim_end()
            .strip_prefix(MARKER_PREFIX)?
            .strip_suffix(']')?;
        let (mut sync, mut source, mut uid) = (None, None, None);
        for field in rest.split(';') {
            match field.split_once('=') {
                Some(("sync", v)) => sync = decode(v),
                Some(("source", v)) => source = decode(v),
                Some(("uid", v)) => uid = decode(v),
                _ => {
                    sync = None;
                    break;
                }
            }
        }
        Some(match (sync, source, uid) {
            (Some(sync), Some(source), Some(uid)) => Self {
                source,
                ids: Some((sync, uid)),
            },
            _ => Self {
                source: rest.to_string(),
                ids: None,
            },
        })
    }

    pub fn sync(&self) -> Option<&str> {
        self.ids.as_ref().map(|(sync, _)| sync.as_str())
    }

    pub fn source_uid(&self) -> Option<&str> {
        self.ids.as_ref().map(|(_, uid)| uid.as_str())
    }

    /// Whether `sync` (reading `source`) created the event. A legacy marker
    /// only names the source, so it matches every sync of that source.
    pub fn belongs_to(&self, sync: &str, source: &str) -> bool {
        match self.sync() {
            Some(own) => own == sync,
            None => self.source == source,
        }
    }
}

fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '%' | ';' | '=' | ']' | '\r' | '\n' => out.push_str(&format!("%{:02X}", c as u8)),
            c => out.push(c),
        }
    }
    out
}

fn decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = value.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    Free,
    NotSupported,
    /// Busy, tentative or unavailable.
    Busy,
}

/// An alarm as read from EventKit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Alarm {
    /// Absolute fire date as a Unix timestamp.
    Absolute(f64),
    /// Offset in seconds relative to the event start.
    Relative(f64),
}

impl Alarm {
    /// iCal TRIGGER line, None for an unset (epoch) absolute date.
    fn trigger_line(self) -> Option<String> {
        match self {
            Alarm::Absolute(ts) => ical_utc(ts).map(|s| format!("TRIGGER;VALUE=DATE-TIME:{s}")),
            Alarm::Relative(offset) => Some(format!(
                "TRIGGER:{}",
                secs_to_ical_trigger(offset.round() as i64)
            )),
        }
    }
}

fn trigger_lines(alarms: &[Alarm]) -> Vec<String> {
    alarms.iter().filter_map(|a| a.trigger_line()).collect()
}

/// Event boundaries, already converted to local values by the ObjC side.
#[derive(Debug, Clone, PartialEq)]
pub enum EkTimes {
    /// Local calendar dates "YYYYMMDD" of the all-day boundaries.
    AllDay { start: String, end: String },
    /// "YYYYMMDDTHHMMSS" in `tzid`. A boundary without a local value (no or
    /// unknown timezone) is emitted in UTC from the event timestamps.
    Timed {
        tzid: Option<String>,
        start_local: Option<String>,
        end_local: Option<String>,
    },
}

/// Plain data of one EventKit event occurrence.
#[derive(Debug, Clone, PartialEq)]
pub struct EkEventData {
    /// calendarItemExternalIdentifier, empty if unset.
    pub base_uid: String,
    pub start_ts: f64,
    pub end_ts: f64,
    pub times: EkTimes,
    pub title: String,
    pub location: Option<String>,
    pub url: Option<String>,
    pub notes: Option<String>,
    pub availability: Availability,
    pub alarms: Vec<Alarm>,
}

impl EkEventData {
    /// Convert the occurrence to iCal. Each occurrence gets a unique UID based
    /// on its start date, so recurring events become individual events.
    pub fn to_ical(&self, now: DateTime<Utc>) -> String {
        // Must match the href generated in fetch_changes.
        let uid = occurrence_href(&self.base_uid, self.start_ts);

        let mut lines = vec![
            "BEGIN:VCALENDAR".to_string(),
            "VERSION:2.0".to_string(),
            format!("PRODID:{PRODID}"),
            "BEGIN:VEVENT".to_string(),
            format!("UID:{uid}"),
            format!(
                "DTSTAMP:{}",
                format_utc(now.timestamp()).unwrap_or_default()
            ),
        ];

        match &self.times {
            EkTimes::AllDay { start, end } => {
                lines.push(format!("DTSTART;VALUE=DATE:{start}"));
                lines.push(format!("DTEND;VALUE=DATE:{end}"));
            }
            EkTimes::Timed {
                tzid,
                start_local,
                end_local,
            } => {
                // Falling back to UTC keeps DTSTART/DTEND present, otherwise
                // put_event would never set start/end on the sink event.
                lines.extend(timed_line("DTSTART", tzid, start_local, self.start_ts));
                lines.extend(timed_line("DTEND", tzid, end_local, self.end_ts));
            }
        }

        lines.push(format!("SUMMARY:{}", escape_text(&self.title)));

        match self.availability {
            Availability::Free => lines.push("TRANSP:TRANSPARENT".to_string()),
            Availability::NotSupported => {}
            Availability::Busy => lines.push("TRANSP:OPAQUE".to_string()),
        }

        if let Some(loc) = &self.location
            && !loc.is_empty()
        {
            lines.push(format!("LOCATION:{}", escape_text(loc)));
        }

        // A URI, not text: written as it is
        if let Some(url) = &self.url
            && !url.is_empty()
        {
            lines.push(format!("URL:{url}"));
        }

        if let Some(notes) = &self.notes {
            let clean = strip_marker(notes);
            if !clean.is_empty() {
                lines.push(format!("DESCRIPTION:{}", escape_text(&clean)));
            }
        }

        // Created by capycal: emit X-CAPYCAL-SOURCE so is_synced_event()
        // prevents sync loops.
        if let Some(marker) = self.notes.as_deref().and_then(find_marker) {
            lines.push(format!("{MARKER_SOURCE}:{}/{uid}", marker.source));
        }

        // No RRULE - each occurrence is synced as an individual event.
        // This ensures deleted occurrences don't appear in the sink.

        for trigger in trigger_lines(&self.alarms) {
            lines.extend(valarm_lines(trigger));
        }

        lines.push("END:VEVENT".to_string());
        lines.push("END:VCALENDAR".to_string());

        lines.join("\r\n")
    }
}

fn timed_line(
    name: &str,
    tzid: &Option<String>,
    local: &Option<String>,
    ts: f64,
) -> Option<String> {
    match (tzid, local) {
        (Some(tz), Some(local)) => Some(format!("{name};TZID={tz}:{local}")),
        _ => ical_utc(ts).map(|s| format!("{name}:{s}")),
    }
}

/// Unique href per occurrence: base ID + start timestamp rounded to the minute.
pub fn occurrence_href(base_uid: &str, start_ts: f64) -> String {
    format!("{base_uid}_{}", round_to_minute(start_ts))
}

/// Source etag of an occurrence.
pub fn etag(modified_ts: f64, end_ts: f64, alarms: &[Alarm], availability: Availability) -> String {
    // endDate is included so that if Exchange delivers a lastModifiedDate
    // change before the full event data (endDate) is refreshed in the local
    // EventKit cache, the stale sync is re-detected on the next cycle.
    let mut etag = format!("{:.0}-{:.0}", modified_ts, end_ts);
    // Alarm edits don't reliably bump lastModifiedDate.
    let triggers = trigger_lines(alarms);
    if !triggers.is_empty() {
        etag.push('-');
        etag.push_str(&triggers.join(","));
    }
    // Lets events marked free before TRANSP support resync once.
    if availability == Availability::Free {
        etag.push_str("-free");
    }
    etag
}

/// Round a timestamp to the nearest minute. Calendar events start on minute
/// boundaries, so this eliminates floating-point instability in EventKit's
/// computed occurrence start times for recurring events.
fn round_to_minute(ts: f64) -> i64 {
    (ts / 60.0).round() as i64 * 60
}

/// UTC datetime string "YYYYMMDDTHHMMSSZ".
/// Returns None for the epoch, which EventKit uses for unset dates.
fn ical_utc(ts: f64) -> Option<String> {
    if ts == 0.0 {
        return None;
    }
    format_utc(ts as i64)
}

/// Split a day-offset range into chunks of at most MAX_QUERY_RANGE_DAYS, since
/// EventKit silently returns nothing for longer predicate ranges.
pub fn query_chunks(start_days: i64, end_days: i64) -> Vec<(i64, i64)> {
    let mut chunks = Vec::new();
    let mut pos = start_days;
    while pos < end_days {
        let chunk_end = (pos + MAX_QUERY_RANGE_DAYS).min(end_days);
        chunks.push((pos, chunk_end));
        pos = chunk_end;
    }
    chunks
}

/// The first marker line of the notes, new or legacy format.
pub fn find_marker(notes: &str) -> Option<NotesMarker> {
    notes.lines().find_map(NotesMarker::parse)
}

fn without_marker(notes: &str) -> String {
    notes
        .lines()
        .filter(|l| !l.starts_with(MARKER_PREFIX))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Notes without marker lines, trimmed.
fn strip_marker(notes: &str) -> String {
    without_marker(notes).trim().to_string()
}

/// Notes with every marker line replaced by `marker` at the end.
pub fn with_marker(notes: &str, marker: &NotesMarker) -> String {
    let rest = without_marker(notes);
    let rest = rest.trim_end();
    if rest.is_empty() {
        marker.line()
    } else {
        format!("{rest}\n{}", marker.line())
    }
}

/// A DTSTART/DTEND value, converted to NSDate on the ObjC side.
#[derive(Debug, Clone, PartialEq)]
pub enum IcalTime {
    /// "YYYYMMDD", an all-day boundary in the local timezone.
    Date(String),
    /// "YYYYMMDDTHHMMSS[Z]" in `tzid`, or UTC if absent.
    DateTime { value: String, tzid: Option<String> },
}

/// A VALARM trigger to set on the sink event.
#[derive(Debug, Clone, PartialEq)]
pub enum IcalAlarm {
    /// UTC "YYYYMMDDTHHMMSS[Z]".
    Absolute(String),
    /// Seconds relative to the event start.
    Relative(i64),
}

/// Changes to apply to a sink EKEvent, parsed from incoming iCal. A text
/// field the iCal leaves out is cleared, as a CalDAV put would; other
/// `None` fields leave the event's current value untouched.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SinkUpdate {
    pub title: Option<String>,
    pub location: Option<String>,
    pub url: Option<String>,
    pub notes: Option<String>,
    pub start: Option<IcalTime>,
    pub end: Option<IcalTime>,
    /// DURATION in seconds; only applies without DTEND (RFC 5545 section 3.6.1).
    pub duration: Option<i64>,
    pub transparent: bool,
    pub alarms: Vec<IcalAlarm>,
    /// From X-CAPYCAL-SOURCE and X-CAPYCAL-SYNC, written as notes marker.
    pub marker: Option<NotesMarker>,
}

impl SinkUpdate {
    /// The notes to write: the description with the marker line.
    pub fn notes_with_marker(&self) -> String {
        let notes = self.notes.as_deref().unwrap_or("");
        match &self.marker {
            Some(marker) => with_marker(notes, marker),
            None => notes.to_string(),
        }
    }

    /// All-day flag implied by DTSTART, so a change in either direction is applied.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub fn all_day(&self) -> Option<bool> {
        self.start
            .as_ref()
            .map(|start| matches!(start, IcalTime::Date(_)))
    }
}

/// The master VEVENT of `ical_data` (the one without RECURRENCE-ID) as a
/// sink update. Overridden occurrences of a series come from `parse_series`.
pub fn parse_ical(ical_data: &str) -> SinkUpdate {
    let unfolded = resolve_timezones(&unfold(ical_data));
    let blocks = vevent_blocks(&unfolded);
    let master = blocks
        .iter()
        .find(|b| !is_override(b))
        .or(blocks.first())
        .map(Vec::as_slice)
        .unwrap_or_default();
    let mut update = parse_vevent(master);
    let sync = master
        .iter()
        .filter_map(|l| parse_ical_line(l))
        .find(|(prop, _, _)| *prop == MARKER_SYNC)
        .map(|(_, _, value)| value.trim().to_string());
    update.marker = extract_source_info(&unfolded).map(|(source, uid)| NotesMarker {
        source,
        ids: sync.map(|sync| (sync, uid)),
    });
    update
}

/// The lines of each top-level VEVENT, including its BEGIN and END.
fn vevent_blocks(unfolded: &str) -> Vec<Vec<&str>> {
    let mut blocks = Vec::new();
    let mut current: Option<Vec<&str>> = None;
    for line in unfolded.lines() {
        let line = line.trim_end();
        match line {
            "BEGIN:VEVENT" => current = Some(vec![line]),
            "END:VEVENT" => {
                if let Some(mut block) = current.take() {
                    block.push(line);
                    blocks.push(block);
                }
            }
            _ => {
                if let Some(block) = &mut current {
                    block.push(line);
                }
            }
        }
    }
    blocks
}

fn is_override(block: &[&str]) -> bool {
    block
        .iter()
        .any(|l| parse_ical_line(l).is_some_and(|(prop, _, _)| prop == "RECURRENCE-ID"))
}

/// The event properties of one VEVENT, without those of its VALARMs.
fn parse_vevent(lines: &[&str]) -> SinkUpdate {
    let mut update = SinkUpdate::default();
    let mut in_valarm = false;

    for line in lines {
        let Some((prop, params, value)) = parse_ical_line(line) else {
            continue;
        };

        // VALARM properties (e.g. its DESCRIPTION) must not leak into the event
        if in_valarm {
            if prop == "END" && value == "VALARM" {
                in_valarm = false;
            } else if prop == "TRIGGER" {
                if params.contains("VALUE=DATE-TIME") {
                    update.alarms.push(IcalAlarm::Absolute(value.to_string()));
                } else if let Some(secs) = parse_ical_duration(value) {
                    update.alarms.push(IcalAlarm::Relative(secs));
                }
            }
            continue;
        }

        match prop {
            "BEGIN" if value == "VALARM" => in_valarm = true,
            "TRANSP" => update.transparent = value == "TRANSPARENT",
            "SUMMARY" => update.title = Some(unescape_text(value)),
            "LOCATION" => update.location = Some(unescape_text(value)),
            "URL" => update.url = Some(value.to_string()),
            "DESCRIPTION" => update.notes = Some(unescape_text(value)),
            "DTSTART" => update.start = Some(ical_time(line, value)),
            "DURATION" => update.duration = parse_ical_duration(value),
            "DTEND" => update.end = Some(ical_time(line, value)),
            _ => {}
        }
    }
    update
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Frequency {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RecurrenceEnd {
    Count(u64),
    Until(IcalTime),
}

/// An RRULE in the terms of `EKRecurrenceRule`.
#[derive(Debug, Clone, PartialEq)]
pub struct Recurrence {
    pub frequency: Frequency,
    pub interval: i64,
    /// EventKit weekday (1 = Sunday ... 7 = Saturday) and week number
    /// (0 = every week, -1 = last, ...).
    pub days_of_week: Vec<(i64, i64)>,
    pub days_of_month: Vec<i64>,
    pub months_of_year: Vec<i64>,
    pub weeks_of_year: Vec<i64>,
    pub days_of_year: Vec<i64>,
    pub set_positions: Vec<i64>,
    pub end: Option<RecurrenceEnd>,
}

/// A recurring event for the sink: the rule on the master event plus the
/// occurrences to remove or change afterwards.
#[derive(Debug, Clone, PartialEq)]
pub struct Series {
    pub rule: Recurrence,
    /// Original starts of the occurrences to remove (EXDATE and cancelled
    /// overrides).
    pub removed: Vec<IcalTime>,
    /// Original start (RECURRENCE-ID) and content of changed occurrences.
    pub changed: Vec<(IcalTime, SinkUpdate)>,
    /// RDATE values, which EventKit cannot represent; they are left out.
    pub ignored_rdates: usize,
}

/// The recurrence of `ical_data`, `None` for a single event, or why EventKit
/// cannot represent it.
pub fn parse_series(ical_data: &str) -> Result<Option<Series>, String> {
    let unfolded = resolve_timezones(&unfold(ical_data));
    let blocks = vevent_blocks(&unfolded);
    let Some(master) = blocks.iter().find(|b| !is_override(b)) else {
        return Ok(None);
    };
    let mut rule = None;
    let mut removed = Vec::new();
    let mut ignored_rdates = 0;
    for line in master {
        let Some((prop, _, value)) = parse_ical_line(line) else {
            continue;
        };
        match prop {
            "RRULE" if rule.is_some() => {
                return Err("more than one RRULE is not supported by Calendar.app".to_string());
            }
            "RRULE" => rule = Some(parse_rrule(value)?),
            "EXDATE" => removed.extend(value.split(',').map(|v| ical_time(line, v))),
            "RDATE" => ignored_rdates += value.split(',').count(),
            _ => {}
        }
    }
    let Some(rule) = rule else {
        return Ok(None);
    };
    let marker = parse_ical(ical_data).marker;
    let mut changed = Vec::new();
    for block in blocks.iter().filter(|b| is_override(b)) {
        let mut recurrence_id = None;
        let mut cancelled = false;
        for line in block {
            match parse_ical_line(line) {
                Some(("RECURRENCE-ID", _, value)) => recurrence_id = Some(ical_time(line, value)),
                Some(("STATUS", _, "CANCELLED")) => cancelled = true,
                _ => {}
            }
        }
        let Some(recurrence_id) = recurrence_id else {
            continue;
        };
        if cancelled {
            removed.push(recurrence_id);
        } else {
            let mut update = parse_vevent(block);
            update.marker = marker.clone();
            changed.push((recurrence_id, update));
        }
    }
    Ok(Some(Series {
        rule,
        removed,
        changed,
        ignored_rdates,
    }))
}

fn parse_rrule(value: &str) -> Result<Recurrence, String> {
    let unsupported = |what: &str| format!("RRULE {what} is not supported by Calendar.app");
    let numbers = |v: &str| -> Result<Vec<i64>, String> {
        v.split(',')
            .map(|n| {
                n.trim()
                    .parse()
                    .map_err(|_| format!("invalid RRULE value {v}"))
            })
            .collect()
    };
    let mut frequency = None;
    let mut rule = Recurrence {
        frequency: Frequency::Daily,
        interval: 1,
        days_of_week: Vec::new(),
        days_of_month: Vec::new(),
        months_of_year: Vec::new(),
        weeks_of_year: Vec::new(),
        days_of_year: Vec::new(),
        set_positions: Vec::new(),
        end: None,
    };
    for part in value.split(';').filter(|p| !p.is_empty()) {
        let (key, v) = part
            .split_once('=')
            .ok_or_else(|| format!("invalid RRULE part {part}"))?;
        match key.to_ascii_uppercase().as_str() {
            "FREQ" => {
                frequency = Some(match v.to_ascii_uppercase().as_str() {
                    "DAILY" => Frequency::Daily,
                    "WEEKLY" => Frequency::Weekly,
                    "MONTHLY" => Frequency::Monthly,
                    "YEARLY" => Frequency::Yearly,
                    _ => return Err(unsupported(part)),
                })
            }
            "INTERVAL" => rule.interval = v.parse().map_err(|_| format!("invalid RRULE {part}"))?,
            "COUNT" => {
                let count = v.parse().map_err(|_| format!("invalid RRULE {part}"))?;
                rule.end = Some(RecurrenceEnd::Count(count));
            }
            "UNTIL" => {
                let until = if v.len() == 8 {
                    IcalTime::Date(v.to_string())
                } else {
                    IcalTime::DateTime {
                        value: v.to_string(),
                        tzid: None,
                    }
                };
                rule.end = Some(RecurrenceEnd::Until(until));
            }
            "BYDAY" => {
                for day in v.split(',') {
                    let day = day.trim().to_ascii_uppercase();
                    let split = day.len().saturating_sub(2);
                    let weekday = match &day[split..] {
                        "SU" => 1,
                        "MO" => 2,
                        "TU" => 3,
                        "WE" => 4,
                        "TH" => 5,
                        "FR" => 6,
                        "SA" => 7,
                        _ => return Err(format!("invalid RRULE {part}")),
                    };
                    let week = match &day[..split] {
                        "" => 0,
                        n => n
                            .trim_start_matches('+')
                            .parse()
                            .map_err(|_| format!("invalid RRULE {part}"))?,
                    };
                    rule.days_of_week.push((weekday, week));
                }
            }
            "BYMONTHDAY" => rule.days_of_month = numbers(v)?,
            "BYMONTH" => rule.months_of_year = numbers(v)?,
            "BYWEEKNO" => rule.weeks_of_year = numbers(v)?,
            "BYYEARDAY" => rule.days_of_year = numbers(v)?,
            "BYSETPOS" => rule.set_positions = numbers(v)?,
            // EventKit uses the user's first weekday
            "WKST" => {}
            _ => return Err(unsupported(part)),
        }
    }
    rule.frequency = frequency.ok_or("RRULE without FREQ")?;
    Ok(rule)
}

fn ical_time(line: &str, value: &str) -> IcalTime {
    if is_date_value(line) {
        IcalTime::Date(value.to_string())
    } else {
        IcalTime::DateTime {
            value: value.to_string(),
            tzid: extract_tzid(line).map(str::to_string),
        }
    }
}

/// Boundaries of a stored sink event.
#[derive(Debug, Clone, PartialEq)]
pub enum StoredTimes {
    /// Local dates "YYYYMMDD" of the first day and of the day after the last.
    AllDay { start: String, end: String },
    /// Unix timestamps.
    Timed { start: f64, end: f64 },
}

/// The fields of a sink event that `apply_update` sets, as stored.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredEvent {
    pub title: String,
    pub location: Option<String>,
    pub url: Option<String>,
    pub notes: Option<String>,
    pub times: StoredTimes,
    pub alarms: Vec<Alarm>,
    /// Whether the event is free, None if the calendar has no availability.
    pub free: Option<bool>,
    /// Part of a series; series are rewritten on every full sync.
    pub recurring: bool,
    /// A timed event without a time zone, which Calendar.app keeps at the
    /// same wall-clock time in every zone.
    pub floating: bool,
}

/// Whether applying `update` would leave `stored` as it is. `to_ts` converts
/// an iCal date-time value in a timezone (UTC if None) to a Unix timestamp,
/// as the sink does when it applies the update. Times the update leaves
/// untouched are not compared; anything that can't be compared counts as a
/// change.
pub fn is_unchanged(
    update: &SinkUpdate,
    stored: &StoredEvent,
    to_ts: impl Fn(&str, Option<&str>) -> Option<f64>,
) -> bool {
    // The occurrences of a stored series are not compared
    if stored.recurring {
        return false;
    }
    // Events written before capycal set the time zone get one
    let zoned = |time: &IcalTime| matches!(time, IcalTime::DateTime { value, tzid } if tzid.is_some() || value.ends_with('Z'));
    if stored.floating && update.start.as_ref().is_some_and(zoned) {
        return false;
    }
    // A missing text is an empty one on both sides
    let text_matches =
        |new: Option<&str>, old: Option<&str>| new.unwrap_or("") == old.unwrap_or("");
    text_matches(update.title.as_deref(), Some(&stored.title))
        && text_matches(update.location.as_deref(), stored.location.as_deref())
        && text_matches(update.url.as_deref(), stored.url.as_deref())
        && text_matches(Some(&update.notes_with_marker()), stored.notes.as_deref())
        && stored.free.is_none_or(|free| free == update.transparent)
        && same_times(update, &stored.times, &to_ts)
        && same_alarms(&update.alarms, &stored.alarms, &to_ts)
}

fn same_times(
    update: &SinkUpdate,
    stored: &StoredTimes,
    to_ts: &impl Fn(&str, Option<&str>) -> Option<f64>,
) -> bool {
    let same_instant = |a: f64, b: f64| a.round() == b.round();
    match stored {
        StoredTimes::AllDay { start, end } => {
            let start = match &update.start {
                Some(IcalTime::Date(new)) if new == start => start,
                None => start,
                Some(_) => return false,
            };
            match (&update.end, update.duration) {
                (Some(IcalTime::Date(new)), _) => new == end,
                (Some(IcalTime::DateTime { .. }), _) => false,
                (None, Some(secs)) => {
                    secs % 86400 == 0
                        && chrono::NaiveDate::parse_from_str(start, "%Y%m%d")
                            .ok()
                            .and_then(|d| {
                                d.checked_add_signed(chrono::Duration::days(secs / 86400))
                            })
                            .is_some_and(|d| d.format("%Y%m%d").to_string() == *end)
                }
                (None, None) => true,
            }
        }
        StoredTimes::Timed { start, end } => {
            let start = match &update.start {
                Some(IcalTime::DateTime { value, tzid }) => {
                    match to_ts(value, tzid.as_deref()) {
                        Some(new) if same_instant(new, *start) => new,
                        Some(_) => return false,
                        // The sink keeps the start it can't convert
                        None => *start,
                    }
                }
                None => *start,
                Some(IcalTime::Date(_)) => return false,
            };
            match (&update.end, update.duration) {
                (Some(IcalTime::DateTime { value, tzid }), _) => {
                    to_ts(value, tzid.as_deref()).is_none_or(|new| same_instant(new, *end))
                }
                (Some(IcalTime::Date(_)), _) => false,
                (None, Some(secs)) => same_instant(start + secs as f64, *end),
                (None, None) => true,
            }
        }
    }
}

/// Alarms compared as sets, ignoring their order.
fn same_alarms(
    new: &[IcalAlarm],
    stored: &[Alarm],
    to_ts: &impl Fn(&str, Option<&str>) -> Option<f64>,
) -> bool {
    fn key(alarm: Alarm) -> (bool, i64) {
        match alarm {
            Alarm::Absolute(ts) => (true, ts.round() as i64),
            Alarm::Relative(secs) => (false, secs.round() as i64),
        }
    }
    let mut new: Vec<_> = new
        .iter()
        .filter_map(|alarm| match alarm {
            IcalAlarm::Absolute(value) => to_ts(value, None).map(Alarm::Absolute),
            IcalAlarm::Relative(secs) => Some(Alarm::Relative(*secs as f64)),
        })
        .map(key)
        .collect();
    let mut stored: Vec<_> = stored.iter().copied().map(key).collect();
    new.sort_unstable();
    stored.sort_unstable();
    new == stored
}

/// Parse an iCal property line into (name, params, value).
/// E.g. "DTSTART;TZID=Europe/Berlin:20240115T130000" -> ("DTSTART", ";TZID=Europe/Berlin", "20240115T130000")
fn parse_ical_line(line: &str) -> Option<(&str, &str, &str)> {
    let colon = line.find(':')?;
    let value = &line[colon + 1..];
    let prefix = &line[..colon];
    let (name, params) = match prefix.find(';') {
        Some(semi) => (&prefix[..semi], &prefix[semi..]),
        None => (prefix, ""),
    };
    Some((name, params, value))
}

#[cfg(test)]
mod tests {
    use super::*;

    // 2024-01-15T09:00:00Z
    const START: f64 = 1705309200.0;
    const END: f64 = START + 3600.0;

    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1700000000, 0).unwrap()
    }

    fn timed() -> EkEventData {
        EkEventData {
            base_uid: "ext-1".to_string(),
            start_ts: START,
            end_ts: END,
            times: EkTimes::Timed {
                tzid: Some("Europe/Berlin".to_string()),
                start_local: Some("20240115T100000".to_string()),
                end_local: Some("20240115T110000".to_string()),
            },
            title: "Meeting".to_string(),
            location: None,
            url: None,
            notes: None,
            availability: Availability::Busy,
            alarms: vec![],
        }
    }

    fn lines(event: &EkEventData) -> Vec<String> {
        event
            .to_ical(now())
            .split("\r\n")
            .map(str::to_string)
            .collect()
    }

    fn has(event: &EkEventData, line: &str) -> bool {
        lines(event).iter().any(|l| l == line)
    }

    fn has_prefix(event: &EkEventData, prefix: &str) -> bool {
        lines(event).iter().any(|l| l.starts_with(prefix))
    }

    #[test]
    fn test_ical_timed_with_tzid() {
        let ical = timed().to_ical(now());
        let expected = [
            "BEGIN:VCALENDAR",
            "VERSION:2.0",
            "PRODID:-//capycal//EN",
            "BEGIN:VEVENT",
            "UID:ext-1_1705309200",
            "DTSTAMP:20231114T221320Z",
            "DTSTART;TZID=Europe/Berlin:20240115T100000",
            "DTEND;TZID=Europe/Berlin:20240115T110000",
            "SUMMARY:Meeting",
            "TRANSP:OPAQUE",
            "END:VEVENT",
            "END:VCALENDAR",
        ]
        .join("\r\n");
        assert_eq!(ical, expected);
    }

    #[test]
    fn test_ical_uid_rounds_start_to_minute() {
        let mut event = timed();
        event.start_ts = START - 0.4;
        assert!(has(&event, "UID:ext-1_1705309200"));
        event.start_ts = START + 31.0;
        assert!(has(&event, "UID:ext-1_1705309260"));
    }

    #[test]
    fn test_ical_all_day() {
        let mut event = timed();
        event.times = EkTimes::AllDay {
            start: "20240115".to_string(),
            end: "20240116".to_string(),
        };
        assert!(has(&event, "DTSTART;VALUE=DATE:20240115"));
        assert!(has(&event, "DTEND;VALUE=DATE:20240116"));
    }

    #[test]
    fn test_ical_utc_fallback() {
        // Unknown timezone: no local values
        let mut event = timed();
        event.times = EkTimes::Timed {
            tzid: Some("Bogus/Zone".to_string()),
            start_local: None,
            end_local: None,
        };
        assert!(has(&event, "DTSTART:20240115T090000Z"));
        assert!(has(&event, "DTEND:20240115T100000Z"));

        // Floating event without timezone
        event.times = EkTimes::Timed {
            tzid: None,
            start_local: None,
            end_local: None,
        };
        assert!(has(&event, "DTSTART:20240115T090000Z"));

        // Epoch means unset: the line is omitted
        event.start_ts = 0.0;
        assert!(!has_prefix(&event, "DTSTART"));
        assert!(has(&event, "DTEND:20240115T100000Z"));
    }

    #[test]
    fn test_ical_location() {
        let mut event = timed();
        event.location = Some(String::new());
        assert!(!has_prefix(&event, "LOCATION"));
        event.location = Some("Room 1, Floor 2".to_string());
        assert!(has(&event, "LOCATION:Room 1\\, Floor 2"));
    }

    #[test]
    fn test_ical_url() {
        let mut event = timed();
        event.url = Some(String::new());
        assert!(!has_prefix(&event, "URL"));
        event.url = Some("https://meet.example.com/a,b;c".to_string());
        assert!(has(&event, "URL:https://meet.example.com/a,b;c"));
    }

    #[test]
    fn test_ical_notes_escaped_and_marker_stripped() {
        let mut event = timed();
        event.notes = Some("Line 1\r\nLine 2\n[capycal:work]\n".to_string());
        assert!(has(&event, "DESCRIPTION:Line 1\\nLine 2"));
        assert!(has(&event, "X-CAPYCAL-SOURCE:work/ext-1_1705309200"));
    }

    #[test]
    fn test_ical_marker_only_notes() {
        let mut event = timed();
        event.notes = Some("[capycal:work]".to_string());
        assert!(!has_prefix(&event, "DESCRIPTION"));
        assert!(has(&event, "X-CAPYCAL-SOURCE:work/ext-1_1705309200"));

        event.notes = Some("  ".to_string());
        assert!(!has_prefix(&event, "DESCRIPTION"));
        assert!(!has_prefix(&event, "X-CAPYCAL-SOURCE"));
    }

    #[test]
    fn test_ical_alarms() {
        let mut event = timed();
        event.alarms = vec![
            Alarm::Relative(-899.6),
            Alarm::Absolute(START - 3600.0),
            Alarm::Absolute(0.0),
        ];
        let lines = lines(&event);
        let triggers: Vec<&String> = lines.iter().filter(|l| l.starts_with("TRIGGER")).collect();
        assert_eq!(
            triggers,
            ["TRIGGER:-PT15M", "TRIGGER;VALUE=DATE-TIME:20240115T080000Z"]
        );
        assert_eq!(lines.iter().filter(|l| *l == "BEGIN:VALARM").count(), 2);
        assert!(lines.contains(&"ACTION:DISPLAY".to_string()));
    }

    #[test]
    fn test_ical_transp() {
        let mut event = timed();
        event.availability = Availability::Free;
        assert!(has(&event, "TRANSP:TRANSPARENT"));
        event.availability = Availability::Busy;
        assert!(has(&event, "TRANSP:OPAQUE"));
        event.availability = Availability::NotSupported;
        assert!(!has_prefix(&event, "TRANSP"));
    }

    #[test]
    fn test_etag() {
        assert_eq!(
            etag(1700000000.4, END, &[], Availability::Busy),
            "1700000000-1705312800"
        );
        assert_eq!(
            etag(0.0, END, &[], Availability::NotSupported),
            "0-1705312800"
        );
        assert_eq!(
            etag(
                1700000000.0,
                END,
                &[Alarm::Relative(-900.0), Alarm::Absolute(START)],
                Availability::Free
            ),
            "1700000000-1705312800-TRIGGER:-PT15M,TRIGGER;VALUE=DATE-TIME:20240115T090000Z-free"
        );
        // Unset absolute alarms don't contribute
        assert_eq!(
            etag(1.0, 2.0, &[Alarm::Absolute(0.0)], Availability::Busy),
            "1-2"
        );
    }

    #[test]
    fn test_occurrence_href() {
        assert_eq!(occurrence_href("abc", START + 10.0), "abc_1705309200");
        assert_eq!(occurrence_href("", START), "_1705309200");
    }

    fn marker(sync: &str, source: &str, uid: &str) -> NotesMarker {
        NotesMarker {
            source: source.to_string(),
            ids: Some((sync.to_string(), uid.to_string())),
        }
    }

    fn legacy(source: &str) -> NotesMarker {
        NotesMarker {
            source: source.to_string(),
            ids: None,
        }
    }

    #[test]
    fn test_marker_line_and_parse() {
        let m = marker("work-to-home", "work", "uid-1");
        assert_eq!(
            m.line(),
            "[capycal:sync=work-to-home;source=work;uid=uid-1]"
        );
        assert_eq!(find_marker(&format!("notes\n{}", m.line())), Some(m));
        assert_eq!(find_marker("x\n[capycal:work]"), Some(legacy("work")));
        assert_eq!(find_marker("[capycal:work"), None);
        assert_eq!(find_marker("see [capycal:work]"), None);
        assert_eq!(find_marker("no marker"), None);
    }

    #[test]
    fn test_marker_escapes_special_characters() {
        let m = marker("a;b=c", "src]", "50%;x]\ny=");
        let line = m.line();
        assert_eq!(line.matches(']').count(), 1, "{line}");
        assert!(!line.contains('\n'));
        assert_eq!(find_marker(&line), Some(m));
        // A broken escape is no new marker
        assert_eq!(
            find_marker("[capycal:sync=a;source=b;uid=%zz]"),
            Some(legacy("sync=a;source=b;uid=%zz"))
        );
    }

    #[test]
    fn test_marker_belongs_to() {
        let m = marker("work-to-home", "work", "u");
        assert!(m.belongs_to("work-to-home", "other"));
        assert!(!m.belongs_to("work-to-cal", "work"));
        assert_eq!(m.sync(), Some("work-to-home"));
        assert_eq!(m.source_uid(), Some("u"));
        let old = legacy("work");
        assert!(old.belongs_to("any", "work"));
        assert!(!old.belongs_to("any", "home"));
    }

    #[test]
    fn test_with_marker_replaces_legacy_and_old_markers() {
        let m = marker("s", "work", "u");
        assert_eq!(with_marker("", &m), m.line());
        assert_eq!(with_marker("notes\n", &m), format!("notes\n{}", m.line()));
        assert_eq!(
            with_marker("a\n[capycal:work]\nb", &m),
            format!("a\nb\n{}", m.line())
        );
        let once = with_marker("notes", &m);
        assert_eq!(with_marker(&once, &m), once);
        assert_eq!(strip_marker("a\n[capycal:work]\nb\n"), "a\nb");
    }

    #[test]
    fn test_query_chunks() {
        assert_eq!(query_chunks(-30, 7), vec![(-30, 7)]);
        let chunks = query_chunks(-20 * 365, 4 * 365);
        assert_eq!(chunks.first().unwrap().0, -20 * 365);
        assert_eq!(chunks.last().unwrap().1, 4 * 365);
        assert!(chunks.iter().all(|(s, e)| e - s <= MAX_QUERY_RANGE_DAYS));
        assert!(chunks.windows(2).all(|w| w[0].1 == w[1].0));
        assert!(query_chunks(5, 5).is_empty());
    }

    fn vevent(body: &str) -> String {
        format!("BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\n{body}\r\nEND:VEVENT\r\nEND:VCALENDAR")
    }

    #[test]
    fn test_parse_all_properties() {
        let ical = vevent(
            "UID:x\r\nSUMMARY:Lunch\r\nLOCATION:Cafe\r\nURL:https://example.com/x\r\nDESCRIPTION:a\\nb\r\n\
             DTSTART;TZID=Europe/Berlin:20240115T120000\r\nDTEND:20240115T120000Z\r\n\
             TRANSP:TRANSPARENT\r\nX-CAPYCAL-SOURCE:work/uid-1\r\nX-CAPYCAL-SYNC:s",
        );
        assert_eq!(
            parse_ical(&ical),
            SinkUpdate {
                title: Some("Lunch".to_string()),
                location: Some("Cafe".to_string()),
                url: Some("https://example.com/x".to_string()),
                notes: Some("a\nb".to_string()),
                start: Some(IcalTime::DateTime {
                    value: "20240115T120000".to_string(),
                    tzid: Some("Europe/Berlin".to_string()),
                }),
                end: Some(IcalTime::DateTime {
                    value: "20240115T120000Z".to_string(),
                    tzid: None,
                }),
                duration: None,
                transparent: true,
                alarms: vec![],
                marker: Some(NotesMarker {
                    source: "work".to_string(),
                    ids: Some(("s".to_string(), "uid-1".to_string())),
                }),
            }
        );
    }

    #[test]
    fn test_parse_folded_lines() {
        let ical = vevent("SUMMARY:Long\r\n  title");
        assert_eq!(parse_ical(&ical).title, Some("Long title".to_string()));
    }

    #[test]
    fn test_parse_all_day() {
        let update = parse_ical(&vevent(
            "DTSTART;VALUE=DATE:20240115\r\nDTEND;VALUE=DATE:20240116",
        ));
        assert_eq!(update.start, Some(IcalTime::Date("20240115".to_string())));
        assert_eq!(update.end, Some(IcalTime::Date("20240116".to_string())));
        assert_eq!(update.all_day(), Some(true));
    }

    #[test]
    fn test_all_day_follows_dtstart() {
        assert_eq!(
            parse_ical(&vevent("DTSTART:20240115T090000Z")).all_day(),
            Some(false)
        );
        assert_eq!(parse_ical(&vevent("SUMMARY:x")).all_day(), None);
    }

    #[test]
    fn test_parse_explicit_date_time_is_not_all_day() {
        let update = parse_ical(&vevent("DTSTART;VALUE=DATE-TIME:20240115T090000Z"));
        assert_eq!(
            update.start,
            Some(IcalTime::DateTime {
                value: "20240115T090000Z".to_string(),
                tzid: None,
            })
        );
    }

    #[test]
    fn test_parse_unescapes_text() {
        let update = parse_ical(&vevent(
            "SUMMARY:Lunch\\, team\r\nLOCATION:Room 1\\; Floor 2\r\nDESCRIPTION:C:\\\\temp\\nnext",
        ));
        assert_eq!(update.title.as_deref(), Some("Lunch, team"));
        assert_eq!(update.location.as_deref(), Some("Room 1; Floor 2"));
        assert_eq!(update.notes.as_deref(), Some("C:\\temp\nnext"));
    }

    #[test]
    fn test_parse_quoted_tzid() {
        let update = parse_ical(&vevent("DTSTART;TZID=\"America/New_York\":20240115T090000"));
        assert_eq!(
            update.start,
            Some(IcalTime::DateTime {
                value: "20240115T090000".to_string(),
                tzid: Some("America/New_York".to_string()),
            })
        );
    }

    #[test]
    fn test_parse_duration_without_dtend() {
        let update = parse_ical(&vevent("DTSTART:20240115T090000Z\r\nDURATION:PT1H30M"));
        assert_eq!(update.end, None);
        assert_eq!(update.duration, Some(5400));
    }

    #[test]
    fn test_parse_transp_defaults_to_opaque() {
        assert!(!parse_ical(&vevent("SUMMARY:x")).transparent);
        assert!(!parse_ical(&vevent("TRANSP:OPAQUE")).transparent);
    }

    #[test]
    fn test_parse_valarm() {
        let update = parse_ical(&vevent(
            "SUMMARY:Event\r\n\
             BEGIN:VALARM\r\nACTION:DISPLAY\r\nDESCRIPTION:Reminder\r\nTRIGGER:-PT15M\r\nEND:VALARM\r\n\
             BEGIN:VALARM\r\nSUMMARY:inner\r\nTRIGGER;VALUE=DATE-TIME:20240115T080000Z\r\nEND:VALARM\r\n\
             BEGIN:VALARM\r\nTRIGGER:bogus\r\nEND:VALARM\r\n\
             LOCATION:after",
        ));
        assert_eq!(update.notes, None);
        assert_eq!(update.title, Some("Event".to_string()));
        assert_eq!(update.location, Some("after".to_string()));
        assert_eq!(
            update.alarms,
            vec![
                IcalAlarm::Relative(-900),
                IcalAlarm::Absolute("20240115T080000Z".to_string()),
            ]
        );
    }

    #[test]
    fn test_parse_empty() {
        assert_eq!(parse_ical(""), SinkUpdate::default());
    }

    #[test]
    fn test_ical_roundtrip() {
        let mut event = timed();
        event.location = Some("Room".to_string());
        event.notes = Some("Agenda\nItem\n[capycal:work]".to_string());
        event.availability = Availability::Free;
        event.alarms = vec![Alarm::Relative(-600.0)];

        let update = parse_ical(&event.to_ical(now()));
        assert_eq!(update.title, Some("Meeting".to_string()));
        assert_eq!(update.location, Some("Room".to_string()));
        assert_eq!(update.notes, Some("Agenda\nItem".to_string()));
        assert!(update.transparent);
        assert_eq!(update.alarms, vec![IcalAlarm::Relative(-600)]);
        assert_eq!(update.marker, Some(legacy("work")));
    }

    #[test]
    fn test_ical_with_new_marker_is_marked_synced() {
        let mut event = timed();
        event.notes = Some(format!("Agenda\n{}", marker("s", "work", "u").line()));
        assert!(has(&event, "DESCRIPTION:Agenda"));
        assert!(has(&event, "X-CAPYCAL-SOURCE:work/ext-1_1705309200"));
        assert!(crate::ical::marker::is_synced_event(&event.to_ical(now())));
    }

    fn to_ts(value: &str, tzid: Option<&str>) -> Option<f64> {
        let naive =
            chrono::NaiveDateTime::parse_from_str(value.trim_end_matches('Z'), "%Y%m%dT%H%M%S")
                .ok()?;
        let ts = match tzid.filter(|_| !value.ends_with('Z')) {
            Some(tz) => naive
                .and_local_timezone(tz.parse::<chrono_tz::Tz>().ok()?)
                .single()?
                .timestamp(),
            None => naive.and_utc().timestamp(),
        };
        Some(ts as f64)
    }

    const SYNCED: &str = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:sink-1\r\n\
        DTSTART;TZID=Europe/Berlin:20240115T100000\r\nDTEND;TZID=Europe/Berlin:20240115T110000\r\n\
        SUMMARY:Meeting\r\nLOCATION:Room\r\nURL:https://example.com/m\r\nDESCRIPTION:Agenda\r\nTRANSP:OPAQUE\r\n\
        X-CAPYCAL-SOURCE:work/u\r\nX-CAPYCAL-SYNC:s\r\n\
        BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT15M\r\nEND:VALARM\r\n\
        BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER;VALUE=DATE-TIME:20240115T080000Z\r\nEND:VALARM\r\n\
        END:VEVENT\r\nEND:VCALENDAR";

    fn stored() -> StoredEvent {
        StoredEvent {
            title: "Meeting".to_string(),
            location: Some("Room".to_string()),
            url: Some("https://example.com/m".to_string()),
            notes: Some(format!("Agenda\n{}", marker("s", "work", "u").line())),
            times: StoredTimes::Timed {
                start: START,
                end: END,
            },
            alarms: vec![Alarm::Relative(-900.0), Alarm::Absolute(START - 3600.0)],
            free: Some(false),
            recurring: false,
            floating: false,
        }
    }

    fn unchanged(ical: &str, stored: &StoredEvent) -> bool {
        is_unchanged(&parse_ical(ical), stored, to_ts)
    }

    #[test]
    fn test_unchanged_when_fields_match() {
        assert!(unchanged(SYNCED, &stored()));
        // A stored series is always rewritten
        let series = StoredEvent {
            recurring: true,
            ..stored()
        };
        assert!(!unchanged(SYNCED, &series));
    }

    #[test]
    fn test_a_text_left_out_clears_the_copy() {
        // The source dropped location, link and notes: the copy loses them
        let bare = SYNCED
            .replace("LOCATION:Room\r\n", "")
            .replace("URL:https://example.com/m\r\n", "")
            .replace("DESCRIPTION:Agenda\r\n", "");
        let update = parse_ical(&bare);
        assert_eq!(update.notes_with_marker(), marker("s", "work", "u").line());
        assert!(!unchanged(&bare, &stored()));
        let cleared = StoredEvent {
            location: None,
            url: None,
            notes: Some(marker("s", "work", "u").line()),
            ..stored()
        };
        assert!(unchanged(&bare, &cleared));
        // Empty and missing are the same
        let empty = StoredEvent {
            location: Some(String::new()),
            ..cleared
        };
        assert!(unchanged(&bare, &empty));
    }

    #[test]
    fn test_changed_when_a_field_differs() {
        let changes: [fn(&mut StoredEvent); 10] = [
            |s| s.floating = true,
            |s| s.title = "Other".to_string(),
            |s| s.location = None,
            |s| s.url = None,
            |s| s.notes = Some(marker("s", "work", "u").line()),
            |s| s.notes = Some("Agenda\n[capycal:work]".to_string()),
            |s| {
                s.times = StoredTimes::Timed {
                    start: START + 60.0,
                    end: END,
                }
            },
            |s| {
                s.times = StoredTimes::Timed {
                    start: START,
                    end: END + 60.0,
                }
            },
            |s| s.free = Some(true),
            |s| {
                s.times = StoredTimes::AllDay {
                    start: "20240115".to_string(),
                    end: "20240116".to_string(),
                }
            },
        ];
        for change in changes {
            let mut stored = stored();
            change(&mut stored);
            assert!(!unchanged(SYNCED, &stored), "{stored:?}");
        }
    }

    #[test]
    fn test_alarms_compare_as_sets() {
        let mut stored = stored();
        stored.alarms.reverse();
        assert!(unchanged(SYNCED, &stored));

        let changes: [fn(&mut Vec<Alarm>); 3] = [
            |a| a.push(Alarm::Relative(-60.0)),
            |a| {
                a.pop();
            },
            |a| a[0] = Alarm::Relative(-600.0),
        ];
        for change in changes {
            let mut stored = stored.clone();
            change(&mut stored.alarms);
            assert!(!unchanged(SYNCED, &stored), "{:?}", stored.alarms);
        }
    }

    #[test]
    fn test_availability_not_compared_without_calendar_support() {
        let mut stored = stored();
        stored.free = None;
        assert!(unchanged(&SYNCED.replace("OPAQUE", "TRANSPARENT"), &stored));
    }

    #[test]
    fn test_times_compare_as_instants() {
        let utc = SYNCED
            .replace(
                "DTSTART;TZID=Europe/Berlin:20240115T100000",
                "DTSTART:20240115T090000Z",
            )
            .replace(
                "DTEND;TZID=Europe/Berlin:20240115T110000",
                "DTEND:20240115T100000Z",
            );
        assert!(unchanged(&utc, &stored()));
    }

    #[test]
    fn test_duration_end_compares_with_stored_end() {
        let duration = SYNCED.replace("DTEND;TZID=Europe/Berlin:20240115T110000", "DURATION:PT1H");
        assert!(unchanged(&duration, &stored()));
        let longer = duration.replace("PT1H", "PT2H");
        assert!(!unchanged(&longer, &stored()));
    }

    #[test]
    fn test_all_day_compares_dates() {
        let all_day = SYNCED
            .replace(
                "DTSTART;TZID=Europe/Berlin:20240115T100000",
                "DTSTART;VALUE=DATE:20240115",
            )
            .replace("DTEND;TZID=Europe/Berlin:20240115T110000", "DURATION:P2D");
        let mut stored = stored();
        stored.times = StoredTimes::AllDay {
            start: "20240115".to_string(),
            end: "20240117".to_string(),
        };
        assert!(unchanged(&all_day, &stored));
        assert!(!unchanged(&all_day.replace("P2D", "P1D"), &stored));
        let dtend = all_day.replace("DURATION:P2D", "DTEND;VALUE=DATE:20240117");
        assert!(unchanged(&dtend, &stored));
        assert!(!unchanged(
            &dtend.replace(":20240115", ":20240116"),
            &stored
        ));
        // A timed event for a stored all-day one toggles the flag
        assert!(!unchanged(SYNCED, &stored));
    }

    const SERIES: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n\
        BEGIN:VEVENT\r\nUID:s\r\nDTSTART;TZID=Europe/Berlin:20240115T090000\r\n\
        DTEND;TZID=Europe/Berlin:20240115T100000\r\nSUMMARY:Weekly\r\n\
        RRULE:FREQ=WEEKLY;INTERVAL=2;BYDAY=MO,-1FR;UNTIL=20241231T235959Z;WKST=MO\r\n\
        EXDATE;TZID=Europe/Berlin:20240129T090000,20240212T090000\r\nRDATE:20240301T090000Z\r\n\
        X-CAPYCAL-SOURCE:work/s\r\nX-CAPYCAL-SYNC:w2h\r\nEND:VEVENT\r\n\
        BEGIN:VEVENT\r\nUID:s\r\nRECURRENCE-ID;TZID=Europe/Berlin:20240122T090000\r\n\
        DTSTART;TZID=Europe/Berlin:20240122T110000\r\nDTEND;TZID=Europe/Berlin:20240122T120000\r\n\
        SUMMARY:Moved\r\nEND:VEVENT\r\n\
        BEGIN:VEVENT\r\nUID:s\r\nRECURRENCE-ID;VALUE=DATE:20240226\r\nSTATUS:CANCELLED\r\n\
        DTSTART;TZID=Europe/Berlin:20240226T090000\r\nEND:VEVENT\r\nEND:VCALENDAR";

    fn berlin(value: &str) -> IcalTime {
        IcalTime::DateTime {
            value: value.to_string(),
            tzid: Some("Europe/Berlin".to_string()),
        }
    }

    #[test]
    fn test_parse_ical_reads_the_master_of_a_series() {
        let update = parse_ical(SERIES);
        assert_eq!(update.title.as_deref(), Some("Weekly"));
        assert_eq!(update.start, Some(berlin("20240115T090000")));
        assert_eq!(update.end, Some(berlin("20240115T100000")));
        assert!(update.marker.is_some_and(|m| m.belongs_to("w2h", "work")));
    }

    #[test]
    fn test_parse_series() {
        let series = parse_series(SERIES).unwrap().unwrap();
        assert_eq!(
            series.rule,
            Recurrence {
                frequency: Frequency::Weekly,
                interval: 2,
                days_of_week: vec![(2, 0), (6, -1)],
                days_of_month: vec![],
                months_of_year: vec![],
                weeks_of_year: vec![],
                days_of_year: vec![],
                set_positions: vec![],
                end: Some(RecurrenceEnd::Until(IcalTime::DateTime {
                    value: "20241231T235959Z".to_string(),
                    tzid: None
                })),
            }
        );
        assert_eq!(
            series.removed,
            [
                berlin("20240129T090000"),
                berlin("20240212T090000"),
                IcalTime::Date("20240226".to_string())
            ]
        );
        assert_eq!(series.ignored_rdates, 1);
        let [(recurrence_id, moved)] = series.changed.as_slice() else {
            panic!("one changed occurrence expected: {:?}", series.changed);
        };
        assert_eq!(*recurrence_id, berlin("20240122T090000"));
        assert_eq!(moved.title.as_deref(), Some("Moved"));
        assert_eq!(moved.start, Some(berlin("20240122T110000")));
        // Occurrences keep the marker of the series
        assert!(
            moved
                .marker
                .as_ref()
                .is_some_and(|m| m.belongs_to("w2h", "work"))
        );
    }

    #[test]
    fn test_parse_series_without_or_with_unsupported_rules() {
        let single = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:a\r\nDTSTART:20240115T090000Z\r\nEND:VEVENT\r\nEND:VCALENDAR";
        assert_eq!(parse_series(single), Ok(None));
        let rule = |rrule: &str| {
            parse_series(&single.replace("END:VEVENT", &format!("RRULE:{rrule}\r\nEND:VEVENT")))
        };
        let monthly = rule("FREQ=MONTHLY;BYMONTHDAY=1,-1;COUNT=5;BYSETPOS=-1")
            .unwrap()
            .unwrap();
        assert_eq!(monthly.rule.frequency, Frequency::Monthly);
        assert_eq!(monthly.rule.days_of_month, [1, -1]);
        assert_eq!(monthly.rule.set_positions, [-1]);
        assert_eq!(monthly.rule.end, Some(RecurrenceEnd::Count(5)));
        assert_eq!(
            rule("FREQ=YEARLY;UNTIL=20300101")
                .unwrap()
                .unwrap()
                .rule
                .end,
            Some(RecurrenceEnd::Until(IcalTime::Date("20300101".to_string())))
        );
        for unsupported in [
            "FREQ=HOURLY",
            "FREQ=DAILY;BYHOUR=9",
            "INTERVAL=2",
            "FREQ=WEEKLY;BYDAY=XX",
        ] {
            assert!(rule(unsupported).is_err(), "{unsupported}");
        }
    }
}
