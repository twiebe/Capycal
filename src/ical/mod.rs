pub mod filter;
pub mod marker;
pub mod timezone;
pub mod transform;
pub mod window;

use std::collections::HashSet;

use chrono::{DateTime, Utc};

use crate::ical::filter::filter_vevent_properties;
use crate::ical::marker::marker_lines;
use crate::ical::transform::{Transform, TransformContext, add_alarms, apply_transforms};

pub const PRODID: &str = "-//capycal//EN";

/// Unfold iCal content lines per RFC 5545 section 3.1.
/// Continuation lines start with a single space or tab and should be
/// joined to the previous line.
pub fn unfold(ical_data: &str) -> String {
    let mut result = String::with_capacity(ical_data.len());
    for line in ical_data.lines() {
        if line.starts_with(' ') || line.starts_with('\t') {
            // Continuation line - append without the leading whitespace
            result.push_str(&line[1..]);
        } else {
            if !result.is_empty() {
                result.push_str("\r\n");
            }
            result.push_str(line.trim_end());
        }
    }
    result
}

/// Extract the UID from a VEVENT in an iCal string.
pub fn extract_uid(ical_data: &str) -> Option<String> {
    let unfolded = unfold(ical_data);
    for line in unfolded.lines() {
        let trimmed = line.trim();
        if let Some(uid) = trimmed.strip_prefix("UID:") {
            return Some(uid.trim().to_string());
        }
    }
    None
}

/// Process a source event through the full pipeline:
/// 1. Apply the transforms (but `add_alarm`) to the whole source event
/// 2. Filter to the structural fields, `fields` and the fields `set_field` sets
/// 3. Add the alarms of `add_alarm`
/// 4. Stamp with capycal markers, set the new UID and DTSTAMP
#[allow(clippy::too_many_arguments)]
pub fn process_event(
    source_ical: &str,
    allowed_fields: &HashSet<String>,
    transforms: &[Transform],
    ctx: &TransformContext,
    source_uid: &str,
    new_uid: &str,
    sync_name: &str,
    source_calendar_name: &str,
    now: DateTime<Utc>,
) -> Option<String> {
    // A cancelled event is not copied; a synced copy is removed like a
    // filtered one
    if transform::is_cancelled(source_ical) {
        return None;
    }
    // Tasks and journal entries share collections with events on some
    // servers; only events are copied
    if !unfold(source_ical)
        .lines()
        .any(|l| l.trim_end() == "BEGIN:VEVENT")
    {
        return None;
    }
    // Transforms see the whole event, so a condition can test a property
    // that is not copied
    let applied = apply_transforms(&unfold(source_ical), transforms, ctx)?;

    // Only what a set_field actually set: a set_field whose conditions did
    // not match must not let the source value through
    let mut allowed = allowed_fields.clone();
    allowed.extend(applied.set_fields);
    let filtered = filter_vevent_properties(&applied.ical, &allowed);

    // After the filter, so the source alarms only stay with "VALARM" in fields
    let transformed = add_alarms(&filtered, &applied.alarms);

    // Replace UID and add markers
    let mut lines: Vec<String> = transformed.lines().map(|l| l.to_string()).collect();

    for line in lines.iter_mut() {
        if line.starts_with("UID:") {
            *line = format!("UID:{new_uid}");
        }
    }

    // Insert markers before END:VEVENT
    if let Some(pos) = lines.iter().position(|l| l.trim() == "END:VEVENT") {
        let markers = marker_lines(source_calendar_name, source_uid, sync_name);
        for (i, marker_line) in markers.lines().enumerate() {
            lines.insert(pos + i, marker_line.to_string());
        }
    }

    let dtstamp = format_utc(now.timestamp()).unwrap_or_default();
    for line in lines.iter_mut() {
        if line.starts_with("DTSTAMP:") {
            *line = format!("DTSTAMP:{dtstamp}");
        }
    }

    // Set PRODID
    for line in lines.iter_mut() {
        if line.starts_with("PRODID:") {
            *line = format!("PRODID:{PRODID}");
        }
    }

    Some(lines.join("\r\n"))
}

/// Format a Unix timestamp as an iCal UTC date-time, e.g. "20240115T090000Z".
pub fn format_utc(secs: i64) -> Option<String> {
    chrono::DateTime::from_timestamp(secs, 0).map(|dt| dt.format("%Y%m%dT%H%M%SZ").to_string())
}

/// Property name of a content line: everything before the first ':' or ';'.
pub fn property_name(line: &str) -> &str {
    let end = line.find([':', ';']).unwrap_or(line.len());
    &line[..end]
}

