use std::collections::{HashMap, HashSet};
use std::time::Duration;

use chrono::{Datelike, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Timelike, Weekday};
use regex::Regex;

use crate::ical::timezone::{Zone, parse_local, vtimezones};
use crate::ical::{
    extract_tzid, is_date_value, property_name, unescape_text, valarm_lines, value_colon,
};

/// Default end of a time-range filter window (23:59) in minutes since midnight.
const LAST_MINUTE_OF_DAY: u32 = 23 * 60 + 59;

/// A validated transform step, applied to each event in order: its action
/// runs only when every group in `when` has a matching condition.
#[derive(Debug, Clone)]
pub struct Transform {
    pub action: Action,
    /// All groups must match; a group matches when any of its conditions
    /// does. A plain condition is a group of one.
    pub when: Vec<Vec<Condition>>,
}

impl From<Action> for Transform {
    fn from(action: Action) -> Self {
        Transform {
            action,
            when: Vec::new(),
        }
    }
}

/// What a transform does.
#[derive(Debug, Clone)]
pub enum Action {
    /// Set (or insert) a property; `value` may contain placeholders.
    Set {
        field: String,
        value: String,
    },
    /// Regex-replace within a property's value; `replacement` may contain placeholders.
    RegexReplace {
        field: String,
        regex: Regex,
        replacement: String,
    },
    /// Replace `search` with `replacement` in a property's value, the first
    /// `count` times or everywhere.
    ReplaceField {
        field: String,
        search: String,
        replacement: String,
        count: Option<usize>,
    },
    Remove {
        field: String,
    },
    /// Move DTSTART earlier and/or DTEND later. Skips all-day events.
    AddBuffer {
        pre: Option<Duration>,
        post: Option<Duration>,
    },
    /// Add a display alarm `trigger` before the event start. The alarm is
    /// added after `fields`, see `Applied::alarms`.
    AddAlarm {
        trigger: Duration,
    },
    /// Drop the event.
    Skip,
}

/// A test on the master VEVENT as the earlier transforms left it.
#[derive(Debug, Clone)]
pub struct Condition {
    pub test: Test,
    pub negate: bool,
}

#[derive(Debug, Clone)]
pub enum Test {
    /// The unescaped value contains `text`; a missing property is empty.
    Contains {
        field: String,
        text: String,
    },
    /// The unescaped value matches `regex`; a missing property is empty.
    Matches {
        field: String,
        regex: Regex,
    },
    /// The property is missing or empty.
    Empty {
        field: String,
    },
    /// The event starts on one of `days` within the local-time window
    /// (defaults: 00:00 - 23:59). All-day events match on day only.
    TimeRange {
        days: Vec<Weekday>,
        start: Option<NaiveTime>,
        end: Option<NaiveTime>,
    },
    AllDay,
}

impl Condition {
    fn matches(&self, lines: &[String]) -> bool {
        let master = master_event(lines);
        // Every line of a repeated property counts; a missing one is empty
        let values = |field: &str| {
            let values = master_values(&master, field);
            if values.is_empty() {
                vec![String::new()]
            } else {
                values
            }
        };
        let hit = match &self.test {
            Test::Contains { field, text } => {
                values(field).iter().any(|v| v.contains(text.as_str()))
            }
            Test::Matches { field, regex } => values(field).iter().any(|v| regex.is_match(v)),
            Test::Empty { field } => values(field).iter().all(String::is_empty),
            Test::TimeRange { start, end, .. }
                if (start.is_some() || end.is_some()) && is_all_day_event(&master) =>
            {
                // Times say nothing about an all-day event, not even negated
                return false;
            }
            Test::TimeRange { days, start, end } => matches_time_range(&master, days, *start, *end),
            Test::AllDay => is_all_day_event(&master),
        };
        hit != self.negate
    }
}

/// The result of `apply_transforms`.
#[derive(Debug)]
pub struct Applied {
    pub ical: String,
    /// The properties a `Set` set, so they are copied even if not in `fields`.
    pub set_fields: HashSet<String>,
    /// The triggers of the `AddAlarm`s whose conditions matched, to add with
    /// `add_alarms` after `fields`.
    pub alarms: Vec<Duration>,
}

/// Context for placeholder expansion in transform values.
pub struct TransformContext {
    pub source_name: String,
    pub source_url: String,
    pub sync_name: String,
}

fn expand_placeholders(template: &str, ctx: &TransformContext) -> String {
    template
        .replace("{{source_name}}", &ctx.source_name)
        .replace("{{source_url}}", &ctx.source_url)
        .replace("{{sync_name}}", &ctx.sync_name)
}

/// Apply a sequence of transforms to an iCal VEVENT string.
/// Returns None if a `Skip` whose conditions match rejects the event.
///
/// Property transforms apply to every VEVENT of the resource (a recurring
/// series carries overridden occurrences as extra VEVENTs) and only to the
/// event's own properties, never to nested VALARMs or VTIMEZONE definitions.
/// Conditions look at the master VEVENT and decide for the whole resource.
pub fn apply_transforms<'a>(
    ical_data: &str,
    transforms: impl IntoIterator<Item = &'a Transform>,
    ctx: &TransformContext,
) -> Option<Applied> {
    let mut lines: Vec<String> = ical_data.lines().map(|l| l.to_string()).collect();
    let mut set_fields = HashSet::new();
    let mut alarms = Vec::new();

    for transform in transforms {
        if !transform
            .when
            .iter()
            .all(|group| group.iter().any(|c| c.matches(&lines)))
        {
            continue;
        }
        match &transform.action {
            Action::Set { field, value } => {
                let new_line = format!("{field}:{}", expand_placeholders(value, ctx));
                set_or_insert_property(&mut lines, field, &new_line);
                set_fields.insert(field.clone());
            }
            Action::RegexReplace {
                field,
                regex,
                replacement,
            } => {
                let expanded = expand_placeholders(replacement, ctx);
                map_values(&mut lines, field, |v| {
                    regex.replace_all(v, &expanded).into_owned()
                });
            }
            Action::ReplaceField {
                field,
                search,
                replacement,
                count,
            } => {
                let expanded = expand_placeholders(replacement, ctx);
                map_values(&mut lines, field, |v| match count {
                    Some(n) => v.replacen(search.as_str(), &expanded, *n),
                    None => v.replace(search.as_str(), &expanded),
                });
            }
            Action::Remove { field } => remove_property(&mut lines, field),
            Action::AddBuffer { pre, post } => apply_buffer(&mut lines, *pre, *post),
            Action::AddAlarm { trigger } => alarms.push(*trigger),
            Action::Skip => return None,
        }
    }

    Some(Applied {
        ical: lines.join("\r\n"),
        set_fields,
        alarms,
    })
}

/// Add a display alarm per trigger to every VEVENT.
pub fn add_alarms(ical_data: &str, triggers: &[Duration]) -> String {
    let mut lines: Vec<String> = ical_data.lines().map(|l| l.to_string()).collect();
    for trigger in triggers {
        insert_valarm(&mut lines, *trigger);
    }
    lines.join("\r\n")
}

/// Line index ranges `(begin, end)` of the BEGIN:VEVENT / END:VEVENT lines.
fn vevent_blocks(lines: &[String]) -> Vec<(usize, usize)> {
    let mut blocks = Vec::new();
    let mut begin = None;
    for (i, line) in lines.iter().enumerate() {
        match line.trim() {
            "BEGIN:VEVENT" => begin = Some(i),
            "END:VEVENT" => {
                if let Some(b) = begin.take() {
                    blocks.push((b, i));
                }
            }
            _ => {}
        }
    }
    blocks
}

/// Indices of a VEVENT's own properties, skipping nested components (VALARM).
fn event_properties(lines: &[String], (begin, end): (usize, usize)) -> Vec<usize> {
    let mut depth = 0usize;
    let mut props = Vec::new();
    for (i, line) in lines.iter().enumerate().take(end).skip(begin + 1) {
        match property_name(line) {
            "BEGIN" => depth += 1,
            "END" => depth = depth.saturating_sub(1),
            _ if depth == 0 => props.push(i),
            _ => {}
        }
    }
    props
}

/// Indices of the own properties of every VEVENT.
fn all_event_properties(lines: &[String]) -> Vec<usize> {
    vevent_blocks(lines)
        .into_iter()
        .flat_map(|block| event_properties(lines, block))
        .collect()
}

