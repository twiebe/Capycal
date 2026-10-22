//! TZIDs Calendar.app may not know. NSTimeZone only knows IANA names, but
//! sources also send Windows names ("W. Europe Standard Time" from Outlook
//! and Exchange) and prefixed IANA names ("/freeassociation.sourceforge.net/
//! Europe/Berlin" from libical). `resolve_timezones` rewrites them before
//! the iCal is parsed for the sink.

use crate::ical::extract_tzid;
use crate::ical::timezone::{Zone, parse_local, vtimezones};

const IANA_AREAS: [&str; 11] = [
    "Africa",
    "America",
    "Antarctica",
    "Arctic",
    "Asia",
    "Atlantic",
    "Australia",
    "Europe",
    "Indian",
    "Pacific",
    "Etc",
];

/// Common Windows time zones and their IANA zone (CLDR windowsZones,
/// territory 001). Others are resolved from the event's VTIMEZONE.
const WINDOWS_ZONES: [(&str, &str); 35] = [
    ("W. Europe Standard Time", "Europe/Berlin"),
    ("Central Europe Standard Time", "Europe/Budapest"),
    ("Romance Standard Time", "Europe/Paris"),
    ("Central European Standard Time", "Europe/Warsaw"),
    ("GMT Standard Time", "Europe/London"),
    ("Greenwich Standard Time", "Atlantic/Reykjavik"),
    ("E. Europe Standard Time", "Europe/Chisinau"),
    ("FLE Standard Time", "Europe/Kiev"),
    ("GTB Standard Time", "Europe/Bucharest"),
    ("Russian Standard Time", "Europe/Moscow"),
    ("Turkey Standard Time", "Europe/Istanbul"),
    ("Eastern Standard Time", "America/New_York"),
    ("Central Standard Time", "America/Chicago"),
    ("Mountain Standard Time", "America/Denver"),
    ("US Mountain Standard Time", "America/Phoenix"),
    ("Pacific Standard Time", "America/Los_Angeles"),
    ("Alaskan Standard Time", "America/Anchorage"),
    ("Hawaiian Standard Time", "Pacific/Honolulu"),
    ("Atlantic Standard Time", "America/Halifax"),
    ("Canada Central Standard Time", "America/Regina"),
    ("E. South America Standard Time", "America/Sao_Paulo"),
    ("Argentina Standard Time", "America/Buenos_Aires"),
    ("India Standard Time", "Asia/Calcutta"),
    ("China Standard Time", "Asia/Shanghai"),
    ("Tokyo Standard Time", "Asia/Tokyo"),
    ("Korea Standard Time", "Asia/Seoul"),
    ("Singapore Standard Time", "Asia/Singapore"),
    ("Arabian Standard Time", "Asia/Dubai"),
    ("Israel Standard Time", "Asia/Jerusalem"),
    ("South Africa Standard Time", "Africa/Johannesburg"),
    ("AUS Eastern Standard Time", "Australia/Sydney"),
    ("E. Australia Standard Time", "Australia/Brisbane"),
    ("W. Australia Standard Time", "Australia/Perth"),
    ("New Zealand Standard Time", "Pacific/Auckland"),
    ("UTC", "Etc/UTC"),
];

/// The IANA name for `tzid`, if it is one, has one behind a prefix or is a
/// known Windows name.
fn iana_name(tzid: &str) -> Option<&str> {
    let is_iana = |name: &str| {
        name == "UTC"
            || name == "GMT"
            || name
                .split_once('/')
                .is_some_and(|(area, rest)| IANA_AREAS.contains(&area) && !rest.is_empty())
    };
    if let Some((_, iana)) = WINDOWS_ZONES.iter().find(|(windows, _)| *windows == tzid) {
        return Some(iana);
    }
    if is_iana(tzid) {
        return Some(tzid);
    }
    tzid.match_indices('/')
        .map(|(i, _)| &tzid[i + 1..])
        .find(|suffix| is_iana(suffix))
}