/// Byte position of the ':' that starts a property line's value: the first
/// one outside a quoted parameter such as ALTREP="http://...".
pub fn value_colon(line: &str) -> Option<usize> {
    let mut quoted = false;
    line.char_indices().find_map(|(i, c)| {
        match c {
            '"' => quoted = !quoted,
            ':' if !quoted => return Some(i),
            _ => {}
        }
        None
    })
}

/// Properties a server or capycal sets anew on every write.
const VOLATILE_PROPERTIES: [&str; 4] = ["DTSTAMP", "LAST-MODIFIED", "CREATED", "SEQUENCE"];

/// Whether two iCal objects have the same content: equal lines in the same
/// order after unfolding, ignoring line endings, trailing whitespace, blank
/// lines and the volatile DTSTAMP, LAST-MODIFIED, CREATED and SEQUENCE.
pub fn same_content(a: &str, b: &str) -> bool {
    fn lines(ical: &str) -> Vec<String> {
        unfold(ical)
            .lines()
            .map(str::trim_end)
            .filter(|l| !l.is_empty() && !VOLATILE_PROPERTIES.contains(&property_name(l)))
            .map(str::to_string)
            .collect()
    }
    lines(a) == lines(b)
}

/// True if a property line has the parameter VALUE=DATE (not VALUE=DATE-TIME).
pub fn is_date_value(line: &str) -> bool {
    let params = &line[..line.find(':').unwrap_or(line.len())];
    params.split(';').skip(1).any(|p| p == "VALUE=DATE")
}

/// TZID parameter of a property line, e.g.
/// "DTSTART;TZID=Europe/Berlin:20240115T090000" -> Some("Europe/Berlin").
pub fn extract_tzid(line: &str) -> Option<&str> {
    let params = &line[..line.find(':').unwrap_or(line.len())];
    let rest = &params[params.find("TZID=")? + 5..];
    let rest = rest.strip_prefix('"').unwrap_or(rest);
    let tzid = &rest[..rest.find([';', ',', '"']).unwrap_or(rest.len())];
    (!tzid.is_empty()).then_some(tzid)
}

/// A DISPLAY VALARM block around the given TRIGGER line.
pub fn valarm_lines(trigger_line: String) -> [String; 5] {
    [
        "BEGIN:VALARM".to_string(),
        "ACTION:DISPLAY".to_string(),
        "DESCRIPTION:Reminder".to_string(),
        trigger_line,
        "END:VALARM".to_string(),
    ]
}

/// Escape a TEXT property value (RFC 5545 section 3.3.11).
pub fn escape_text(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace(';', "\\;")
        .replace(',', "\\,")
        .replace('\r', "")
        .replace('\n', "\\n")
}