/// Whether the master VEVENT of `ical_data` is cancelled (`STATUS:CANCELLED`).
/// A cancelled single occurrence of a series does not count.
pub fn is_cancelled(ical_data: &str) -> bool {
    let lines: Vec<String> = ical_data
        .lines()
        .map(|l| l.trim_end().to_string())
        .collect();
    master_event(&lines)
        .iter()
        .any(|l| property_name(l) == "STATUS" && l.ends_with(":CANCELLED"))
}

/// The unescaped values of every `field` line in the master VEVENT's
/// properties.
fn master_values(master: &[String], field: &str) -> Vec<String> {
    master
        .iter()
        .filter(|line| property_name(line) == field)
        .filter_map(|line| value_colon(line).map(|i| unescape_text(&line[i + 1..])))
        .collect()
}

/// The own properties of the master VEVENT (the one without RECURRENCE-ID).
fn master_event(lines: &[String]) -> Vec<String> {
    let props: Vec<Vec<usize>> = vevent_blocks(lines)
        .into_iter()
        .map(|block| event_properties(lines, block))
        .collect();
    let is_override = |p: &Vec<usize>| {
        p.iter()
            .any(|&i| property_name(&lines[i]) == "RECURRENCE-ID")
    };
    props
        .iter()
        .find(|p| !is_override(p))
        .or(props.first())
        .map(|p| p.iter().map(|&i| lines[i].clone()).collect())
        .unwrap_or_default()
}

fn set_or_insert_property(lines: &mut Vec<String>, field: &str, new_line: &str) {
    // Back to front, so inserting does not shift the blocks still to come
    for block in vevent_blocks(lines).into_iter().rev() {
        let existing: Vec<usize> = event_properties(lines, block)
            .into_iter()
            .filter(|&i| property_name(&lines[i]) == field)
            .collect();
        match existing.split_first() {
            Some((&first, rest)) => {
                // A repeated line would keep the source's value
                for &i in rest.iter().rev() {
                    lines.remove(i);
                }
                lines[first] = new_line.to_string();
            }
            None => lines.insert(block.1, new_line.to_string()),
        }
    }
}

/// Rewrite the value (after the first ':') of every `field` property.
fn map_values(lines: &mut [String], field: &str, f: impl Fn(&str) -> String) {
    for i in all_event_properties(lines) {
        let line = &lines[i];
        if property_name(line) != field {
            continue;
        }
        if let Some(colon_pos) = value_colon(line) {
            let prefix = &line[..colon_pos + 1];
            let value = &line[colon_pos + 1..];
            let new_value = f(value);
            lines[i] = format!("{prefix}{new_value}");
        }
    }
}

fn remove_property(lines: &mut Vec<String>, field: &str) {
    let mut doomed: Vec<usize> = all_event_properties(lines)
        .into_iter()
        .filter(|&i| property_name(&lines[i]) == field)
        .collect();
    doomed.sort_unstable();
    for i in doomed.into_iter().rev() {
        lines.remove(i);
    }
}

/// System's local IANA timezone, cached for the process lifetime. Falls
/// back to UTC if it can't be determined.
pub(crate) fn local_tz() -> chrono_tz::Tz {
    static TZ: std::sync::OnceLock<chrono_tz::Tz> = std::sync::OnceLock::new();
    *TZ.get_or_init(|| {
        iana_time_zone::get_timezone()
            .ok()
            .and_then(|name| name.parse().ok())
            .unwrap_or(chrono_tz::UTC)
    })
}

/// Convert a DTSTART's naive wall-clock time into the target local timezone.
/// UTC (`Z` suffix) and TZID-qualified times are converted properly; floating
/// times (no timezone info, or an unrecognized TZID) are assumed to already
/// be in the target zone and passed through as-is.
fn to_local_naive(
    ndt: NaiveDateTime,
    tzid: Option<&str>,
    is_utc: bool,
    local: chrono_tz::Tz,
) -> NaiveDateTime {
    if is_utc {
        return ndt.and_utc().with_timezone(&local).naive_local();
    }
    if let Some(name) = tzid
        && let Ok(tz) = name.parse::<chrono_tz::Tz>()
        && let Some(dt) = tz.from_local_datetime(&ndt).earliest()
    {
        return dt.with_timezone(&local).naive_local();
    }
    ndt
}

/// Check if the event's DTSTART matches the given day+time filter.
/// All-day events match on day-of-week only; the time window is ignored.
fn matches_time_range(
    lines: &[String],
    days: &[Weekday],
    start: Option<NaiveTime>,
    end: Option<NaiveTime>,
) -> bool {
    matches_time_range_in_tz(lines, days, start, end, local_tz())
}

fn matches_time_range_in_tz(
    lines: &[String],
    days: &[Weekday],
    start: Option<NaiveTime>,
    end: Option<NaiveTime>,
    local: chrono_tz::Tz,
) -> bool {
    for line in lines {
        let prop = property_name(line);
        if prop != "DTSTART" {
            continue;
        }

        // All-day events have no start time: only a range without times
        // matches them, by day of week. iCal dates have no timezone.
        if is_date_value(line) {
            if start.is_some() || end.is_some() {
                return false;
            }
            let Some(colon) = line.rfind(':') else {
                return false;
            };
            let Some(nd) = NaiveDate::parse_from_str(&line[colon + 1..], "%Y%m%d").ok() else {
                return false;
            };
            return days.contains(&nd.weekday());
        }

        // Extract the datetime value after ':'
        let Some(colon) = line.rfind(':') else {
            return false;
        };
        let dt_str = &line[colon + 1..];
        let is_utc = dt_str.ends_with('Z');
        let bare = dt_str.strip_suffix('Z').unwrap_or(dt_str);

        let Some(ndt) = NaiveDateTime::parse_from_str(bare, "%Y%m%dT%H%M%S").ok() else {
            return false;
        };
        let converted = to_local_naive(ndt, extract_tzid(line), is_utc, local);

        // Check day of week
        if !days.contains(&converted.date().weekday()) {
            return false;
        }

        // Check time window
        let event_minutes = minute_of_day(converted.time());
        let range_start = start.map(minute_of_day).unwrap_or(0);
        let range_end = end.map(minute_of_day).unwrap_or(LAST_MINUTE_OF_DAY);

        // A start after the end wraps around midnight
        return if range_start <= range_end {
            event_minutes >= range_start && event_minutes <= range_end
        } else {
            event_minutes >= range_start || event_minutes <= range_end
        };
    }

    false
}

fn minute_of_day(t: NaiveTime) -> u32 {
    t.hour() * 60 + t.minute()
}

fn is_all_day_event(lines: &[String]) -> bool {
    lines.iter().any(|l| {
        let prop = property_name(l);
        prop == "DTSTART" && is_date_value(l)
    })
}

/// Insert a VALARM block before END:VEVENT with the given trigger duration.
fn insert_valarm(lines: &mut Vec<String>, trigger: Duration) {
    let alarm = valarm_lines(format!("TRIGGER:{}", duration_to_ical_trigger(trigger)));

    // Insert before each END:VEVENT (supports multiple VEVENTs)
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim() == "END:VEVENT" {
            for (j, line) in alarm.iter().enumerate() {
                lines.insert(i + j, line.clone());
            }
            i += alarm.len() + 1;
        } else {
            i += 1;
        }
    }
}

/// Convert a Duration to iCal TRIGGER format: -PT15M, -PT1H, PT0S, etc.
fn duration_to_ical_trigger(dur: Duration) -> String {
    secs_to_ical_trigger(-(dur.as_secs() as i64))
}

/// Format a signed offset in seconds as an iCal TRIGGER duration.
/// Negative values fire before the event start: -900 -> "-PT15M", 300 -> "PT5M".
pub fn secs_to_ical_trigger(offset: i64) -> String {
    let total_secs = offset.unsigned_abs();
    if total_secs == 0 {
        return "-PT0S".to_string();
    }

    let hours = total_secs / 3600;
    let minutes = (total_secs % 3600) / 60;
    let seconds = total_secs % 60;

    let mut parts = String::from(if offset < 0 { "-PT" } else { "PT" });
    if hours > 0 {
        parts.push_str(&format!("{hours}H"));
    }
    if minutes > 0 {
        parts.push_str(&format!("{minutes}M"));
    }
    if seconds > 0 {
        parts.push_str(&format!("{seconds}S"));
    }
    parts
}

