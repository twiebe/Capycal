//! The preview in the Settings window: a sample event and the copy a sync
//! would make of it, through the library's own pipeline.

use capycal::SyncConfig;
use chrono::{NaiveDateTime, Utc};

use crate::settings_view::EventPreview;

/// A Tuesday meeting with a place, notes and a reminder.
const SAMPLE: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//capycal//preview//EN\r\n\
    BEGIN:VEVENT\r\nUID:preview\r\nDTSTAMP:20261015T000000Z\r\n\
    DTSTART:20261020T090000\r\nDTEND:20261020T100000\r\nSUMMARY:Design review\r\n\
    LOCATION:Room 3\r\nDESCRIPTION:Agenda in the doc\r\n\
    BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT10M\r\nDESCRIPTION:Reminder\r\nEND:VALARM\r\n\
    END:VEVENT\r\nEND:VCALENDAR\r\n";

/// What the copy of the sample shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Copy {
    Event(EventPreview),
    /// A transform skips the sample.
    Skipped,
    /// The sync is not valid yet.
    Unknown,
}

/// The event shown: `event` (a real one, as iCal) or the sample.
pub fn source(event: Option<&str>) -> EventPreview {
    read(event.unwrap_or(SAMPLE))
}

/// The copy `sync` makes of `event` or the sample.
pub fn copy(sync: Option<&SyncConfig>, event: Option<&str>) -> Copy {
    let event = event.unwrap_or(SAMPLE);
    match sync.map(|s| s.preview(event, Utc::now())) {
        Some(Ok(Some(ical))) => Copy::Event(read(&ical)),
        Some(Ok(None)) => Copy::Skipped,
        _ => Copy::Unknown,
    }
}

/// The event's time, title and the rest in one line.
fn read(ical: &str) -> EventPreview {
    let unfolded = ical.replace("\r\n ", "").replace("\r\n\t", "");
    let mut in_alarm = false;
    let (mut start, mut end, mut title) = (None, None, String::new());
    let mut day = None;
    let (mut place, mut notes, mut alarm) = (String::new(), String::new(), String::new());
    for line in unfolded.lines() {
        match line {
            "BEGIN:VALARM" => in_alarm = true,
            "END:VALARM" => in_alarm = false,
            _ => {}
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let name = key.split(';').next().unwrap_or(key);
        let value = value
            .replace(r"\,", ",")
            .replace(r"\;", ";")
            .replace(r"\n", " ");
        match (in_alarm, name) {
            (true, "TRIGGER") if alarm.is_empty() => alarm = reminder(&value),
            (false, "DTSTART") => {
                start = time(&value);
                day = chrono::NaiveDate::parse_from_str(&value, "%Y%m%d").ok();
            }
            (false, "DTEND") => end = time(&value),
            (false, "SUMMARY") => title = value,
            (false, "LOCATION") => place = value,
            (false, "DESCRIPTION") => notes = value,
            _ => {}
        }
    }
    let time = match (start, end) {
        (Some((start, utc)), Some((end, _))) => format!(
            "{} {} - {}{}",
            start.format("%a"),
            start.format("%H:%M"),
            end.format("%H:%M"),
            if utc { " UTC" } else { "" }
        ),
        _ => match day {
            Some(day) => format!("{}, all day", day.format("%a")),
            None => String::new(),
        },
    };
    EventPreview {
        time,
        title: if title.is_empty() {
            "(no title)".to_string()
        } else {
            title
        },
        detail: [place, notes, alarm]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(", "),
    }
}

/// A date-time and whether it is in UTC (a real event's may be).
fn time(value: &str) -> Option<(NaiveDateTime, bool)> {
    NaiveDateTime::parse_from_str(value.trim_end_matches('Z'), "%Y%m%dT%H%M%S")
        .ok()
        .map(|t| (t, value.ends_with('Z')))
}

/// "reminder 10 min before" for a trigger like `-PT10M`.
fn reminder(trigger: &str) -> String {
    let amount = trigger.trim_start_matches('-').trim_start_matches('P');
    let (number, unit) = match amount.strip_prefix('T') {
        Some(t) => (t.trim_end_matches(['M', 'H', 'S']), t.chars().last()),
        None => (amount.trim_end_matches(['D', 'W']), amount.chars().last()),
    };
    let unit = match unit {
        Some('M') => "min",
        Some('H') => "h",
        Some('D') => "days",
        Some('W') => "weeks",
        _ => return "reminder".to_string(),
    };
    format!("reminder {number} {unit} before")
}

#[cfg(test)]
mod tests {
    use super::*;
    use capycal::{TransformAction, TransformConfig};
    use std::time::Duration;

    #[test]
    fn test_real_events_say_utc_and_all_day() {
        let event = |start: &str, end: &str| {
            format!(
                "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\n{start}\r\n{end}\r\nSUMMARY:x\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
            )
        };
        let utc = event("DTSTART:20261021T140000Z", "DTEND:20261021T150000Z");
        assert_eq!(source(Some(&utc)).time, "Wed 14:00 - 15:00 UTC");
        let day = event("DTSTART;VALUE=DATE:20261021", "DTEND;VALUE=DATE:20261022");
        assert_eq!(source(Some(&day)).time, "Wed, all day");
    }

    #[test]
    fn test_source_reads_the_sample() {
        let source = source(None);
        assert_eq!(source.time, "Tue 09:00 - 10:00");
        assert_eq!(source.title, "Design review");
        assert_eq!(
            source.detail,
            "Room 3, Agenda in the doc, reminder 10 min before"
        );
    }

    #[test]
    fn test_copy_follows_fields_and_transforms() {
        let mut sync = SyncConfig::new("s", "a", "b");
        assert_eq!(
            copy(Some(&sync), None),
            Copy::Event(EventPreview {
                time: "Tue 09:00 - 10:00".into(),
                title: "(no title)".into(),
                detail: String::new(),
            })
        );
        let mut alarm = TransformConfig::new(TransformAction::AddAlarm);
        alarm.trigger = Some(Duration::from_secs(3600));
        sync.fields = vec!["SUMMARY".into()];
        sync.transforms = vec![alarm];
        let Copy::Event(event) = copy(Some(&sync), None) else {
            panic!("no copy");
        };
        assert_eq!(event.title, "Design review");
        assert_eq!(event.detail, "reminder 1 h before");
        assert_eq!(copy(None, None), Copy::Unknown);
    }
}