/// Reverse of `escape_text`. Single pass, so an escaped backslash followed by "n" is not read as a newline.
pub fn unescape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n' | 'N') => out.push('\n'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_same_content_ignores_volatile_lines_and_folding() {
        let ours = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:a\r\nDTSTAMP:20260101T000000Z\r\n\
                    SUMMARY:Busy (work)\r\nEND:VEVENT\r\nEND:VCALENDAR";
        let stored = "BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:a\nDTSTAMP:20260202T000000Z\n\
                      SEQUENCE:3\nSUMMARY:Busy\n  (work)\nEND:VEVENT\nEND:VCALENDAR\n";
        assert!(same_content(ours, stored));
        assert!(!same_content(ours, &stored.replace("work", "home")));
        // Order matters
        let swapped = ours
            .replace("UID:a\r\nDTSTAMP", "DTSTAMP")
            .replace("END:VEVENT", "UID:a\r\nEND:VEVENT");
        assert!(!same_content(ours, &swapped));
    }

    #[test]
    fn test_is_date_value() {
        assert!(is_date_value("DTSTART;VALUE=DATE:20240115"));
        assert!(is_date_value("DTEND;X-FOO=1;VALUE=DATE:20240116"));
        assert!(!is_date_value("DTSTART;VALUE=DATE-TIME:20240115T090000Z"));
        assert!(!is_date_value("DTSTART:20240115T090000Z"));
        assert!(!is_date_value("SUMMARY:VALUE=DATE"));
        assert!(!is_date_value("VALUE=DATE"));
    }

    #[test]
    fn test_text_escaping_roundtrip() {
        let raw = "a, b; c\\d\nline";
        let escaped = escape_text(raw);
        assert_eq!(escaped, "a\\, b\\; c\\\\d\\nline");
        assert_eq!(unescape_text(&escaped), raw);
        assert_eq!(unescape_text("x\\\\ny"), "x\\ny");
        assert_eq!(unescape_text("x\\Ny"), "x\ny");
        assert_eq!(unescape_text("trailing\\"), "trailing\\");
    }

    #[test]
    fn test_extract_uid() {
        let ical = "BEGIN:VEVENT\r\nUID:abc-123\r\nSUMMARY:Test\r\nEND:VEVENT";
        assert_eq!(extract_uid(ical), Some("abc-123".to_string()));
    }

    #[test]
    fn test_extract_uid_missing() {
        let ical = "BEGIN:VEVENT\r\nSUMMARY:Test\r\nEND:VEVENT";
        assert_eq!(extract_uid(ical), None);
    }

    #[test]
    fn test_format_utc() {
        assert_eq!(format_utc(0), Some("19700101T000000Z".to_string()));
        assert_eq!(format_utc(1705309200), Some("20240115T090000Z".to_string()));
        // Leap day
        assert_eq!(format_utc(1709251199), Some("20240229T235959Z".to_string()));
    }

    #[test]
    fn test_property_name() {
        assert_eq!(property_name("SUMMARY:Test"), "SUMMARY");
        assert_eq!(
            property_name("DTSTART;TZID=Europe/Berlin:20240115"),
            "DTSTART"
        );
        assert_eq!(property_name("END"), "END");
    }

    #[test]
    fn test_extract_tzid() {
        assert_eq!(
            extract_tzid("DTSTART;TZID=Europe/Berlin:20240115T090000"),
            Some("Europe/Berlin")
        );
        assert_eq!(
            extract_tzid("DTSTART;TZID=Europe/Berlin;X-A=b:20240115T090000"),
            Some("Europe/Berlin")
        );
        assert_eq!(
            extract_tzid("DTSTART;TZID=\"Europe/Berlin\":20240115T090000"),
            Some("Europe/Berlin")
        );
        assert_eq!(extract_tzid("DTSTART:20240115T090000Z"), None);
        assert_eq!(extract_tzid("DESCRIPTION:see TZID=Foo"), None);
        assert_eq!(extract_tzid("DTSTART;TZID=:20240115T090000"), None);
    }

    #[test]
    fn test_valarm_lines() {
        let alarm = valarm_lines("TRIGGER:-PT15M".to_string());
        assert_eq!(alarm[0], "BEGIN:VALARM");
        assert_eq!(alarm[3], "TRIGGER:-PT15M");
        assert_eq!(alarm[4], "END:VALARM");
    }

    #[test]
    fn test_unfold_continuation_lines() {
        let folded = "UID:very-long-uid-that-was\r\n  folded-across-lines\r\nSUMMARY:Test";
        let unfolded = unfold(folded);
        assert!(unfolded.contains("UID:very-long-uid-that-was folded-across-lines"));
        assert!(unfolded.contains("SUMMARY:Test"));
    }

    #[test]
    fn test_unfold_tab_continuation() {
        let folded = "DESCRIPTION:line one\r\n\tcontinued";
        let unfolded = unfold(folded);
        assert!(unfolded.contains("DESCRIPTION:line onecontinued"));
    }

    #[test]
    fn test_extract_uid_folded() {
        let ical = "BEGIN:VEVENT\r\nUI\r\n D:folded-uid-123\r\nSUMMARY:Test\r\nEND:VEVENT";
        assert_eq!(extract_uid(ical), Some("folded-uid-123".to_string()));
    }

    #[test]
    fn test_process_event_skips_resources_without_an_event() {
        use crate::ical::transform::TransformContext;

        let ctx = TransformContext {
            source_name: "work".to_string(),
            source_url: String::new(),
            sync_name: "s".to_string(),
        };
        let todo = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VTODO\r\nUID:t\r\n\
            DTSTAMP:20240101T000000Z\r\nSUMMARY:Secret task\r\nEND:VTODO\r\nEND:VCALENDAR";
        let processed = process_event(
            todo,
            &HashSet::new(),
            &[],
            &ctx,
            "t",
            "capycal-t",
            "s",
            "work",
            DateTime::from_timestamp(1767225600, 0).unwrap(),
        );
        assert_eq!(processed, None);
    }

    #[test]
    fn test_process_event_skips_cancelled_events() {
        use crate::ical::transform::TransformContext;

        let event = |status: &str, overridden: &str| {
            format!(
                "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:s\r\nDTSTAMP:20240101T000000Z\r\n\
                 DTSTART:20240115T090000Z\r\nRRULE:FREQ=WEEKLY\r\n{status}END:VEVENT\r\n\
                 BEGIN:VEVENT\r\nUID:s\r\nDTSTAMP:20240101T000000Z\r\nRECURRENCE-ID:20240122T090000Z\r\n\
                 DTSTART:20240122T090000Z\r\n{overridden}END:VEVENT\r\nEND:VCALENDAR"
            )
        };
        let ctx = TransformContext {
            source_name: "work".to_string(),
            source_url: String::new(),
            sync_name: "s".to_string(),
        };
        let process = |ical: &str| {
            process_event(
                ical,
                &HashSet::new(),
                &[],
                &ctx,
                "s",
                "capycal-s",
                "s",
                "work",
                DateTime::from_timestamp(1767225600, 0).unwrap(),
            )
        };
        assert!(process(&event("STATUS:CANCELLED\r\n", "")).is_none());
        // A cancelled occurrence keeps the series and is copied as cancelled
        let series = process(&event("STATUS:CONFIRMED\r\n", "STATUS:CANCELLED\r\n")).unwrap();
        assert!(series.contains("STATUS:CANCELLED"));
        assert!(series.contains("RRULE:FREQ=WEEKLY"));
    }

    #[test]
    fn test_exclude_regex_matches_a_property_fields_drops() {
        use crate::ical::transform::{Action, Condition, Test, Transform, TransformContext};

        let source = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:a\r\nDTSTART:20240115T090000Z\r\n\
                      SUMMARY:Lunch\r\nEND:VEVENT\r\nEND:VCALENDAR";
        let ctx = TransformContext {
            source_name: "work".to_string(),
            source_url: String::new(),
            sync_name: "s".to_string(),
        };
        let process = |pattern: &str| {
            let transforms = [Transform {
                action: Action::Skip,
                when: vec![vec![Condition {
                    test: Test::Matches {
                        field: "SUMMARY".to_string(),
                        regex: regex::Regex::new(pattern).unwrap(),
                    },
                    negate: false,
                }]],
            }];
            process_event(
                source,
                &HashSet::new(),
                &transforms,
                &ctx,
                "a",
                "capycal-a",
                "s",
                "work",
                DateTime::from_timestamp(1767225600, 0).unwrap(),
            )
        };
        assert!(process("(?i)lunch").is_none());
        let kept = process("Dinner").unwrap();
        assert!(!kept.contains("SUMMARY"), "{kept}");
    }

    #[test]
    fn test_transforms_run_in_order_on_the_whole_event_before_fields() {
        use crate::ical::transform::{Action, Condition, Test, Transform, TransformContext};
        use std::time::Duration;

        let source = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:a\r\nDTSTART:20240115T090000Z\r\n\
                      SUMMARY:Lunch\r\nLOCATION:Can\r\n teen\r\nX-SECRET:1\r\n\
                      BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT1H\r\nEND:VALARM\r\n\
                      END:VEVENT\r\nEND:VCALENDAR";
        let ctx = TransformContext {
            source_name: "work".to_string(),
            source_url: String::new(),
            sync_name: "s".to_string(),
        };
        let process = |fields: &[&str], transforms: &[Transform]| {
            process_event(
                source,
                &fields.iter().map(|f| f.to_string()).collect(),
                transforms,
                &ctx,
                "a",
                "capycal-a",
                "s",
                "work",
                DateTime::from_timestamp(1767225600, 0).unwrap(),
            )
        };
        let set = Transform::from(Action::Set {
            field: "SUMMARY".to_string(),
            value: "Busy".to_string(),
        });
        let contains = |field: &str, text: &str| Condition {
            test: Test::Contains {
                field: field.to_string(),
                text: text.to_string(),
            },
            negate: false,
        };
        let exclude = |search: &str| Transform {
            action: Action::Skip,
            when: vec![vec![contains("SUMMARY", search)]],
        };
        let alarm = Transform::from(Action::AddAlarm {
            trigger: Duration::from_secs(600),
        });
        // An exclude sees the value of an earlier set_field, a later one the source
        assert!(process(&[], &[set.clone(), exclude("Busy")]).is_none());
        assert!(process(&[], &[exclude("Lunch"), set.clone()]).is_none());
        let kept = process(&[], &[exclude("Busy"), set.clone(), alarm.clone()]).unwrap();
        // set_field keeps its field, fields drops the rest after the transforms
        assert!(kept.contains("SUMMARY:Busy"), "{kept}");
        assert!(
            !kept.contains("LOCATION") && !kept.contains("X-SECRET"),
            "{kept}"
        );
        // add_alarm adds to the filtered event: the source alarm needs "VALARM"
        assert!(
            kept.contains("TRIGGER:-PT10M") && !kept.contains("-PT1H"),
            "{kept}"
        );
        let both = process(&["VALARM", "LOCATION"], std::slice::from_ref(&alarm)).unwrap();
        assert!(
            both.contains("TRIGGER:-PT10M") && both.contains("TRIGGER:-PT1H"),
            "{both}"
        );
        // Folded source lines are unfolded first
        assert!(both.contains("LOCATION:Canteen"), "{both}");
        let exclude_location = Transform {
            action: Action::Skip,
            when: vec![vec![contains("LOCATION", "Canteen")]],
        };
        assert!(process(&[], &[exclude_location]).is_none());
        // A set_field whose condition does not match lets nothing through
        let busy_if_dinner = Transform {
            when: vec![vec![contains("SUMMARY", "Dinner")]],
            ..set.clone()
        };
        let hidden = process(&[], &[busy_if_dinner]).unwrap();
        assert!(!hidden.contains("SUMMARY"), "{hidden}");
        // An alarm checks its conditions where it stands, before fields
        let alarm_if_canteen = Transform {
            when: vec![vec![contains("LOCATION", "Canteen")]],
            ..alarm.clone()
        };
        let reminded = process(&[], &[alarm_if_canteen]).unwrap();
        assert!(reminded.contains("TRIGGER:-PT10M"), "{reminded}");
    }

    #[test]
    fn test_process_event_end_to_end() {
        use crate::ical::transform::{Action, Transform, TransformContext};

        let source = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\nBEGIN:VEVENT\r\nUID:original-uid\r\nDTSTAMP:20240101T000000Z\r\nDTSTART:20240115T090000Z\r\nDTEND:20240115T100000Z\r\nSUMMARY:Secret Meeting\r\nLOCATION:Room 42\r\nDESCRIPTION:Confidential\r\nEND:VEVENT\r\nEND:VCALENDAR";

        let allowed: HashSet<String> = ["DTSTART", "DTEND"].iter().map(|s| s.to_string()).collect();

        let transforms = vec![Transform::from(Action::Set {
            field: "SUMMARY".to_string(),
            value: "Busy ({{source_name}})".to_string(),
        })];

        let ctx = TransformContext {
            source_name: "work".to_string(),
            source_url: "https://example.com/cal/".to_string(),
            sync_name: "work-to-personal".to_string(),
        };

        let result = process_event(
            source,
            &allowed,
            &transforms,
            &ctx,
            "original-uid",
            "capycal-new-uid",
            "work-to-personal",
            "work",
            DateTime::from_timestamp(1767225600, 0).unwrap(),
        )
        .unwrap();

        // UID replaced
        assert!(result.contains("UID:capycal-new-uid"));
        assert!(!result.contains("UID:original-uid"));

        // Filtered: only DTSTART and DTEND kept (plus structural)
        assert!(result.contains("DTSTART:20240115T090000Z"));
        assert!(result.contains("DTEND:20240115T100000Z"));
        assert!(!result.contains("LOCATION"));
        assert!(!result.contains("DESCRIPTION"));
        assert!(!result.contains("Confidential"));

        // Transform applied
        assert!(result.contains("SUMMARY:Busy (work)"));
        assert!(!result.contains("Secret Meeting"));

        // Markers present
        assert!(result.contains("X-CAPYCAL-SOURCE:work/original-uid"));
        assert!(result.contains("X-CAPYCAL-SYNC:work-to-personal"));

        // PRODID set
        assert!(result.contains("PRODID:-//capycal//EN"));

        // DTSTAMP stamped with the given time
        assert!(!result.contains("DTSTAMP:20240101T000000Z"));
        assert!(result.contains("DTSTAMP:20260101T000000Z"));

        // Valid structure
        assert!(result.contains("BEGIN:VCALENDAR"));
        assert!(result.contains("END:VCALENDAR"));
        assert!(result.contains("BEGIN:VEVENT"));
        assert!(result.contains("END:VEVENT"));
    }

    #[test]
    fn test_unfold_empty_input() {
        assert_eq!(unfold(""), "");
    }

    #[test]
    fn test_unfold_no_continuation() {
        let input = "LINE1\r\nLINE2\r\nLINE3";
        let result = unfold(input);
        assert_eq!(result, "LINE1\r\nLINE2\r\nLINE3");
    }
}