/// Parse an iCal duration as used by TRIGGER and DURATION ("-PT15M", "P1D", "-P1DT2H", "-P1W") into
/// a signed offset in seconds.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn parse_ical_duration(s: &str) -> Option<i64> {
    let (sign, rest) = match s.strip_prefix('-') {
        Some(r) => (-1, r),
        None => (1, s.strip_prefix('+').unwrap_or(s)),
    };
    let rest = rest.strip_prefix('P')?;
    let mut total = 0i64;
    let mut num = String::new();
    let mut in_time = false;
    let mut any = false;
    for c in rest.chars() {
        match c {
            '0'..='9' => num.push(c),
            'T' if num.is_empty() => in_time = true,
            _ => {
                let n: i64 = num.parse().ok()?;
                num.clear();
                total += n * match (c, in_time) {
                    ('W', false) => 604800,
                    ('D', false) => 86400,
                    ('H', true) => 3600,
                    ('M', true) => 60,
                    ('S', true) => 1,
                    _ => return None,
                };
                any = true;
            }
        }
    }
    if !any || !num.is_empty() {
        return None;
    }
    Some(sign * total)
}

/// Add time buffers around non-full-day events.
/// Subtracts `pre` from DTSTART and adds `post` to DTEND.
/// Start `pre` earlier and end `post` later. The instance dates of a series
/// (EXDATE, RDATE, RECURRENCE-ID) move with DTSTART, so they keep matching
/// its occurrences. All-day values stay as they are.
fn apply_buffer(lines: &mut [String], pre: Option<Duration>, post: Option<Duration>) {
    let pre_secs = pre.map_or(0, |d| d.as_secs() as i64);
    let post_secs = post.map_or(0, |d| d.as_secs() as i64);
    let zones = vtimezones(&lines.join("\r\n"));
    for block in vevent_blocks(lines) {
        let props = event_properties(lines, block);
        let all_day = props
            .iter()
            .any(|&i| property_name(&lines[i]) == "DTSTART" && is_date_value(&lines[i]));
        if !all_day {
            buffer_event(lines, &props, pre_secs, post_secs, &zones);
        }
    }
}

fn buffer_event(
    lines: &mut [String],
    props: &[usize],
    pre_secs: i64,
    post_secs: i64,
    zones: &HashMap<String, Zone>,
) {
    for &i in props {
        let line = &lines[i];
        let prop = property_name(line);
        let shift = match prop {
            "DTSTART" | "EXDATE" | "RDATE" | "RECURRENCE-ID" => -pre_secs,
            "DTEND" => post_secs,
            "DURATION" => {
                let Some(colon) = line.rfind(':') else {
                    continue;
                };
                let Some(secs) = parse_ical_duration(&line[colon + 1..]) else {
                    continue;
                };
                lines[i] = format!("{}PT{}S", &line[..colon + 1], secs + pre_secs + post_secs);
                continue;
            }
            _ => continue,
        };
        if shift == 0 || is_date_value(line) || line.contains("VALUE=PERIOD") {
            continue;
        }
        let Some(colon) = line.rfind(':') else {
            continue;
        };
        // A local time in a known zone is shifted as an instant, so it lands
        // on a valid time across a daylight saving switch: by the event's
        // VTIMEZONE, else by the IANA name (Calendar.app sends no VTIMEZONE)
        let tzid = extract_tzid(line);
        let zone = tzid.and_then(|tzid| zones.get(tzid));
        let iana = tzid.and_then(|tzid| tzid.parse::<chrono_tz::Tz>().ok());
        let shifted: Option<Vec<String>> = line[colon + 1..]
            .split(',')
            .map(|value| match (zone, iana) {
                (Some(zone), _) => shift_in_zone(value, shift, zone),
                (None, Some(tz)) => shift_in_iana_zone(value, shift, tz),
                (None, None) => shift_ical_datetime(value, shift),
            })
            .collect();
        if let Some(shifted) = shifted {
            lines[i] = format!("{}{}", &line[..colon + 1], shifted.join(","));
        }
    }
}

/// Shift a local iCal datetime in `zone` by `secs` seconds of real time.
fn shift_in_zone(value: &str, secs: i64, zone: &Zone) -> Option<String> {
    let utc = zone.to_utc(parse_local(value)?)? + chrono::Duration::seconds(secs);
    Some(zone.to_local(utc)?.format("%Y%m%dT%H%M%S").to_string())
}

/// Shift a local iCal datetime in the IANA zone `tz` by `secs` seconds of
/// real time.
fn shift_in_iana_zone(value: &str, secs: i64, tz: chrono_tz::Tz) -> Option<String> {
    let local = tz.from_local_datetime(&parse_local(value)?).earliest()?;
    let shifted = local + chrono::Duration::seconds(secs);
    Some(shifted.naive_local().format("%Y%m%dT%H%M%S").to_string())
}

