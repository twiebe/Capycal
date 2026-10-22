use std::collections::HashSet;

use super::{PRODID, extract_tzid, property_name, unfold};

/// Properties every copied VEVENT keeps, whatever `fields` lists: identity,
/// time, recurrence and availability. Without any of them the copy would
/// show at the wrong time or as the wrong kind of event. RECURRENCE-ID marks
/// an overridden occurrence of a series; without it the extra VEVENT would be
/// a second master with the same UID.
pub const STRUCTURAL_FIELDS: [&str; 11] = [
    "UID",
    "DTSTAMP",
    "RECURRENCE-ID",
    "DTSTART",
    "DTEND",
    "DURATION",
    "RRULE",
    "RDATE",
    "EXDATE",
    "STATUS",
    "TRANSP",
];

/// Filter a VEVENT's properties to only include allowed fields.
/// Always preserves `STRUCTURAL_FIELDS` and any VTIMEZONE components
/// referenced by the event.
/// Handles multiple VEVENTs (recurring events with exceptions).
pub fn filter_vevent_properties(ical_data: &str, allowed_fields: &HashSet<String>) -> String {
    let unfolded = unfold(ical_data);
    let mut output_lines: Vec<String> = Vec::new();
    let mut in_vevent = false;
    let mut in_vtimezone = false;
    let mut in_valarm = false;
    let keep_valarm = allowed_fields.contains("VALARM");
    let mut vtimezone_blocks: Vec<String> = Vec::new();
    let mut current_vtimezone: Vec<String> = Vec::new();
    let mut referenced_tzids: HashSet<String> = HashSet::new();
    let mut vcalendar_header: Vec<String> = Vec::new();
    let mut vevent_lines: Vec<String> = Vec::new();
    let mut seen_vcalendar = false;
    let mut vtimezone_depth = 0;
    // Nesting depth inside a component other than VEVENT or VTIMEZONE
    // (VTODO, VJOURNAL, ...), which is dropped
    let mut other_depth = 0;

    let always_keep =
        |name: &str| ["BEGIN", "END"].contains(&name) || STRUCTURAL_FIELDS.contains(&name);

    let lines: Vec<&str> = unfolded.lines().collect();

    for line in &lines {
        let line = line.trim_end();

        if line.starts_with("BEGIN:VTIMEZONE") {
            in_vtimezone = true;
            vtimezone_depth = 1;
            current_vtimezone.clear();
            current_vtimezone.push(line.to_string());
            continue;
        }

        if in_vtimezone {
            current_vtimezone.push(line.to_string());
            if line.starts_with("BEGIN:") {
                vtimezone_depth += 1;
            }
            if line.starts_with("END:") {
                vtimezone_depth -= 1;
                if vtimezone_depth == 0 {
                    in_vtimezone = false;
                    vtimezone_blocks.push(current_vtimezone.join("\r\n"));
                    current_vtimezone.clear();
                }
            }
            continue;
        }

        if line.starts_with("BEGIN:VEVENT") {
            in_vevent = true;
            vevent_lines.push(line.to_string());
            continue;
        }

        if line.starts_with("END:VEVENT") {
            in_vevent = false;
            vevent_lines.push(line.to_string());
            // Don't break - there may be more VEVENTs (recurring exceptions)
            continue;
        }

        if in_vevent {
            // Handle VALARM sub-component as a block
            if line.starts_with("BEGIN:VALARM") {
                in_valarm = true;
                if keep_valarm {
                    vevent_lines.push(line.to_string());
                }
                continue;
            }
            if in_valarm {
                if keep_valarm {
                    vevent_lines.push(line.to_string());
                }
                if line.starts_with("END:VALARM") {
                    in_valarm = false;
                }
                continue;
            }

            let prop_name = property_name(line);

            let keep = always_keep(prop_name) || allowed_fields.contains(prop_name);

            if keep {
                vevent_lines.push(line.to_string());
                // Track TZID references
                if let Some(tzid) = extract_tzid(line) {
                    referenced_tzids.insert(tzid.to_string());
                }
            }
            continue;
        }

        if other_depth > 0 || (line.starts_with("BEGIN:") && line != "BEGIN:VCALENDAR") {
            if line.starts_with("BEGIN:") {
                other_depth += 1;
            } else if line.starts_with("END:") {
                other_depth -= 1;
            }
            continue;
        }

        // Outside VEVENT/VTIMEZONE - this is VCALENDAR-level
        if line.starts_with("BEGIN:VCALENDAR") {
            seen_vcalendar = true;
        }
        if seen_vcalendar {
            vcalendar_header.push(line.to_string());
        }
        if line.starts_with("END:VCALENDAR") {
            // We'll rebuild this
            vcalendar_header.pop(); // Remove END:VCALENDAR, we add it later
        }
    }

    // Build output: VCALENDAR header, referenced VTIMEZONEs, filtered VEVENT, END:VCALENDAR
    if vcalendar_header.is_empty() {
        // No VCALENDAR wrapper found, create a minimal one
        output_lines.push("BEGIN:VCALENDAR".to_string());
        output_lines.push("VERSION:2.0".to_string());
        output_lines.push(format!("PRODID:{PRODID}"));
    } else {
        output_lines.extend(vcalendar_header);
    }

    // Include only referenced VTIMEZONE blocks
    for tz_block in &vtimezone_blocks {
        if let Some(tzid) = extract_vtimezone_tzid(tz_block)
            && referenced_tzids.contains(&tzid)
        {
            output_lines.push(tz_block.clone());
        }
    }

    output_lines.extend(vevent_lines);
    output_lines.push("END:VCALENDAR".to_string());

    output_lines.join("\r\n")
}

