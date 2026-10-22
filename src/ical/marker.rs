pub const MARKER_SOURCE: &str = "X-CAPYCAL-SOURCE";
pub const MARKER_SYNC: &str = "X-CAPYCAL-SYNC";

/// Check if an iCal string contains the X-CAPYCAL-SOURCE marker,
/// indicating it was created by capycal and should not be synced again.
pub fn is_synced_event(ical_data: &str) -> bool {
    let unfolded = super::unfold(ical_data);
    unfolded.lines().any(|line| line.starts_with(MARKER_SOURCE))
}

/// Extract the source info from a synced event's X-CAPYCAL-SOURCE property.
/// Returns (source_calendar_name, original_uid) if present.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn extract_source_info(ical_data: &str) -> Option<(String, String)> {
    let unfolded = super::unfold(ical_data);
    for line in unfolded.lines() {
        if let Some(value) = line.strip_prefix(MARKER_SOURCE) {
            let value = value.strip_prefix(':').unwrap_or(value).trim();
            if let Some(slash_pos) = value.find('/') {
                let source = value[..slash_pos].to_string();
                let uid = value[slash_pos + 1..].to_string();
                return Some((source, uid));
            }
        }
    }
    None
}

/// The source UID of a synced event, from its X-CAPYCAL-SOURCE value
/// `<source calendar>/<uid>`. The calendar name is matched as a whole when
/// it is `source_name`, so a UID with slashes stays intact.
pub fn source_uid(ical_data: &str, source_name: &str) -> Option<String> {
    let unfolded = super::unfold(ical_data);
    let value = unfolded
        .lines()
        .find_map(|l| l.strip_prefix(MARKER_SOURCE)?.strip_prefix(':'))?
        .trim();
    value
        .strip_prefix(source_name)
        .and_then(|rest| rest.strip_prefix('/'))
        .or_else(|| value.split_once('/').map(|(_, uid)| uid))
        .filter(|uid| !uid.is_empty())
        .map(str::to_string)
}

/// Whether the event carries the sync marker of exactly this sync.
pub fn has_sync_marker(ical_data: &str, sync_name: &str) -> bool {
    let marker = format!("{MARKER_SYNC}:{sync_name}");
    crate::ical::unfold(ical_data)
        .lines()
        .any(|l| l.trim_end() == marker)
}

/// Generate the marker lines to insert into a VEVENT.
pub fn marker_lines(source_calendar: &str, source_uid: &str, sync_name: &str) -> String {
    format!("{MARKER_SOURCE}:{source_calendar}/{source_uid}\r\n{MARKER_SYNC}:{sync_name}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_has_sync_marker_matches_exact_name() {
        let ical = "BEGIN:VEVENT\r\nX-CAPYCAL-SYNC:work-to-home-old\r\nEND:VEVENT";
        assert!(has_sync_marker(ical, "work-to-home-old"));
        assert!(!has_sync_marker(ical, "work-to-home"));
        let folded = "BEGIN:VEVENT\r\nX-CAPYCAL-SYNC:work-to\r\n -home\r\nEND:VEVENT";
        assert!(has_sync_marker(folded, "work-to-home"));
    }

    #[test]
    fn test_is_synced_event() {
        let ical = "BEGIN:VEVENT\r\nSUMMARY:Test\r\nX-CAPYCAL-SOURCE:work/uid-123\r\nEND:VEVENT";
        assert!(is_synced_event(ical));

        let ical = "BEGIN:VEVENT\r\nSUMMARY:Test\r\nEND:VEVENT";
        assert!(!is_synced_event(ical));
    }

    #[test]
    fn test_extract_source_info() {
        let ical = "BEGIN:VEVENT\r\nX-CAPYCAL-SOURCE:work/uid-123\r\nEND:VEVENT";
        let (source, uid) = extract_source_info(ical).unwrap();
        assert_eq!(source, "work");
        assert_eq!(uid, "uid-123");
    }

    #[test]
    fn test_source_uid() {
        let ical = "BEGIN:VEVENT\r\nX-CAPYCAL-SOURCE:my/cal/uid/1\r\nEND:VEVENT";
        assert_eq!(source_uid(ical, "my/cal").as_deref(), Some("uid/1"));
        // A renamed source calendar still yields the UID after the first slash
        assert_eq!(source_uid(ical, "work").as_deref(), Some("cal/uid/1"));
        assert_eq!(source_uid("X-CAPYCAL-SOURCE:work/", "work"), None);
        assert_eq!(source_uid("X-CAPYCAL-SYNC:s", "work"), None);
    }

    #[test]
    fn test_marker_lines() {
        let lines = marker_lines("work", "uid-123", "work-to-personal");
        assert!(lines.contains("X-CAPYCAL-SOURCE:work/uid-123"));
        assert!(lines.contains("X-CAPYCAL-SYNC:work-to-personal"));
    }
}