/// Shift an iCal datetime (YYYYMMDDTHHMMSS with optional Z suffix) by
/// `secs` seconds.
fn shift_ical_datetime(dt_str: &str, secs: i64) -> Option<String> {
    let (bare, suffix) = if let Some(s) = dt_str.strip_suffix('Z') {
        (s, "Z")
    } else {
        (dt_str, "")
    };

    let ndt = NaiveDateTime::parse_from_str(bare, "%Y%m%dT%H%M%S").ok()?;
    let shifted = ndt + chrono::Duration::seconds(secs);
    Some(format!("{}{suffix}", shifted.format("%Y%m%dT%H%M%S")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use Weekday::*;

    const WEEKDAYS: &[Weekday] = &[Mon, Tue, Wed, Thu, Fri];

    fn test_ctx() -> TransformContext {
        TransformContext {
            source_name: "work".to_string(),
            source_url: "https://caldav.example.com/cal/".to_string(),
            sync_name: "work-to-personal".to_string(),
        }
    }

    fn apply(ical: &str, transforms: &[Transform]) -> Option<String> {
        apply_transforms(ical, transforms, &test_ctx()).map(|a| add_alarms(&a.ical, &a.alarms))
    }

    fn set(field: &str, value: &str) -> Transform {
        Action::Set {
            field: field.to_string(),
            value: value.to_string(),
        }
        .into()
    }

    fn regex_replace(field: &str, pattern: &str, replacement: &str) -> Transform {
        Action::RegexReplace {
            field: field.to_string(),
            regex: Regex::new(pattern).unwrap(),
            replacement: replacement.to_string(),
        }
        .into()
    }

    fn remove(field: &str) -> Transform {
        Action::Remove {
            field: field.to_string(),
        }
        .into()
    }

    fn buffer(pre_min: Option<u64>, post_min: Option<u64>) -> Transform {
        Action::AddBuffer {
            pre: pre_min.map(|m| Duration::from_secs(m * 60)),
            post: post_min.map(|m| Duration::from_secs(m * 60)),
        }
        .into()
    }

    fn alarm(secs: u64) -> Transform {
        Action::AddAlarm {
            trigger: Duration::from_secs(secs),
        }
        .into()
    }

    fn hm(h: u32, m: u32) -> Option<NaiveTime> {
        NaiveTime::from_hms_opt(h, m, 0)
    }

    #[test]
    fn test_set_transform() {
        let ical = "BEGIN:VEVENT\r\nUID:123\r\nSUMMARY:Original Title\r\nEND:VEVENT";
        let result = apply(ical, &[set("SUMMARY", "Busy ({{source_name}})")]).unwrap();
        assert!(result.contains("SUMMARY:Busy (work)"));
        assert!(!result.contains("Original Title"));
    }

    #[test]
    fn test_set_inserts_new_field() {
        let ical = "BEGIN:VEVENT\r\nUID:123\r\nEND:VEVENT";
        let result = apply(ical, &[set("SUMMARY", "Blocked")]).unwrap();
        assert!(result.contains("SUMMARY:Blocked"));
    }

    #[test]
    fn test_regex_replace_transform() {
        let ical = "BEGIN:VEVENT\r\nUID:123\r\nSUMMARY:Meeting with Bob\r\nEND:VEVENT";
        let transforms = [regex_replace(
            "SUMMARY",
            "Meeting with (.+)",
            "Busy: $1 ({{source_name}})",
        )];
        let result = apply(ical, &transforms).unwrap();
        assert!(result.contains("SUMMARY:Busy: Bob (work)"));
    }

    #[test]
    fn test_remove_transform() {
        let ical = "BEGIN:VEVENT\r\nUID:123\r\nSUMMARY:Test\r\nLOCATION:Room 42\r\nEND:VEVENT";
        let result = apply(ical, &[remove("LOCATION")]).unwrap();
        assert!(!result.contains("LOCATION"));
        assert!(result.contains("SUMMARY:Test"));
    }

    #[test]
    fn test_multiple_transforms() {
        let ical = "BEGIN:VEVENT\r\nUID:123\r\nSUMMARY:Secret\r\nLOCATION:HQ\r\nDESCRIPTION:Details\r\nEND:VEVENT";
        let transforms = [
            set("SUMMARY", "Busy"),
            remove("LOCATION"),
            remove("DESCRIPTION"),
        ];
        let result = apply(ical, &transforms).unwrap();
        assert!(result.contains("SUMMARY:Busy"));
        assert!(!result.contains("LOCATION"));
        assert!(!result.contains("DESCRIPTION"));
    }

    #[test]
    fn test_transforms_apply_in_order() {
        let ical = "BEGIN:VEVENT\r\nUID:1\r\nSUMMARY:Secret\r\nEND:VEVENT";
        let set_then_replace = [
            set("SUMMARY", "Busy"),
            regex_replace("SUMMARY", "Busy", "Away"),
        ];
        assert!(
            apply(ical, &set_then_replace)
                .unwrap()
                .contains("SUMMARY:Away")
        );
        let replace_then_set = [
            regex_replace("SUMMARY", "Busy", "Away"),
            set("SUMMARY", "Busy"),
        ];
        assert!(
            apply(ical, &replace_then_set)
                .unwrap()
                .contains("SUMMARY:Busy")
        );
    }

    #[test]
    fn test_regex_replace_on_property_with_parameters() {
        // DTSTART has a ;TZID= parameter - regex should only affect the value, not the param
        let ical =
            "BEGIN:VEVENT\r\nUID:1\r\nDTSTART;TZID=Europe/Berlin:20240115T090000\r\nEND:VEVENT";
        let result = apply(ical, &[regex_replace("DTSTART", "T09", "T10")]).unwrap();
        assert!(result.contains("DTSTART;TZID=Europe/Berlin:20240115T100000"));
    }

    #[test]
    fn test_remove_nonexistent_field() {
        let ical = "BEGIN:VEVENT\r\nUID:1\r\nSUMMARY:Test\r\nEND:VEVENT";
        let result = apply(ical, &[remove("LOCATION")]).unwrap();
        assert!(result.contains("SUMMARY:Test"));
    }

    #[test]
    fn test_set_all_placeholders() {
        let ical = "BEGIN:VEVENT\r\nUID:1\r\nEND:VEVENT";
        let transforms = [set(
            "SUMMARY",
            "{{source_name}} via {{sync_name}} from {{source_url}}",
        )];
        let result = apply(ical, &transforms).unwrap();
        assert!(
            result
                .contains("SUMMARY:work via work-to-personal from https://caldav.example.com/cal/")
        );
    }

    #[test]
    fn test_transforms_empty_list() {
        let ical = "BEGIN:VEVENT\r\nUID:1\r\nSUMMARY:Original\r\nEND:VEVENT";
        let result = apply(ical, &[]).unwrap();
        assert!(result.contains("SUMMARY:Original"));
    }

    #[test]
    fn test_add_buffer_utc() {
        let ical = "BEGIN:VEVENT\r\nUID:1\r\nDTSTART:20240115T130000Z\r\nDTEND:20240115T140000Z\r\nEND:VEVENT";
        let result = apply(ical, &[buffer(Some(60), Some(30))]).unwrap();
        assert!(result.contains("DTSTART:20240115T120000Z"));
        assert!(result.contains("DTEND:20240115T143000Z"));
    }

    #[test]
    fn test_add_buffer_with_timezone() {
        let ical = "BEGIN:VEVENT\r\nUID:1\r\nDTSTART;TZID=Europe/Berlin:20240115T130000\r\nDTEND;TZID=Europe/Berlin:20240115T140000\r\nEND:VEVENT";
        let result = apply(ical, &[buffer(Some(30), Some(15))]).unwrap();
        assert!(result.contains("DTSTART;TZID=Europe/Berlin:20240115T123000"));
        assert!(result.contains("DTEND;TZID=Europe/Berlin:20240115T141500"));
    }

    #[test]
    fn test_add_buffer_skips_fullday() {
        let ical = "BEGIN:VEVENT\r\nUID:1\r\nDTSTART;VALUE=DATE:20240115\r\nDTEND;VALUE=DATE:20240116\r\nEND:VEVENT";
        let result = apply(ical, &[buffer(Some(60), Some(30))]).unwrap();
        assert!(result.contains("DTSTART;VALUE=DATE:20240115"));
        assert!(result.contains("DTEND;VALUE=DATE:20240116"));
    }

    #[test]
    fn test_add_buffer_pre_only() {
        let ical = "BEGIN:VEVENT\r\nUID:1\r\nDTSTART:20240115T130000Z\r\nDTEND:20240115T140000Z\r\nEND:VEVENT";
        let result = apply(ical, &[buffer(Some(45), None)]).unwrap();
        assert!(result.contains("DTSTART:20240115T121500Z"));
        assert!(result.contains("DTEND:20240115T140000Z"));
    }

    #[test]
    fn test_add_buffer_crosses_midnight() {
        let ical = "BEGIN:VEVENT\r\nUID:1\r\nDTSTART:20240115T003000Z\r\nDTEND:20240115T010000Z\r\nEND:VEVENT";
        let result = apply(ical, &[buffer(Some(60), None)]).unwrap();
        assert!(result.contains("DTSTART:20240114T233000Z"));
    }

    #[test]
    fn test_add_alarm_15min() {
        let ical = "BEGIN:VEVENT\r\nUID:1\r\nDTSTART:20240115T130000Z\r\nEND:VEVENT";
        let result = apply(ical, &[alarm(15 * 60)]).unwrap();
        assert!(result.contains("BEGIN:VALARM"));
        assert!(result.contains("TRIGGER:-PT15M"));
        assert!(result.contains("ACTION:DISPLAY"));
        assert!(result.contains("END:VALARM"));
    }

    #[test]
    fn test_add_alarm_zero() {
        let ical = "BEGIN:VEVENT\r\nUID:1\r\nDTSTART:20240115T130000Z\r\nEND:VEVENT";
        let result = apply(ical, &[alarm(0)]).unwrap();
        assert!(result.contains("TRIGGER:-PT0S"));
    }

    #[test]
    fn test_parse_ical_duration() {
        assert_eq!(parse_ical_duration("-PT15M"), Some(-900));
        assert_eq!(parse_ical_duration("PT5M"), Some(300));
        assert_eq!(parse_ical_duration("+PT1H30M"), Some(5400));
        assert_eq!(parse_ical_duration("-P1D"), Some(-86400));
        assert_eq!(parse_ical_duration("-P1DT2H"), Some(-93600));
        assert_eq!(parse_ical_duration("-P1W"), Some(-604800));
        assert_eq!(parse_ical_duration("-PT0S"), Some(0));
        assert_eq!(parse_ical_duration("15M"), None);
        assert_eq!(parse_ical_duration("-P"), None);
        assert_eq!(parse_ical_duration("-PT15"), None);
        assert_eq!(parse_ical_duration("-P15M"), None);
    }

    #[test]
    fn test_secs_to_ical_trigger_roundtrip() {
        for secs in [-900, 300, -5400, -93600, 0, -45] {
            let s = secs_to_ical_trigger(secs);
            assert_eq!(parse_ical_duration(&s), Some(secs), "{s}");
        }
        assert_eq!(secs_to_ical_trigger(-900), "-PT15M");
        assert_eq!(secs_to_ical_trigger(300), "PT5M");
    }

    #[test]
    fn test_add_alarm_1h30m() {
        let ical = "BEGIN:VEVENT\r\nUID:1\r\nDTSTART:20240115T130000Z\r\nEND:VEVENT";
        let result = apply(ical, &[alarm(90 * 60)]).unwrap();
        assert!(result.contains("TRIGGER:-PT1H30M"));
    }

    #[test]
    fn test_add_multiple_alarms() {
        let ical = "BEGIN:VEVENT\r\nUID:1\r\nDTSTART:20240115T130000Z\r\nEND:VEVENT";
        let result = apply(ical, &[alarm(15 * 60), alarm(60)]).unwrap();
        assert_eq!(result.matches("BEGIN:VALARM").count(), 2);
        assert!(result.contains("TRIGGER:-PT15M"));
        assert!(result.contains("TRIGGER:-PT1M"));
    }

    // --- exclude_time_range tests ---
    //
    // These call matches_time_range_in_tz directly with an explicit target
    // timezone, so results don't depend on the host machine's system tz.

    fn matches(
        ical: &str,
        days: &[Weekday],
        start: Option<NaiveTime>,
        end: Option<NaiveTime>,
        local: chrono_tz::Tz,
    ) -> bool {
        let lines: Vec<String> = ical.lines().map(|l| l.to_string()).collect();
        matches_time_range_in_tz(&lines, days, start, end, local)
    }

    fn filter_days(days: &[Weekday]) -> [Transform; 1] {
        [skip(vec![Condition {
            test: Test::TimeRange {
                days: days.to_vec(),
                start: None,
                end: None,
            },
            negate: false,
        }])]
    }

    #[test]
    fn test_exclude_time_range_days_only_match() {
        // Saturday event, filter sat+sun -> dropped
        let ical = "BEGIN:VEVENT\r\nUID:1\r\nDTSTART:20240113T100000Z\r\nEND:VEVENT"; // 2024-01-13 = Saturday
        assert!(matches(ical, &[Sat, Sun], None, None, chrono_tz::UTC));
    }

    #[test]
    fn test_exclude_time_range_days_only_no_match() {
        // Monday event, filter sat+sun -> kept
        let ical = "BEGIN:VEVENT\r\nUID:1\r\nDTSTART:20240115T100000Z\r\nEND:VEVENT"; // 2024-01-15 = Monday
        assert!(!matches(ical, &[Sat, Sun], None, None, chrono_tz::UTC));
    }

    #[test]
    fn test_exclude_time_range_start_time_only() {
        // Monday 19:00, filter mon-fri from 18:00 -> dropped
        let ical = "BEGIN:VEVENT\r\nUID:1\r\nDTSTART:20240115T190000Z\r\nEND:VEVENT";
        assert!(matches(ical, WEEKDAYS, hm(18, 0), None, chrono_tz::UTC));
    }

    #[test]
    fn test_exclude_time_range_start_time_no_match() {
        // Monday 10:00, filter mon-fri from 18:00 -> kept
        let ical = "BEGIN:VEVENT\r\nUID:1\r\nDTSTART:20240115T100000Z\r\nEND:VEVENT";
        assert!(!matches(ical, WEEKDAYS, hm(18, 0), None, chrono_tz::UTC));
    }

    #[test]
    fn test_exclude_time_range_end_time_only() {
        // Monday 07:30, filter mon-fri until 07:59 -> dropped
        let ical = "BEGIN:VEVENT\r\nUID:1\r\nDTSTART:20240115T073000Z\r\nEND:VEVENT";
        assert!(matches(ical, WEEKDAYS, None, hm(7, 59), chrono_tz::UTC));
    }

    #[test]
    fn test_exclude_time_range_end_time_no_match() {
        // Monday 08:00, filter mon-fri until 07:59 -> kept
        let ical = "BEGIN:VEVENT\r\nUID:1\r\nDTSTART:20240115T080000Z\r\nEND:VEVENT";
        assert!(!matches(ical, WEEKDAYS, None, hm(7, 59), chrono_tz::UTC));
    }

    #[test]
    fn test_exclude_time_range_end_minute_is_inclusive() {
        // Monday 07:59:30 is still within "until 07:59" (minute granularity)
        let ical = "BEGIN:VEVENT\r\nUID:1\r\nDTSTART:20240115T075930Z\r\nEND:VEVENT";
        assert!(matches(ical, WEEKDAYS, None, hm(7, 59), chrono_tz::UTC));
    }

    #[test]
    fn test_exclude_time_range_both_times() {
        // Monday 12:00, filter mon 08:00-17:59 -> dropped
        let ical = "BEGIN:VEVENT\r\nUID:1\r\nDTSTART:20240115T120000Z\r\nEND:VEVENT";
        assert!(matches(ical, &[Mon], hm(8, 0), hm(17, 59), chrono_tz::UTC));
    }

    #[test]
    fn test_exclude_time_range_both_times_no_match() {
        // Monday 18:00, filter mon 08:00-17:59 -> kept
        let ical = "BEGIN:VEVENT\r\nUID:1\r\nDTSTART:20240115T180000Z\r\nEND:VEVENT";
        assert!(!matches(ical, &[Mon], hm(8, 0), hm(17, 59), chrono_tz::UTC));
    }

    #[test]
    fn test_exclude_time_range_allday_matches_weekday() {
        // All-day Saturday, filter sat+sun -> dropped (matches by day)
        let ical = "BEGIN:VEVENT\r\nUID:1\r\nDTSTART;VALUE=DATE:20240113\r\nEND:VEVENT";
        assert!(apply(ical, &filter_days(&[Sat, Sun])).is_none());
    }

    #[test]
    fn test_exclude_time_range_allday_no_match_weekday() {
        // All-day Saturday, filter mon-fri -> kept
        let ical = "BEGIN:VEVENT\r\nUID:1\r\nDTSTART;VALUE=DATE:20240113\r\nEND:VEVENT";
        assert!(apply(ical, &filter_days(WEEKDAYS)).is_some());
    }

    #[test]
    fn test_exclude_time_range_with_times_keeps_allday() {
        // All-day Monday, filter mon-fri from 18:00 -> kept, it has no start time
        let ical = "BEGIN:VEVENT\r\nUID:1\r\nDTSTART;VALUE=DATE:20240115\r\nEND:VEVENT";
        assert!(!matches(ical, WEEKDAYS, hm(18, 0), None, chrono_tz::UTC));
        assert!(!matches(ical, WEEKDAYS, None, hm(7, 59), chrono_tz::UTC));
    }

    #[test]
    fn test_exclude_time_range_wraps_around_midnight() {
        // Mon-Fri 18:00-07:59: outside working hours
        let at = |time: &str| {
            format!("BEGIN:VEVENT\r\nUID:1\r\nDTSTART:20240115T{time}00Z\r\nEND:VEVENT")
        };
        let outside =
            |time: &str| matches(&at(time), WEEKDAYS, hm(18, 0), hm(7, 59), chrono_tz::UTC);
        assert!(outside("0600"));
        assert!(outside("0759"));
        assert!(!outside("0800"));
        assert!(!outside("1759"));
        assert!(outside("1800"));
        assert!(outside("2330"));
        // Saturday 06:00 is not in the days
        let saturday = "BEGIN:VEVENT\r\nUID:1\r\nDTSTART:20240113T060000Z\r\nEND:VEVENT";
        assert!(!matches(
            saturday,
            WEEKDAYS,
            hm(18, 0),
            hm(7, 59),
            chrono_tz::UTC
        ));
    }

    #[test]
    fn test_exclude_time_range_with_tzid_same_zone() {
        // Monday 19:00 Europe/Berlin, local=Europe/Berlin, filter from 18:00 -> dropped
        let ical =
            "BEGIN:VEVENT\r\nUID:1\r\nDTSTART;TZID=Europe/Berlin:20240115T190000\r\nEND:VEVENT";
        assert!(matches(
            ical,
            &[Mon],
            hm(18, 0),
            None,
            chrono_tz::Europe::Berlin
        ));
    }

    #[test]
    fn test_exclude_time_range_tzid_converts_to_local() {
        // Monday 13:00 America/New_York (winter, UTC-5) = Monday 19:00 Europe/Berlin.
        // filter mon from 18:00, local=Europe/Berlin -> dropped
        let ical =
            "BEGIN:VEVENT\r\nUID:1\r\nDTSTART;TZID=America/New_York:20240115T130000\r\nEND:VEVENT";
        assert!(matches(
            ical,
            &[Mon],
            hm(18, 0),
            None,
            chrono_tz::Europe::Berlin
        ));
    }

    #[test]
    fn test_exclude_time_range_utc_converts_to_local() {
        // Monday 16:00 UTC = Monday 17:00 Europe/Berlin (winter, UTC+1).
        // 17:00 Berlin >= 17:00 threshold -> dropped, whereas comparing the
        // raw UTC hour (16:00) against the same threshold would keep it.
        let ical = "BEGIN:VEVENT\r\nUID:1\r\nDTSTART:20240115T160000Z\r\nEND:VEVENT";
        assert!(matches(
            ical,
            &[Mon],
            hm(17, 0),
            None,
            chrono_tz::Europe::Berlin
        ));
    }

    // --- exclude_all_day tests ---

    #[test]
    fn test_exclude_all_day_drops() {
        let ical = "BEGIN:VEVENT\r\nUID:1\r\nDTSTART;VALUE=DATE:20240115\r\nEND:VEVENT";
        assert!(apply(ical, &[skip(vec![all_day()])]).is_none());
    }

    #[test]
    fn test_exclude_all_day_keeps_timed() {
        let ical = "BEGIN:VEVENT\r\nUID:1\r\nDTSTART:20240115T100000Z\r\nEND:VEVENT";
        assert!(apply(ical, &[skip(vec![all_day()])]).is_some());
    }

    #[test]
    fn test_exclude_all_day_keeps_explicit_date_time() {
        let ical =
            "BEGIN:VEVENT\r\nUID:1\r\nDTSTART;VALUE=DATE-TIME:20240115T100000Z\r\nEND:VEVENT";
        assert!(apply(ical, &[skip(vec![all_day()])]).is_some());
    }

    #[test]
    fn test_exclude_time_range_explicit_date_time_uses_time_window() {
        // Monday 18:00 with VALUE=DATE-TIME, filter mon 08:00-17:59 -> kept
        let ical =
            "BEGIN:VEVENT\r\nUID:1\r\nDTSTART;VALUE=DATE-TIME:20240115T180000Z\r\nEND:VEVENT";
        assert!(!matches(ical, &[Mon], hm(8, 0), hm(17, 59), chrono_tz::UTC));
        assert!(matches(ical, &[Mon], hm(18, 0), None, chrono_tz::UTC));
    }

    #[test]
    fn test_add_buffer_applies_to_explicit_date_time() {
        let ical = "BEGIN:VEVENT\r\nUID:1\r\nDTSTART;VALUE=DATE-TIME:20240115T130000Z\r\nDTEND;VALUE=DATE-TIME:20240115T140000Z\r\nEND:VEVENT";
        let result = apply(ical, &[buffer(Some(30), Some(15))]).unwrap();
        assert!(result.contains("DTSTART;VALUE=DATE-TIME:20240115T123000Z"));
        assert!(result.contains("DTEND;VALUE=DATE-TIME:20240115T141500Z"));
    }

    /// A CalDAV resource as servers send it: VTIMEZONE (with its own 1970
    /// DTSTART) before a recurring master and one overridden occurrence.
    const SERIES: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n\
        BEGIN:VTIMEZONE\r\nTZID:Europe/Berlin\r\n\
        BEGIN:STANDARD\r\nDTSTART:19701025T030000\r\nTZOFFSETFROM:+0200\r\nTZOFFSETTO:+0100\r\nEND:STANDARD\r\n\
        END:VTIMEZONE\r\n\
        BEGIN:VEVENT\r\nUID:s1\r\nDTSTART;TZID=Europe/Berlin:20240115T100000\r\n\
        DTEND;TZID=Europe/Berlin:20240115T110000\r\nRRULE:FREQ=WEEKLY\r\nSUMMARY:Board Review\r\n\
        BEGIN:VALARM\r\nACTION:DISPLAY\r\nDESCRIPTION:Reminder\r\nTRIGGER:-PT15M\r\nEND:VALARM\r\n\
        END:VEVENT\r\n\
        BEGIN:VEVENT\r\nUID:s1\r\nRECURRENCE-ID;TZID=Europe/Berlin:20240122T100000\r\n\
        DTSTART;TZID=Europe/Berlin:20240127T100000\r\nDTEND;TZID=Europe/Berlin:20240127T110000\r\n\
        SUMMARY:Board Review (moved)\r\nEND:VEVENT\r\nEND:VCALENDAR";

    #[test]
    fn test_set_applies_to_every_vevent_of_a_series() {
        let result = apply(SERIES, &[set("SUMMARY", "Busy")]).unwrap();
        assert_eq!(result.matches("SUMMARY:Busy").count(), 2);
        assert!(!result.contains("Board Review"));
    }

    #[test]
    fn test_set_inserts_into_every_vevent_without_the_property() {
        let ical = SERIES.replace("SUMMARY:Board Review (moved)\r\n", "");
        let result = apply(&ical, &[set("LOCATION", "Away")]).unwrap();
        assert_eq!(result.matches("LOCATION:Away").count(), 2);
    }

    #[test]
    fn test_regex_and_remove_apply_to_every_vevent() {
        let result = apply(SERIES, &[regex_replace("SUMMARY", "Board", "Team")]).unwrap();
        assert_eq!(result.matches("SUMMARY:Team Review").count(), 2);
        let result = apply(SERIES, &[remove("SUMMARY")]).unwrap();
        assert!(!result.contains("SUMMARY"));
    }

    #[test]
    fn test_property_transforms_leave_valarm_alone() {
        let result = apply(SERIES, &[set("DESCRIPTION", "Blocked")]).unwrap();
        assert!(result.contains("DESCRIPTION:Reminder"));
        assert_eq!(result.matches("DESCRIPTION:Blocked").count(), 2);
        let result = apply(SERIES, &[remove("DESCRIPTION")]).unwrap();
        assert!(result.contains("DESCRIPTION:Reminder"));
    }

    #[test]
    fn test_buffer_leaves_vtimezone_alone() {
        let result = apply(SERIES, &[buffer(Some(30), Some(15))]).unwrap();
        assert!(result.contains("DTSTART:19701025T030000"));
        assert!(result.contains("DTSTART;TZID=Europe/Berlin:20240115T093000"));
        assert!(result.contains("DTEND;TZID=Europe/Berlin:20240127T111500"));
    }

    #[test]
    fn test_filters_look_at_the_master_event_not_vtimezone() {
        // Master starts Monday 2024-01-15; the VTIMEZONE's 1970-10-25 is a Sunday
        assert!(apply(SERIES, &filter_days(&[Sun])).is_some());
        assert!(apply(SERIES, &filter_days(&[Mon])).is_none());
        // The override's Saturday start does not decide for the series
        assert!(apply(SERIES, &filter_days(&[Sat])).is_some());
    }

    fn exclude_regex(field: &str, pattern: &str) -> Transform {
        skip(vec![Condition {
            test: Test::Matches {
                field: field.to_string(),
                regex: Regex::new(pattern).unwrap(),
            },
            negate: false,
        }])
    }

    fn all_day() -> Condition {
        Condition {
            test: Test::AllDay,
            negate: false,
        }
    }

    #[test]
    fn test_exclude_regex_matches_the_master_event() {
        assert!(apply(SERIES, &[exclude_regex("SUMMARY", "^Board")]).is_none());
        assert!(apply(SERIES, &[exclude_regex("SUMMARY", "Lunch")]).is_some());
        // Only the master decides, like the other excludes
        assert!(apply(SERIES, &[exclude_regex("SUMMARY", "moved")]).is_some());
        // A property the event does not have counts as an empty value
        assert!(apply(SERIES, &[exclude_regex("LOCATION", "^$")]).is_none());
        assert!(apply(SERIES, &[exclude_regex("LOCATION", ".")]).is_some());
        // Nested VALARM properties are not the event's
        assert!(apply(SERIES, &[exclude_regex("DESCRIPTION", "Reminder")]).is_some());
    }

    #[test]
    fn test_exclude_regex_sees_unfolded_unescaped_values() {
        let ical =
            "BEGIN:VEVENT\r\nUID:a\r\nSUMMARY;LANGUAGE=de:Mittag\\, priv\r\n at\r\nEND:VEVENT";
        // process_event unfolds before the transforms run
        let ical = crate::ical::unfold(ical);
        assert!(apply(&ical, &[exclude_regex("SUMMARY", "Mittag, privat")]).is_none());
    }

    fn replace(search: &str, replacement: &str, count: Option<usize>) -> Transform {
        Action::ReplaceField {
            field: "SUMMARY".to_string(),
            search: search.to_string(),
            replacement: replacement.to_string(),
            count,
        }
        .into()
    }

    #[test]
    fn test_replace_field_everywhere_or_count_times() {
        let ical = "BEGIN:VEVENT\r\nUID:a\r\nSUMMARY:a-a-a\r\nLOCATION:a\r\nEND:VEVENT";
        let all = apply(ical, &[replace("a", "b", None)]).unwrap();
        assert!(
            all.contains("SUMMARY:b-b-b") && all.contains("LOCATION:a"),
            "{all}"
        );
        let two = apply(ical, &[replace("a", "b", Some(2))]).unwrap();
        assert!(two.contains("SUMMARY:b-b-a"), "{two}");
        // Plain text, no regex; placeholders are expanded
        let ical = ical.replace("a-a-a", "1.5 (x)");
        let plain = apply(&ical, &[replace(".", "{{source_name}}", None)]).unwrap();
        assert!(plain.contains("SUMMARY:1work5 (x)"), "{plain}");
    }

    #[test]
    fn test_replace_field_covers_every_vevent_of_a_series() {
        let result = apply(SERIES, &[replace("Board", "Team", None)]).unwrap();
        assert_eq!(result.matches("SUMMARY:Team Review").count(), 2);
    }

    #[test]
    fn test_exclude_regex_keeps_only_events_with_a_place() {
        let only_places = [
            exclude_regex("LOCATION", "^$"),
            exclude_regex("LOCATION", "^https?://"),
        ];
        let with =
            |location: &str| format!("BEGIN:VEVENT\r\nUID:a\r\nSUMMARY:x\r\n{location}END:VEVENT");
        assert!(apply(&with(""), &only_places).is_none());
        assert!(apply(&with("LOCATION:\r\n"), &only_places).is_none());
        assert!(
            apply(
                &with("LOCATION:https://meet.example.com/x\r\n"),
                &only_places
            )
            .is_none()
        );
        assert!(apply(&with("LOCATION:Room 5\r\n"), &only_places).is_some());
    }

    #[test]
    fn test_exclude_field_drops_values_that_contain_the_text() {
        let exclude = |search: &str| skip(vec![contains("SUMMARY", search)]);
        assert!(apply(SERIES, &[exclude("ard Rev")]).is_none());
        // Case-sensitive, plain text, master only
        assert!(apply(SERIES, &[exclude("board")]).is_some());
        assert!(apply(SERIES, &[exclude("B.ard")]).is_some());
        assert!(apply(SERIES, &[exclude("moved")]).is_some());
        let ical = "BEGIN:VEVENT\r\nUID:a\r\nSUMMARY:Lunch\\, Room 5\r\nEND:VEVENT";
        assert!(apply(ical, &[exclude("Lunch, Room")]).is_none());
    }

    #[test]
    fn test_exclude_all_day_ignores_vtimezone() {
        assert!(apply(SERIES, &[skip(vec![all_day()])]).is_some());
    }

    #[test]
    fn test_add_buffer_across_a_daylight_saving_switch() {
        let berlin = "BEGIN:VTIMEZONE\r\nTZID:Europe/Berlin\r\n\
            BEGIN:STANDARD\r\nDTSTART:19701025T030000\r\nTZOFFSETFROM:+0200\r\nTZOFFSETTO:+0100\r\n\
            RRULE:FREQ=YEARLY;BYMONTH=10;BYDAY=-1SU\r\nEND:STANDARD\r\n\
            BEGIN:DAYLIGHT\r\nDTSTART:19700329T020000\r\nTZOFFSETFROM:+0100\r\nTZOFFSETTO:+0200\r\n\
            RRULE:FREQ=YEARLY;BYMONTH=3;BYDAY=-1SU\r\nEND:DAYLIGHT\r\nEND:VTIMEZONE";
        // 03:00 on 31 March 2024 is the first summer time minute; an hour
        // earlier is 01:00, not the skipped 02:00
        let ical = format!(
            "BEGIN:VCALENDAR\r\n{berlin}\r\nBEGIN:VEVENT\r\nUID:x\r\n\
             DTSTART;TZID=Europe/Berlin:20240331T030000\r\n\
             DTEND;TZID=Europe/Berlin:20241027T020000\r\nEND:VEVENT\r\nEND:VCALENDAR"
        );
        let buffer = Transform::from(Action::AddBuffer {
            pre: Some(Duration::from_secs(3600)),
            post: Some(Duration::from_secs(3600)),
        });
        let result = apply(&ical, &[buffer]).unwrap();
        assert!(
            result.contains("DTSTART;TZID=Europe/Berlin:20240331T010000"),
            "{result}"
        );
        // 02:00 summer time on 27 October plus an hour is 02:00 winter time
        assert!(
            result.contains("DTEND;TZID=Europe/Berlin:20241027T020000"),
            "{result}"
        );
    }

    #[test]
    fn test_add_buffer_across_a_daylight_saving_switch_without_vtimezone() {
        let ical = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:x\r\n\
            DTSTART;TZID=Europe/Berlin:20240331T030000\r\n\
            DTEND;TZID=Europe/Berlin:20240331T040000\r\nEND:VEVENT\r\nEND:VCALENDAR";
        let buffer = Transform::from(Action::AddBuffer {
            pre: Some(Duration::from_secs(3600)),
            post: None,
        });
        let result = apply(ical, &[buffer]).unwrap();
        assert!(
            result.contains("DTSTART;TZID=Europe/Berlin:20240331T010000"),
            "{result}"
        );
    }

    #[test]
    fn test_add_buffer_moves_series_dates_and_extends_duration() {
        let ical = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:s\r\n\
            DTSTART;TZID=Europe/Berlin:20240115T090000\r\nDURATION:PT1H\r\nRRULE:FREQ=WEEKLY\r\n\
            EXDATE;TZID=Europe/Berlin:20240129T090000,20240205T090000\r\nRDATE:20240301T080000Z\r\n\
            END:VEVENT\r\nBEGIN:VEVENT\r\nUID:s\r\nRECURRENCE-ID;TZID=Europe/Berlin:20240122T090000\r\n\
            DTSTART;TZID=Europe/Berlin:20240122T110000\r\nDTEND;TZID=Europe/Berlin:20240122T120000\r\n\
            END:VEVENT\r\nBEGIN:VEVENT\r\nUID:d\r\nDTSTART;VALUE=DATE:20240115\r\nDURATION:P1D\r\n\
            END:VEVENT\r\nEND:VCALENDAR";
        let transforms = [Transform::from(Action::AddBuffer {
            pre: Some(Duration::from_secs(1800)),
            post: Some(Duration::from_secs(900)),
        })];
        let result = apply(ical, &transforms).unwrap();
        for expected in [
            "DTSTART;TZID=Europe/Berlin:20240115T083000",
            "DURATION:PT6300S",
            "EXDATE;TZID=Europe/Berlin:20240129T083000,20240205T083000",
            "RDATE:20240301T073000Z",
            "RECURRENCE-ID;TZID=Europe/Berlin:20240122T083000",
            "DTSTART;TZID=Europe/Berlin:20240122T103000",
            "DTEND;TZID=Europe/Berlin:20240122T121500",
            "RRULE:FREQ=WEEKLY",
            // All-day events are left alone
            "DTSTART;VALUE=DATE:20240115",
            "DURATION:P1D",
        ] {
            assert!(result.contains(expected), "{expected}: {result}");
        }
    }

    // --- conditions ---

    const MEETING: &str = "BEGIN:VEVENT\r\nUID:a\r\nSUMMARY:Lunch\r\nLOCATION:Room 5\r\n\
        DTSTART:20240115T100000Z\r\nDTEND:20240115T110000Z\r\nEND:VEVENT";

    /// `transform` when all of `conditions` match.
    fn when(transform: Transform, conditions: Vec<Condition>) -> Transform {
        Transform {
            when: conditions.into_iter().map(|c| vec![c]).collect(),
            ..transform
        }
    }

    fn contains(field: &str, text: &str) -> Condition {
        Condition {
            test: Test::Contains {
                field: field.to_string(),
                text: text.to_string(),
            },
            negate: false,
        }
    }

    fn empty(field: &str) -> Condition {
        Condition {
            test: Test::Empty {
                field: field.to_string(),
            },
            negate: false,
        }
    }

    fn not(condition: Condition) -> Condition {
        Condition {
            negate: true,
            ..condition
        }
    }

    fn skip(conditions: Vec<Condition>) -> Transform {
        when(Action::Skip.into(), conditions)
    }

    #[test]
    fn test_conditions_see_every_line_of_a_repeated_property() {
        let event = MEETING.replace(
            "LOCATION:Room 5\r\n",
            "ATTENDEE:mailto:me@corp\r\nATTENDEE:mailto:ceo@corp\r\n",
        );
        assert!(apply(&event, &[skip(vec![contains("ATTENDEE", "ceo@")])]).is_none());
        assert!(apply(&event, &[exclude_regex("ATTENDEE", "ceo@")]).is_none());
        // "does not contain" means no line contains it
        assert!(apply(&event, &[skip(vec![not(contains("ATTENDEE", "ceo@"))])]).is_some());
        assert!(apply(&event, &[skip(vec![not(contains("ATTENDEE", "boss@"))])]).is_none());
        let blank_first = event.replace("ATTENDEE:mailto:me@corp", "ATTENDEE:");
        assert!(apply(&blank_first, &[skip(vec![empty("ATTENDEE")])]).is_some());
    }

    #[test]
    fn test_a_quoted_colon_in_a_parameter_is_not_the_value() {
        let event = MEETING.replace("SUMMARY:Lunch", "SUMMARY;ALTREP=\"http://zoom.us/a\":Lunch");
        assert!(apply(&event, &[skip(vec![contains("SUMMARY", "zoom")])]).is_some());
        assert!(apply(&event, &[skip(vec![contains("SUMMARY", "Lunch")])]).is_none());
        let out = apply(&event, &[replace("Lunch", "Busy", None)]).unwrap();
        assert!(
            out.contains("SUMMARY;ALTREP=\"http://zoom.us/a\":Busy"),
            "{out}"
        );
        let out = apply(&event, &[replace("zoom", "x", None)]).unwrap();
        assert!(out.contains("ALTREP=\"http://zoom.us/a\""), "{out}");
    }

    #[test]
    fn test_a_time_condition_with_times_never_matches_all_day_events() {
        let holiday = "BEGIN:VEVENT\r\nUID:h\r\nSUMMARY:Holiday\r\n\
            DTSTART;VALUE=DATE:20240115\r\nDTEND;VALUE=DATE:20240116\r\nEND:VEVENT";
        let hours = |negate| Condition {
            test: Test::TimeRange {
                days: WEEKDAYS.to_vec(),
                start: NaiveTime::from_hms_opt(8, 0, 0),
                end: NaiveTime::from_hms_opt(17, 59, 0),
            },
            negate,
        };
        assert!(apply(holiday, &[skip(vec![hours(false)])]).is_some());
        assert!(apply(holiday, &[skip(vec![hours(true)])]).is_some());
    }

    #[test]
    fn test_set_replaces_every_line_of_the_property() {
        let doubled = MEETING.replace("SUMMARY:Lunch\r\n", "SUMMARY:Lunch\r\nSUMMARY:Secret\r\n");
        let out = apply(&doubled, &[set("SUMMARY", "Busy")]).unwrap();
        assert!(!out.contains("Secret") && !out.contains("Lunch"), "{out}");
        assert_eq!(out.matches("SUMMARY:Busy").count(), 1, "{out}");
    }

    #[test]
    fn test_condition_applies_the_action_only_when_it_matches() {
        let travel = when(buffer(Some(30), None), vec![not(empty("LOCATION"))]);
        let with = apply(MEETING, std::slice::from_ref(&travel)).unwrap();
        assert!(with.contains("DTSTART:20240115T093000Z"), "{with}");
        let without = apply(&MEETING.replace("LOCATION:Room 5\r\n", ""), &[travel]).unwrap();
        assert!(without.contains("DTSTART:20240115T100000Z"), "{without}");
    }

    #[test]
    fn test_all_conditions_of_a_transform_must_match() {
        let travel = when(
            buffer(Some(30), None),
            vec![not(empty("LOCATION")), not(contains("LOCATION", "Zoom"))],
        );
        let room = apply(MEETING, std::slice::from_ref(&travel)).unwrap();
        assert!(room.contains("DTSTART:20240115T093000Z"), "{room}");
        let zoom = apply(&MEETING.replace("Room 5", "Zoom link"), &[travel]).unwrap();
        assert!(zoom.contains("DTSTART:20240115T100000Z"), "{zoom}");
    }

    #[test]
    fn test_a_group_needs_one_matching_condition() {
        // Lunch and (in Room 5 or on Zoom)
        let busy = Transform {
            when: vec![
                vec![contains("SUMMARY", "Lunch")],
                vec![contains("LOCATION", "Room 5"), contains("LOCATION", "Zoom")],
            ],
            ..set("SUMMARY", "Busy")
        };
        let busy = std::slice::from_ref(&busy);
        assert!(apply(MEETING, busy).unwrap().contains("SUMMARY:Busy"));
        let zoom = MEETING.replace("Room 5", "Zoom");
        assert!(apply(&zoom, busy).unwrap().contains("SUMMARY:Busy"));
        let elsewhere = MEETING.replace("Room 5", "Room 6");
        assert!(apply(&elsewhere, busy).unwrap().contains("SUMMARY:Lunch"));
        let dinner = MEETING.replace("Lunch", "Dinner");
        assert!(apply(&dinner, busy).unwrap().contains("SUMMARY:Dinner"));
    }

    #[test]
    fn test_empty_counts_a_missing_or_blank_property() {
        let busy = when(set("SUMMARY", "Busy"), vec![empty("LOCATION")]);
        let blank = MEETING.replace("Room 5", "");
        assert!(
            apply(&blank, std::slice::from_ref(&busy))
                .unwrap()
                .contains("SUMMARY:Busy")
        );
        let missing = MEETING.replace("LOCATION:Room 5\r\n", "");
        assert!(
            apply(&missing, std::slice::from_ref(&busy))
                .unwrap()
                .contains("SUMMARY:Busy")
        );
        assert!(apply(MEETING, &[busy]).unwrap().contains("SUMMARY:Lunch"));
    }

    #[test]
    fn test_conditions_see_what_earlier_transforms_changed() {
        let busy = set("SUMMARY", "Busy");
        let skip_lunch = skip(vec![contains("SUMMARY", "Lunch")]);
        assert!(apply(MEETING, &[busy.clone(), skip_lunch.clone()]).is_some());
        assert!(apply(MEETING, &[skip_lunch, busy.clone()]).is_none());
        assert!(apply(MEETING, &[busy, skip(vec![contains("SUMMARY", "Busy")])]).is_none());
    }

    #[test]
    fn test_applied_lists_only_the_fields_that_were_set() {
        let ctx = test_ctx();
        let transforms = [
            when(set("SUMMARY", "Busy"), vec![contains("SUMMARY", "Board")]),
            set("DESCRIPTION", "blocked"),
        ];
        let applied = apply_transforms(MEETING, &transforms, &ctx).unwrap();
        assert!(applied.set_fields.contains("DESCRIPTION"));
        assert!(!applied.set_fields.contains("SUMMARY"));
    }

    #[test]
    fn test_alarms_are_collected_where_their_conditions_match() {
        let ctx = test_ctx();
        let transforms = [
            alarm(600),
            when(alarm(60), vec![contains("SUMMARY", "Board")]),
            set("SUMMARY", "Board"),
            when(alarm(120), vec![contains("SUMMARY", "Board")]),
        ];
        let applied = apply_transforms(MEETING, &transforms, &ctx).unwrap();
        assert_eq!(
            applied.alarms,
            [Duration::from_secs(600), Duration::from_secs(120)]
        );
        assert!(!applied.ical.contains("VALARM"));
    }
}
