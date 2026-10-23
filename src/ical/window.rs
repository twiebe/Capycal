//! The days around today a sync copies: whether a source event might have
//! an occurrence in them.

use chrono::{DateTime, NaiveDate, NaiveDateTime, TimeDelta, Utc};

use super::transform::parse_ical_duration;
use super::{is_date_value, property_name, unfold, value_colon};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub days_past: u64,
    pub days_future: u64,
}

impl Window {
    /// Whether the event `ical` overlaps the window around `now`. A series
    /// counts when it runs during the window, whether or not an occurrence
    /// falls into it, and when one of its moved occurrences does. Local and
    /// zoned times are read as UTC; the window grows by a day on each side,
    /// so a time zone never loses an event.
    /// An event without a readable start is kept.
    pub fn contains(&self, ical: &str, now: DateTime<Utc>) -> bool {
        let days = |d: u64| {
            i64::try_from(d)
                .ok()
                .and_then(|d| TimeDelta::try_days(d.saturating_add(1)))
        };
        let now = now.naive_utc();
        let from = days(self.days_past)
            .and_then(|d| now.checked_sub_signed(d))
            .unwrap_or(NaiveDateTime::MIN);
        let until = days(self.days_future)
            .and_then(|d| now.checked_add_signed(d))
            .unwrap_or(NaiveDateTime::MAX);
        // A moved occurrence counts too: it may lie in the window when its
        // series does not
        let events = vevents(&unfold(ical));
        events.is_empty() || events.iter().any(|e| e.overlaps(from, until))
    }
}

#[derive(Default)]
struct Vevent {
    start: Option<NaiveDateTime>,
    all_day: bool,
    end: Option<NaiveDateTime>,
    duration: Option<i64>,
    rrule: bool,
    until: Option<NaiveDateTime>,
    rdate: bool,
}

impl Vevent {
    fn overlaps(&self, from: NaiveDateTime, until: NaiveDateTime) -> bool {
        let Some(start) = self.start else {
            return true;
        };
        let end = self
            .end
            .or_else(|| {
                self.duration
                    .map(|secs| start + TimeDelta::seconds(secs.max(0)))
            })
            .unwrap_or(if self.all_day {
                start + TimeDelta::days(1)
            } else {
                start
            });
        let last_end = if self.rdate {
            None
        } else if self.rrule {
            self.until.map(|u| u + (end - start))
        } else {
            Some(end)
        };
        start <= until && last_end.is_none_or(|e| e >= from)
    }
}

/// The VEVENTs of an unfolded calendar object, without their alarms.
fn vevents(unfolded: &str) -> Vec<Vevent> {
    let mut events = Vec::new();
    let mut current: Option<Vevent> = None;
    let mut nested = 0;
    for line in unfolded.lines().map(str::trim_end) {
        match line {
            "BEGIN:VEVENT" => current = Some(Vevent::default()),
            "END:VEVENT" => events.extend(current.take()),
            _ if line.starts_with("BEGIN:") && current.is_some() => nested += 1,
            _ if line.starts_with("END:") && nested > 0 => nested -= 1,
            _ => {
                let (Some(event), 0) = (current.as_mut(), nested) else {
                    continue;
                };
                let Some(value) = value_colon(line).map(|i| &line[i + 1..]) else {
                    continue;
                };
                match property_name(line) {
                    "DTSTART" => {
                        event.start = parse_time(value);
                        event.all_day = is_date_value(line) || value.len() == 8;
                    }
                    "DTEND" => event.end = parse_time(value),
                    "DURATION" => event.duration = parse_ical_duration(value),
                    "RRULE" => {
                        event.rrule = true;
                        event.until = value
                            .split(';')
                            .find_map(|part| part.strip_prefix("UNTIL="))
                            .and_then(parse_time);
                    }
                    "RDATE" => event.rdate = true,
                    _ => {}
                }
            }
        }
    }
    events
}

