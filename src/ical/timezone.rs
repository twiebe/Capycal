//! VTIMEZONE definitions: UTC offsets of local times in a zone the event
//! itself defines, for TZIDs no time zone database knows and for shifting
//! local times across a daylight saving switch.

use std::collections::HashMap;

use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime, Weekday};

/// One STANDARD or DAYLIGHT part of a VTIMEZONE.
pub struct Observance {
    /// Local start, in the offset before it.
    start: NaiveDateTime,
    offset_from: i64,
    offset_to: i64,
    /// Yearly onset as month and BYDAY (week number, weekday), if it recurs.
    yearly: Option<(u32, i64, Weekday)>,
}

impl Observance {
    /// When it starts in `year`, as local time.
    fn onset(&self, year: i32) -> Option<NaiveDateTime> {
        if year < self.start.year() {
            return None;
        }
        let Some((month, week, weekday)) = self.yearly else {
            return (year == self.start.year()).then_some(self.start);
        };
        let day = nth_weekday(year, month, week, weekday)?;
        Some(day.and_time(self.start.time()))
    }
}

fn nth_weekday(year: i32, month: u32, week: i64, weekday: Weekday) -> Option<NaiveDate> {
    if week > 0 {
        NaiveDate::from_weekday_of_month_opt(year, month, weekday, week as u8)
    } else {
        // Count back from the last day of the month
        let next = if month == 12 {
            NaiveDate::from_ymd_opt(year + 1, 1, 1)?
        } else {
            NaiveDate::from_ymd_opt(year, month + 1, 1)?
        };
        let last = next - Duration::days(1);
        let back = (last.weekday().num_days_from_monday() + 7 - weekday.num_days_from_monday()) % 7;
        Some(last - Duration::days(back as i64 + 7 * (-week - 1)))
    }
}

/// A VTIMEZONE: its STANDARD and DAYLIGHT observances.
pub struct Zone(Vec<Observance>);

impl Zone {
    /// UTC offset in seconds of the local time `local`.
    pub fn utc_offset(&self, local: NaiveDateTime) -> Option<i64> {
        let latest = self
            .0
            .iter()
            .flat_map(|o| {
                [local.year() - 1, local.year()]
                    .into_iter()
                    .filter_map(|year| o.onset(year))
                    .filter(|onset| *onset <= local)
                    .map(move |onset| (onset, o.offset_to))
            })
            .max_by_key(|(onset, _)| *onset);
        match latest {
            Some((_, offset)) => Some(offset),
            None => self.before_all(),
        }
    }

    /// The UTC instant of the local time `local`.
    pub fn to_utc(&self, local: NaiveDateTime) -> Option<NaiveDateTime> {
        Some(local - Duration::seconds(self.utc_offset(local)?))
    }

    /// The local time of the UTC instant `utc`.
    pub fn to_local(&self, utc: NaiveDateTime) -> Option<NaiveDateTime> {
        let latest = self
            .0
            .iter()
            .flat_map(|o| {
                [utc.year() - 1, utc.year()]
                    .into_iter()
                    .filter_map(|year| o.onset(year))
                    // Onsets are local times in the offset before them
                    .map(|onset| onset - Duration::seconds(o.offset_from))
                    .filter(|onset| *onset <= utc)
                    .map(move |onset| (onset, o.offset_to))
            })
            .max_by_key(|(onset, _)| *onset);
        let offset = match latest {
            Some((_, offset)) => offset,
            None => self.before_all()?,
        };
        Some(utc + Duration::seconds(offset))
    }

    /// The offset before the first observance.
    fn before_all(&self) -> Option<i64> {
        self.0.iter().min_by_key(|o| o.start).map(|o| o.offset_from)
    }
}

fn parse_offset(value: &str) -> Option<i64> {
    let (sign, digits) = match value.split_at_checked(1)? {
        ("+", d) => (1, d),
        ("-", d) => (-1, d),
        _ => return None,
    };
    let hours: i64 = digits.get(0..2)?.parse().ok()?;
    let minutes: i64 = digits.get(2..4)?.parse().ok()?;
    let seconds: i64 = digits.get(4..6).and_then(|s| s.parse().ok()).unwrap_or(0);
    Some(sign * (hours * 3600 + minutes * 60 + seconds))
}