fn extract_vtimezone_tzid(tz_block: &str) -> Option<String> {
    for line in tz_block.lines() {
        if line.starts_with("TZID:") || line.starts_with("TZID;") {
            let value = &line[5..];
            return Some(value.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_filter_drops_other_components() {
        let ical = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n\
            BEGIN:VEVENT\r\nUID:e\r\nDTSTART:20240115T090000Z\r\nSUMMARY:Meeting\r\nEND:VEVENT\r\n\
            BEGIN:VTODO\r\nUID:t\r\nSUMMARY:Secret task\r\n\
            BEGIN:VALARM\r\nACTION:DISPLAY\r\nEND:VALARM\r\nEND:VTODO\r\nEND:VCALENDAR";
        let result = filter_vevent_properties(ical, &HashSet::new());
        assert!(!result.contains("VTODO"), "{result}");
        assert!(!result.contains("Secret task"), "{result}");
        assert!(!result.contains("VALARM"), "{result}");
        assert!(result.contains("UID:e"));
        assert!(result.ends_with("END:VCALENDAR"));
    }

    #[test]
    fn test_filter_basic() {
        let ical = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:123\r\nDTSTAMP:20240101T000000Z\r\nDTSTART:20240115T090000Z\r\nDTEND:20240115T100000Z\r\nSUMMARY:Secret Meeting\r\nLOCATION:Room 42\r\nDESCRIPTION:Confidential details\r\nEND:VEVENT\r\nEND:VCALENDAR";

        let allowed: HashSet<String> = ["DTSTART", "DTEND"].iter().map(|s| s.to_string()).collect();
        let result = filter_vevent_properties(ical, &allowed);

        assert!(result.contains("DTSTART:20240115T090000Z"));
        assert!(result.contains("DTEND:20240115T100000Z"));
        assert!(result.contains("UID:123")); // always kept
        assert!(!result.contains("SUMMARY"));
        assert!(!result.contains("LOCATION"));
        assert!(!result.contains("DESCRIPTION"));
    }

    #[test]
    fn test_filter_with_timezone() {
        let ical = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VTIMEZONE\r\nTZID:Europe/Berlin\r\nBEGIN:STANDARD\r\nDTSTART:19701025T030000\r\nEND:STANDARD\r\nEND:VTIMEZONE\r\nBEGIN:VTIMEZONE\r\nTZID:US/Pacific\r\nBEGIN:STANDARD\r\nDTSTART:19701025T030000\r\nEND:STANDARD\r\nEND:VTIMEZONE\r\nBEGIN:VEVENT\r\nUID:456\r\nDTSTAMP:20240101T000000Z\r\nDTSTART;TZID=Europe/Berlin:20240115T090000\r\nDTEND;TZID=Europe/Berlin:20240115T100000\r\nSUMMARY:Meeting\r\nEND:VEVENT\r\nEND:VCALENDAR";

        let allowed: HashSet<String> = ["DTSTART", "DTEND"].iter().map(|s| s.to_string()).collect();
        let result = filter_vevent_properties(ical, &allowed);

        // Referenced VTIMEZONE block should be complete
        assert!(result.contains("BEGIN:VTIMEZONE"));
        assert!(result.contains("TZID:Europe/Berlin"));
        assert!(result.contains("END:VTIMEZONE"));
        // Unreferenced VTIMEZONE should be excluded entirely
        assert!(!result.contains("US/Pacific"));
    }

    #[test]
    fn test_filter_empty_fields_keeps_only_structural() {
        let ical = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:123\r\nDTSTAMP:20240101T000000Z\r\nDTSTART:20240115T090000Z\r\nSUMMARY:Meeting\r\nLOCATION:Room\r\nEND:VEVENT\r\nEND:VCALENDAR";

        let allowed: HashSet<String> = HashSet::new();
        let result = filter_vevent_properties(ical, &allowed);

        // Structural properties always kept
        assert!(result.contains("UID:123"));
        assert!(result.contains("DTSTAMP:"));
        assert!(result.contains("BEGIN:VEVENT"));
        assert!(result.contains("END:VEVENT"));

        // Time and recurrence are structural too, content is stripped
        assert!(result.contains("DTSTART:20240115T090000Z"));
        assert!(!result.contains("SUMMARY"));
        assert!(!result.contains("LOCATION"));
    }

    #[test]
    fn test_filter_multi_vevent_recurring() {
        let ical = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:recurring-1\r\nDTSTAMP:20240101T000000Z\r\nDTSTART:20240115T090000Z\r\nRRULE:FREQ=WEEKLY\r\nSUMMARY:Weekly Meeting\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:recurring-1\r\nDTSTAMP:20240101T000000Z\r\nRECURRENCE-ID:20240122T090000Z\r\nDTSTART:20240122T100000Z\r\nSUMMARY:Moved Meeting\r\nEND:VEVENT\r\nEND:VCALENDAR";

        let allowed: HashSet<String> = ["DTSTART", "RRULE", "RECURRENCE-ID"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let result = filter_vevent_properties(ical, &allowed);

        // Both VEVENTs should be present
        let vevent_count = result.matches("BEGIN:VEVENT").count();
        assert_eq!(vevent_count, 2, "both VEVENTs should be preserved");

        // First VEVENT keeps RRULE
        assert!(result.contains("RRULE:FREQ=WEEKLY"));

        // Second VEVENT keeps RECURRENCE-ID
        assert!(result.contains("RECURRENCE-ID:20240122T090000Z"));

        // SUMMARY stripped from both
        assert!(!result.contains("SUMMARY"));
    }

    #[test]
    fn test_filter_property_with_parameters() {
        let ical = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:p1\r\nDTSTAMP:20240101T000000Z\r\nDTSTART;TZID=Europe/Berlin:20240115T090000\r\nDTEND;TZID=Europe/Berlin:20240115T100000\r\nSUMMARY;LANGUAGE=en:English Title\r\nEND:VEVENT\r\nEND:VCALENDAR";

        let allowed: HashSet<String> = ["DTSTART", "DTEND"].iter().map(|s| s.to_string()).collect();
        let result = filter_vevent_properties(ical, &allowed);

        // DTSTART with TZID parameter should be kept (property name match is before ';')
        assert!(result.contains("DTSTART;TZID=Europe/Berlin:20240115T090000"));
        assert!(result.contains("DTEND;TZID=Europe/Berlin:20240115T100000"));

        // SUMMARY not in allowed list, should be stripped even with parameters
        assert!(!result.contains("SUMMARY"));
        assert!(!result.contains("English Title"));
    }

    #[test]
    fn test_filter_no_vcalendar_wrapper() {
        // Some contexts might pass just a VEVENT without VCALENDAR
        let ical = "BEGIN:VEVENT\r\nUID:bare-1\r\nDTSTAMP:20240101T000000Z\r\nDTSTART:20240115T090000Z\r\nSUMMARY:Bare Event\r\nEND:VEVENT";

        let allowed: HashSet<String> = ["DTSTART"].iter().map(|s| s.to_string()).collect();
        let result = filter_vevent_properties(ical, &allowed);

        // Should auto-create VCALENDAR wrapper
        assert!(result.contains("BEGIN:VCALENDAR"));
        assert!(result.contains("END:VCALENDAR"));
        assert!(result.contains("PRODID:-//capycal//EN"));
        assert!(result.contains("DTSTART:20240115T090000Z"));
        assert!(!result.contains("SUMMARY"));
    }

    #[test]
    fn test_filter_valarm_kept_when_allowed() {
        let ical = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:1\r\nDTSTAMP:20240101T000000Z\r\nDTSTART:20240115T090000Z\r\nSUMMARY:Meeting\r\nBEGIN:VALARM\r\nACTION:DISPLAY\r\nDESCRIPTION:Reminder\r\nTRIGGER:-PT15M\r\nEND:VALARM\r\nEND:VEVENT\r\nEND:VCALENDAR";

        let allowed: HashSet<String> = ["DTSTART", "VALARM"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let result = filter_vevent_properties(ical, &allowed);

        assert!(result.contains("DTSTART:"));
        assert!(result.contains("BEGIN:VALARM"));
        assert!(result.contains("TRIGGER:-PT15M"));
        assert!(result.contains("END:VALARM"));
        assert!(!result.contains("SUMMARY"));
    }

    #[test]
    fn test_filter_valarm_stripped_when_not_allowed() {
        let ical = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:1\r\nDTSTAMP:20240101T000000Z\r\nDTSTART:20240115T090000Z\r\nBEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT15M\r\nEND:VALARM\r\nEND:VEVENT\r\nEND:VCALENDAR";

        let allowed: HashSet<String> = ["DTSTART"].iter().map(|s| s.to_string()).collect();
        let result = filter_vevent_properties(ical, &allowed);

        assert!(result.contains("DTSTART:"));
        assert!(!result.contains("VALARM"));
        assert!(!result.contains("TRIGGER"));
    }

    #[test]
    fn test_filter_keeps_recurrence_id_of_overridden_occurrences() {
        let ical = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n\
            BEGIN:VEVENT\r\nUID:s1\r\nDTSTAMP:20240101T000000Z\r\nDTSTART:20240115T100000Z\r\nRRULE:FREQ=WEEKLY\r\nEND:VEVENT\r\n\
            BEGIN:VEVENT\r\nUID:s1\r\nDTSTAMP:20240101T000000Z\r\nRECURRENCE-ID:20240122T100000Z\r\nDTSTART:20240123T100000Z\r\nEND:VEVENT\r\n\
            END:VCALENDAR";
        let allowed: HashSet<String> = ["DTSTART"].iter().map(|s| s.to_string()).collect();
        let result = filter_vevent_properties(ical, &allowed);
        assert!(result.contains("RECURRENCE-ID:20240122T100000Z"));
        assert!(result.contains("RRULE:FREQ=WEEKLY"));
    }

    #[test]
    fn test_filter_always_keeps_time_recurrence_and_availability() {
        let ical = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:1\r\nDTSTAMP:20240101T000000Z\r\n\
            DTSTART:20240115T090000Z\r\nDURATION:PT1H\r\nRRULE:FREQ=DAILY\r\nRDATE:20240301T090000Z\r\n\
            EXDATE:20240116T090000Z\r\nSTATUS:TENTATIVE\r\nTRANSP:TRANSPARENT\r\nSUMMARY:Secret\r\n\
            END:VEVENT\r\nEND:VCALENDAR";
        let result = filter_vevent_properties(ical, &HashSet::new());
        for kept in [
            "DURATION:PT1H",
            "RRULE:FREQ=DAILY",
            "RDATE:20240301T090000Z",
            "EXDATE:20240116T090000Z",
            "STATUS:TENTATIVE",
            "TRANSP:TRANSPARENT",
        ] {
            assert!(result.contains(kept), "{kept}: {result}");
        }
        assert!(!result.contains("Secret"));
    }
}