/// A DATE or DATE-TIME value, local or UTC, as a naive time.
fn parse_time(value: &str) -> Option<NaiveDateTime> {
    match value.get(..15) {
        Some(time) => NaiveDateTime::parse_from_str(time, "%Y%m%dT%H%M%S").ok(),
        None => NaiveDate::parse_from_str(value.get(..8)?, "%Y%m%d")
            .ok()?
            .and_hms_opt(0, 0, 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WINDOW: Window = Window {
        days_past: 30,
        days_future: 30,
    };

    /// 2026-06-15 12:00 UTC
    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1781524800, 0).unwrap()
    }

    fn event(lines: &str) -> String {
        format!(
            "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:x\r\n{}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
            lines.replace('\n', "\r\n")
        )
    }

    fn contains(lines: &str) -> bool {
        WINDOW.contains(&event(lines), now())
    }

    #[test]
    fn test_timed_events_at_the_edges() {
        // The window with its day of slack: 2026-05-15 12:00 to 2026-07-16 12:00
        assert!(contains("DTSTART:20260615T090000Z\nDTEND:20260615T100000Z"));
        assert!(contains("DTSTART:20260515T130000Z\nDTEND:20260515T140000Z"));
        assert!(!contains(
            "DTSTART:20260515T100000Z\nDTEND:20260515T110000Z"
        ));
        assert!(contains("DTSTART:20260716T110000Z\nDTEND:20260716T120000Z"));
        assert!(!contains(
            "DTSTART:20260716T130000Z\nDTEND:20260716T140000Z"
        ));
    }

    #[test]
    fn test_an_event_ending_in_the_window_counts() {
        assert!(contains("DTSTART:20260501T090000Z\nDTEND:20260520T100000Z"));
    }

    #[test]
    fn test_zoned_and_floating_times_read_as_utc() {
        assert!(contains(
            "DTSTART;TZID=Europe/Berlin:20260620T090000\nDTEND;TZID=Europe/Berlin:20260620T100000"
        ));
        assert!(contains("DTSTART:20260620T090000\nDTEND:20260620T100000"));
        assert!(!contains("DTSTART;TZID=Pacific/Auckland:20260801T090000"));
    }

    #[test]
    fn test_all_day_without_end_lasts_a_day() {
        // The first day of the window
        assert!(contains("DTSTART;VALUE=DATE:20260515"));
        assert!(contains("DTSTART:20260515"));
        assert!(!contains("DTSTART;VALUE=DATE:20260513"));
        assert!(contains("DTSTART;VALUE=DATE:20260716"));
        assert!(!contains("DTSTART;VALUE=DATE:20260717"));
    }

    #[test]
    fn test_duration_gives_the_end() {
        assert!(contains("DTSTART:20260510T090000Z\nDURATION:P7D"));
        assert!(!contains("DTSTART:20260510T090000Z\nDURATION:PT1H"));
    }

    #[test]
    fn test_series_until() {
        let series = |until: &str| {
            contains(&format!(
                "DTSTART:20250101T090000Z\nDTEND:20250101T100000Z\nRRULE:FREQ=WEEKLY;UNTIL={until};BYDAY=MO"
            ))
        };
        assert!(!series("20260101T000000Z"));
        assert!(series("20260601T000000Z"));
        assert!(series("20270101T000000Z"));
        // A DATE value
        assert!(!series("20250101"));
        assert!(series("20260520"));
    }

    #[test]
    fn test_series_without_end_is_kept() {
        assert!(contains(
            "DTSTART:20210101T090000Z\nRRULE:FREQ=DAILY;COUNT=10"
        ));
        assert!(contains("DTSTART:20210101T090000Z\nRRULE:FREQ=WEEKLY"));
        // Yearly with no occurrence in the window: still copied
        assert!(contains("DTSTART;VALUE=DATE:20210301\nRRULE:FREQ=YEARLY"));
        assert!(contains("DTSTART:20210101T090000Z\nRDATE:20210201T090000Z"));
    }

    #[test]
    fn test_series_starting_after_the_window_is_dropped() {
        assert!(!contains("DTSTART:20270101T090000Z\nRRULE:FREQ=WEEKLY"));
    }

    #[test]
    fn test_a_moved_occurrence_counts() {
        let master = "BEGIN:VEVENT\r\nUID:x\r\nDTSTART:20210101T090000Z\r\nRRULE:FREQ=WEEKLY;UNTIL=20220101T000000Z\r\nEND:VEVENT\r\n";
        let moved = "BEGIN:VEVENT\r\nUID:x\r\nRECURRENCE-ID:20210108T090000Z\r\nDTSTART:20260615T090000Z\r\nEND:VEVENT\r\n";
        let ical = |events: &str| format!("BEGIN:VCALENDAR\r\n{events}END:VCALENDAR\r\n");
        // The series ended years ago, one occurrence moved into the window
        assert!(!WINDOW.contains(&ical(master), now()));
        assert!(WINDOW.contains(&ical(&format!("{master}{moved}")), now()));
        assert!(WINDOW.contains(&ical(moved), now()));
    }

    #[test]
    fn test_alarms_do_not_count() {
        assert!(!contains(
            "DTSTART:20200101T090000Z\nBEGIN:VALARM\nTRIGGER;VALUE=DATE-TIME:20260615T090000Z\nDTSTART:20260615T090000Z\nEND:VALARM"
        ));
    }

    #[test]
    fn test_unreadable_start_is_kept() {
        assert!(contains("SUMMARY:no start"));
        assert!(contains("DTSTART:soon"));
    }

    #[test]
    fn test_folded_lines() {
        assert!(!contains(
            "DTSTART:20270101T090000Z\nRRULE:FREQ=WEEKLY;BY\n DAY=MO"
        ));
        assert!(contains(
            "DTSTART:20250101T090000Z\nRRULE:FREQ=WEEKLY;UNTIL=2026\n 0601T000000Z"
        ));
    }

    #[test]
    fn test_huge_window() {
        let all = Window {
            days_past: u64::MAX,
            days_future: u64::MAX,
        };
        assert!(all.contains(&event("DTSTART:19000101T090000Z"), now()));
    }
}