fn parse_yearly(rrule: &str) -> Option<(u32, i64, Weekday)> {
    let mut month = None;
    let mut byday = None;
    for part in rrule.split(';') {
        match part.split_once('=')? {
            ("FREQ", freq) if freq != "YEARLY" => return None,
            ("BYMONTH", m) => month = m.parse().ok(),
            ("BYDAY", d) => byday = Some(d),
            _ => {}
        }
    }
    let byday = byday?;
    let (week, day) = byday.split_at(byday.len().checked_sub(2)?);
    let weekday = match day {
        "MO" => Weekday::Mon,
        "TU" => Weekday::Tue,
        "WE" => Weekday::Wed,
        "TH" => Weekday::Thu,
        "FR" => Weekday::Fri,
        "SA" => Weekday::Sat,
        "SU" => Weekday::Sun,
        _ => return None,
    };
    let week = if week.is_empty() {
        1
    } else {
        week.parse().ok()?
    };
    Some((month?, week, weekday))
}

pub fn parse_local(value: &str) -> Option<NaiveDateTime> {
    NaiveDateTime::parse_from_str(value.get(..15)?, "%Y%m%dT%H%M%S").ok()
}

/// An observance while its properties are read.
#[derive(Default)]
struct Draft {
    start: Option<NaiveDateTime>,
    offset_from: Option<i64>,
    offset_to: Option<i64>,
    yearly: Option<(u32, i64, Weekday)>,
}

/// Every VTIMEZONE in `unfolded`, by TZID.
pub fn vtimezones(unfolded: &str) -> HashMap<String, Zone> {
    let mut zones: HashMap<String, Zone> = HashMap::new();
    let mut tzid = None;
    let mut draft: Option<Draft> = None;
    for line in unfolded.lines().map(str::trim_end) {
        let (name, value) = line.split_once(':').unwrap_or((line, ""));
        match (name, value, &mut draft) {
            ("BEGIN", "VTIMEZONE", _) => tzid = None,
            ("TZID", id, None) => tzid = Some(id.to_string()),
            ("BEGIN", "STANDARD" | "DAYLIGHT", _) => draft = Some(Draft::default()),
            ("END", "STANDARD" | "DAYLIGHT", _) => {
                if let (Some(id), Some(d)) = (&tzid, draft.take())
                    && let (Some(start), Some(offset_from), Some(offset_to)) =
                        (d.start, d.offset_from, d.offset_to)
                {
                    zones
                        .entry(id.clone())
                        .or_insert_with(|| Zone(Vec::new()))
                        .0
                        .push(Observance {
                            start,
                            offset_from,
                            offset_to,
                            yearly: d.yearly,
                        });
                }
            }
            ("DTSTART", _, Some(d)) => d.start = parse_local(value),
            ("TZOFFSETFROM", _, Some(d)) => d.offset_from = parse_offset(value),
            ("TZOFFSETTO", _, Some(d)) => d.offset_to = parse_offset(value),
            ("RRULE", _, Some(d)) => d.yearly = parse_yearly(value),
            _ => {}
        }
    }
    zones
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
    fn test_offsets_around_the_switch() {
        let zones = vtimezones(EXCHANGE);
        let zone = &zones["Customized Time Zone"];
        let at = |s: &str| zone.utc_offset(parse_local(s).unwrap()).unwrap();
        // 2024: summer time from 31 March 02:00 to 27 October 03:00
        assert_eq!(at("20240331T015959"), 3600);
        assert_eq!(at("20240331T030000"), 7200);
        assert_eq!(at("20241027T025959"), 7200);
        assert_eq!(at("20241027T030000"), 3600);
        assert_eq!(at("20241231T120000"), 3600);
    }

    #[test]
    fn test_nth_weekday() {
        let day = |y, m, w, d| nth_weekday(y, m, w, d).unwrap().to_string();
        assert_eq!(day(2024, 3, -1, Weekday::Sun), "2024-03-31");
        assert_eq!(day(2024, 10, -1, Weekday::Sun), "2024-10-27");
        assert_eq!(day(2024, 3, 2, Weekday::Sun), "2024-03-10");
        assert_eq!(day(2024, 11, 1, Weekday::Sun), "2024-11-03");
    }

    #[test]
    fn test_to_local_and_back() {
        let zones = vtimezones(EXCHANGE);
        let zone = &zones["Customized Time Zone"];
        let local = |s: &str| parse_local(s).unwrap();
        // 31 March 2024: 01:00 UTC is 03:00 summer time, 00:59:59 UTC 01:59:59
        assert_eq!(
            zone.to_local(local("20240331T010000")),
            Some(local("20240331T030000"))
        );
        assert_eq!(
            zone.to_local(local("20240331T005959")),
            Some(local("20240331T015959"))
        );
        assert_eq!(
            zone.to_utc(local("20240715T090000")),
            Some(local("20240715T070000"))
        );
    }
}