/// Rewrite the TZIDs of date-time properties outside VTIMEZONE blocks so
/// Calendar.app can resolve them: a prefixed IANA name or a known Windows
/// name becomes the IANA name; another name defined by a VTIMEZONE becomes
/// UTC values computed from it. Anything else is left as is.
pub fn resolve_timezones(unfolded: &str) -> String {
    let zones = vtimezones(unfolded);
    let mut in_vtimezone = false;
    let mut lines = Vec::new();
    for line in unfolded.lines().map(str::trim_end) {
        match line {
            "BEGIN:VTIMEZONE" => in_vtimezone = true,
            "END:VTIMEZONE" => in_vtimezone = false,
            _ => {}
        }
        let tzid = if in_vtimezone {
            None
        } else {
            extract_tzid(line)
        };
        let Some(tzid) = tzid else {
            lines.push(line.to_string());
            continue;
        };
        if let Some(iana) = iana_name(tzid) {
            lines.push(
                line.replacen(tzid, iana, 1)
                    .replace(&format!("\"{iana}\""), iana),
            );
        } else if let Some(utc) = zones.get(tzid).and_then(|z| to_utc(line, z)) {
            lines.push(utc);
        } else {
            lines.push(line.to_string());
        }
    }
    lines.join("\r\n")
}

/// `line` with its values converted to UTC and without the TZID parameter.
fn to_utc(line: &str, zone: &Zone) -> Option<String> {
    let colon = line.find(':')?;
    let (head, values) = (&line[..colon], &line[colon + 1..]);
    let mut params = head.split(';');
    let name = params.next()?;
    let params: String = params
        .filter(|p| !p.starts_with("TZID="))
        .map(|p| format!(";{p}"))
        .collect();
    let values = values
        .split(',')
        .map(|value| {
            let local = parse_local(value)?;
            let utc = zone.to_utc(local)?;
            Some(utc.format("%Y%m%dT%H%M%SZ").to_string())
        })
        .collect::<Option<Vec<_>>>()?;
    Some(format!("{name}{params}:{}", values.join(",")))
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXCHANGE: &str = "BEGIN:VCALENDAR\r\n\
        BEGIN:VTIMEZONE\r\nTZID:Customized Time Zone\r\n\
        BEGIN:STANDARD\r\nDTSTART:16010101T030000\r\nTZOFFSETFROM:+0200\r\nTZOFFSETTO:+0100\r\n\
        RRULE:FREQ=YEARLY;INTERVAL=1;BYDAY=-1SU;BYMONTH=10\r\nEND:STANDARD\r\n\
        BEGIN:DAYLIGHT\r\nDTSTART:16010101T020000\r\nTZOFFSETFROM:+0100\r\nTZOFFSETTO:+0200\r\n\
        RRULE:FREQ=YEARLY;INTERVAL=1;BYDAY=-1SU;BYMONTH=3\r\nEND:DAYLIGHT\r\n\
        END:VTIMEZONE\r\n\
        BEGIN:VEVENT\r\nUID:a\r\n\
        DTSTART;TZID=Customized Time Zone:20240115T090000\r\n\
        DTEND;TZID=\"Customized Time Zone\":20240715T090000\r\n\
        EXDATE;TZID=Customized Time Zone:20240122T090000,20240129T090000\r\n\
        END:VEVENT\r\nEND:VCALENDAR";

    #[test]
    fn test_unknown_tzid_is_converted_to_utc_from_its_vtimezone() {
        let resolved = resolve_timezones(EXCHANGE);
        // Winter: +01:00, summer: +02:00
        assert!(
            resolved.contains("\r\nDTSTART:20240115T080000Z\r\n"),
            "{resolved}"
        );
        assert!(
            resolved.contains("\r\nDTEND:20240715T070000Z\r\n"),
            "{resolved}"
        );
        assert!(resolved.contains("\r\nEXDATE:20240122T080000Z,20240129T080000Z\r\n"));
        // The VTIMEZONE itself is untouched
        assert!(resolved.contains("TZID:Customized Time Zone"));
    }

    #[test]
    fn test_windows_and_prefixed_names_become_iana() {
        let line = |tzid: &str| format!("DTSTART;TZID={tzid}:20240115T090000");
        let resolved = resolve_timezones(
            &[
                line("W. Europe Standard Time"),
                line("\"Pacific Standard Time\""),
                line("/freeassociation.sourceforge.net/Tzfile/Europe/Berlin"),
                line("/mozilla.org/20050126_1/America/Argentina/Buenos_Aires"),
                line("Europe/Berlin"),
                line("Unknown Zone"),
            ]
            .join("\r\n"),
        );
        assert_eq!(
            resolved.lines().collect::<Vec<_>>(),
            [
                "DTSTART;TZID=Europe/Berlin:20240115T090000",
                "DTSTART;TZID=America/Los_Angeles:20240115T090000",
                "DTSTART;TZID=Europe/Berlin:20240115T090000",
                "DTSTART;TZID=America/Argentina/Buenos_Aires:20240115T090000",
                "DTSTART;TZID=Europe/Berlin:20240115T090000",
                "DTSTART;TZID=Unknown Zone:20240115T090000",
            ]
        );
    }
}
