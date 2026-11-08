//! JSCalendar (RFC 8984, as JMAP Calendars uses it) to iCal and back. The sync
//! pipeline works on iCal, so the source side turns each event into iCal and
//! the sink side turns what the pipeline wrote into a JSCalendar event.

use chrono::{NaiveDate, NaiveDateTime, TimeDelta, TimeZone, Utc};
use chrono_tz::Tz;
use serde_json::{Map, Value, json};

use crate::error::{Error, Result};
use crate::ical::marker::{MARKER_SOURCE, MARKER_SYNC};
use crate::ical::tzid::resolve_timezones;
use crate::ical::{self, escape_text, unescape_text, unfold, valarm_lines, value_colon};

/// The markers of a copy, as vendor properties (RFC 8984 section 1.4).
const VENDOR_SOURCE: &str = "capycal.app:source";
const VENDOR_SYNC: &str = "capycal.app:sync";

/// The properties a put sets; one missing from the iCal is cleared.
const MANAGED: [&str; 20] = [
    "title",
    "description",
    "start",
    "timeZone",
    "showWithoutTime",
    "duration",
    "status",
    "freeBusyStatus",
    "locations",
    "links",
    "virtualLocations",
    "keywords",
    "priority",
    "privacy",
    "color",
    "participants",
    "organizerCalendarAddress",
    "alerts",
    "recurrenceRule",
    "recurrenceOverrides",
];

/// Maps whose entries a server must not add to or drop, unlike the fields of
/// the entries themselves.
const MAPS: [&str; 7] = [
    "locations",
    "links",
    "virtualLocations",
    "keywords",
    "participants",
    "alerts",
    "recurrenceOverrides",
];

/// Values a server may leave out or fill in.
const DEFAULTS: [(&str, &str); 3] = [
    ("status", "confirmed"),
    ("freeBusyStatus", "busy"),
    ("privacy", "public"),
];

const LOCAL: &str = "%Y-%m-%dT%H:%M:%S";
const ICAL_LOCAL: &str = "%Y%m%dT%H%M%S";

/// A date-time with its zone: None is floating, `Etc/UTC` is written with Z.
#[derive(Clone)]
struct Moment {
    local: NaiveDateTime,
    zone: Option<String>,
    date: bool,
}

fn tz(zone: &str) -> Option<Tz> {
    zone.parse().ok()
}

fn to_utc(local: NaiveDateTime, zone: &str) -> Option<NaiveDateTime> {
    let found = tz(zone)?.from_local_datetime(&local);
    Some(found.earliest()?.with_timezone(&Utc).naive_utc())
}

fn to_local(utc: NaiveDateTime, zone: &str) -> Option<NaiveDateTime> {
    Some(
        Utc.from_utc_datetime(&utc)
            .with_timezone(&tz(zone)?)
            .naive_local(),
    )
}

impl Moment {
    /// An iCal value with its TZID. A zone that is no IANA name is floating:
    /// `resolve_timezones` already replaced the ones it can.
    fn from_ical(value: &str, tzid: Option<&str>, date: bool) -> Option<Self> {
        if date || value.len() == 8 {
            let day = NaiveDate::parse_from_str(value, "%Y%m%d").ok()?;
            return Some(Self {
                local: day.and_hms_opt(0, 0, 0)?,
                zone: None,
                date: true,
            });
        }
        let (text, utc) = match value.strip_suffix('Z') {
            Some(text) => (text, true),
            None => (value, false),
        };
        Some(Self {
            local: NaiveDateTime::parse_from_str(text, ICAL_LOCAL).ok()?,
            zone: if utc {
                Some("Etc/UTC".to_string())
            } else {
                tzid.filter(|z| tz(z).is_some()).map(str::to_string)
            },
            date: false,
        })
    }

    /// The local time in `zone`, as far as both zones are known.
    fn in_zone(&self, zone: Option<&str>) -> NaiveDateTime {
        match (self.zone.as_deref(), zone) {
            (Some(from), Some(to)) if from != to => to_utc(self.local, from)
                .and_then(|utc| to_local(utc, to))
                .unwrap_or(self.local),
            _ => self.local,
        }
    }

    /// `self` plus a duration: the days on the calendar, then the exact
    /// seconds, as RFC 5545 section 3.3.6 has it.
    fn plus(&self, days: i64, secs: i64) -> Self {
        let local = self.local + TimeDelta::days(days);
        let delta = TimeDelta::seconds(secs);
        let local = self
            .zone
            .as_deref()
            .and_then(|zone| to_local(to_utc(local, zone)? + delta, zone))
            .unwrap_or(local + delta);
        Self {
            local,
            zone: self.zone.clone(),
            date: self.date,
        }
    }

    fn line(&self, name: &str) -> String {
        if self.date {
            return format!("{name};VALUE=DATE:{}", self.local.format("%Y%m%d"));
        }
        let value = self.local.format(ICAL_LOCAL);
        match self.zone.as_deref() {
            Some("Etc/UTC" | "UTC" | "Etc/Zulu") => format!("{name}:{value}Z"),
            Some(zone) => format!("{name};TZID={zone}:{value}"),
            None => format!("{name}:{value}"),
        }
    }
}

struct Prop<'a> {
    name: &'a str,
    params: Vec<(&'a str, &'a str)>,
    value: &'a str,
}

impl<'a> Prop<'a> {
    fn parse(line: &'a str) -> Option<Self> {
        let colon = value_colon(line)?;
        let head = &line[..colon];
        let mut parts = Vec::new();
        let (mut begin, mut quoted) = (0, false);
        for (i, c) in head.char_indices() {
            match c {
                '"' => quoted = !quoted,
                ';' if !quoted => {
                    parts.push(&head[begin..i]);
                    begin = i + 1;
                }
                _ => {}
            }
        }
        parts.push(&head[begin..]);
        let params = parts[1..]
            .iter()
            .filter_map(|p| p.split_once('='))
            .map(|(k, v)| (k, v.trim_matches('"')))
            .collect();
        Some(Self {
            name: parts[0],
            params,
            value: line[colon + 1..].trim_end(),
        })
    }

    fn param(&self, name: &str) -> Option<&'a str> {
        self.params
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| *v)
    }

    fn moment(&self) -> Option<Moment> {
        self.moment_of(self.value)
    }

    fn moment_of(&self, value: &str) -> Option<Moment> {
        let date = self.param("VALUE") == Some("DATE");
        Moment::from_ical(value, self.param("TZID"), date)
    }

    fn text(&self) -> String {
        unescape_text(self.value)
    }
}

#[derive(Default)]
struct Vevent<'a> {
    props: Vec<Prop<'a>>,
    alarms: Vec<Vec<Prop<'a>>>,
}

impl<'a> Vevent<'a> {
    fn prop(&self, name: &str) -> Option<&Prop<'a>> {
        self.props.iter().find(|p| p.name == name)
    }
}

fn vevents(text: &str) -> Vec<Vevent<'_>> {
    let mut events = Vec::new();
    let mut event: Option<Vevent> = None;
    let mut alarm: Option<Vec<Prop>> = None;
    for line in text.lines().map(str::trim_end) {
        match line {
            "BEGIN:VEVENT" => event = Some(Vevent::default()),
            "END:VEVENT" => events.extend(event.take()),
            "BEGIN:VALARM" => alarm = Some(Vec::new()),
            "END:VALARM" => {
                if let (Some(event), Some(alarm)) = (&mut event, alarm.take()) {
                    event.alarms.push(alarm);
                }
            }
            line => {
                let Some(prop) = Prop::parse(line) else {
                    continue;
                };
                if let Some(alarm) = &mut alarm {
                    alarm.push(prop);
                } else if let Some(event) = &mut event {
                    event.props.push(prop);
                }
            }
        }
    }
    events
}

/// A JSCalendar duration: "PT1H", "P1DT30M"; without a sign for a span of
/// time, with one for an alert's offset.
fn format_duration(secs: i64) -> String {
    let (d, rest) = (secs / 86400, secs % 86400);
    let (h, m, s) = (rest / 3600, rest % 3600 / 60, rest % 60);
    let mut text = "P".to_string();
    if d > 0 {
        text.push_str(&format!("{d}D"));
    }
    if rest > 0 || d == 0 {
        text.push('T');
        if h > 0 {
            text.push_str(&format!("{h}H"));
        }
        if m > 0 {
            text.push_str(&format!("{m}M"));
        }
        if s > 0 || rest == 0 {
            text.push_str(&format!("{s}S"));
        }
    }
    text
}

/// The calendar days and exact seconds of a duration, both negative for a
/// negative one.
fn parse_duration(text: &str) -> Option<(i64, i64)> {
    let (sign, rest) = match text.strip_prefix('-') {
        Some(rest) => (-1, rest),
        None => (1, text.strip_prefix('+').unwrap_or(text)),
    };
    let rest = rest.strip_prefix('P')?;
    let (mut days, mut secs, mut number) = (0, 0, String::new());
    for c in rest.chars() {
        match c {
            'T' => {}
            '0'..='9' => number.push(c),
            // A fraction of a second: only the whole seconds are worth keeping
            '.' => {
                secs += number.parse::<i64>().ok()?;
                return Some((sign * days, sign * secs));
            }
            unit => {
                let n: i64 = number.parse().ok()?;
                number.clear();
                match unit {
                    'W' => days += n * 7,
                    'D' => days += n,
                    'H' => secs += n * 3600,
                    'M' => secs += n * 60,
                    'S' => secs += n,
                    _ => return None,
                }
            }
        }
    }
    Some((sign * days, sign * secs))
}

fn numbered<T>(items: impl IntoIterator<Item = T>, f: impl Fn(T) -> Value) -> Option<Value> {
    let map: Map<String, Value> = items
        .into_iter()
        .enumerate()
        .map(|(i, item)| ((i + 1).to_string(), f(item)))
        .collect();
    (!map.is_empty()).then_some(Value::Object(map))
}

fn rrule_to_json(value: &str, start: &Moment) -> Value {
    let mut rule = Map::new();
    for part in value.split(';') {
        let Some((key, val)) = part.split_once('=') else {
            continue;
        };
        let numbers = || -> Value {
            val.split(',')
                .filter_map(|n| n.parse::<i64>().ok())
                .collect::<Vec<_>>()
                .into()
        };
        let (name, json) = match key {
            "FREQ" => ("frequency", json!(val.to_lowercase())),
            "INTERVAL" => ("interval", json!(val.parse::<i64>().unwrap_or(1))),
            "COUNT" => ("count", json!(val.parse::<i64>().unwrap_or(1))),
            "WKST" => ("firstDayOfWeek", json!(val.to_lowercase())),
            "RSCALE" => ("rscale", json!(val.to_lowercase())),
            "SKIP" => ("skip", json!(val.to_lowercase())),
            "BYMONTH" => ("byMonth", json!(val.split(',').collect::<Vec<_>>())),
            "BYMONTHDAY" => ("byMonthDay", numbers()),
            "BYYEARDAY" => ("byYearDay", numbers()),
            "BYWEEKNO" => ("byWeekNo", numbers()),
            "BYHOUR" => ("byHour", numbers()),
            "BYMINUTE" => ("byMinute", numbers()),
            "BYSECOND" => ("bySecond", numbers()),
            "BYSETPOS" => ("bySetPosition", numbers()),
            "BYDAY" => (
                "byDay",
                val.split(',')
                    .filter(|d| d.len() >= 2)
                    .map(|d| {
                        let (nth, day) = d.split_at(d.len() - 2);
                        let mut entry = json!({ "day": day.to_lowercase() });
                        if let Ok(nth) = nth.parse::<i64>() {
                            entry["nthOfPeriod"] = json!(nth);
                        }
                        entry
                    })
                    .collect::<Vec<_>>()
                    .into(),
            ),
            "UNTIL" => {
                let Some(until) = Moment::from_ical(val, None, false) else {
                    continue;
                };
                let local = if until.date && !start.date {
                    // The whole last day counts
                    until.local + TimeDelta::seconds(86399)
                } else {
                    until.in_zone(start.zone.as_deref())
                };
                ("until", json!(local.format(LOCAL).to_string()))
            }
            _ => continue,
        };
        rule.insert(name.to_string(), json);
    }
    Value::Object(rule)
}

fn rrule_to_ical(rule: &Value, start: &Moment) -> Option<String> {
    let text = |key: &str| rule.get(key).and_then(Value::as_str);
    let mut parts = vec![format!("FREQ={}", text("frequency")?.to_uppercase())];
    if let Some(until) = text("until").and_then(parse_local) {
        let utc = start.zone.as_deref().and_then(|z| to_utc(until, z));
        parts.push(match utc {
            _ if start.date => format!("UNTIL={}", until.format("%Y%m%d")),
            Some(utc) => format!("UNTIL={}Z", utc.format(ICAL_LOCAL)),
            None => format!("UNTIL={}", until.format(ICAL_LOCAL)),
        });
    }
    for (key, name) in [("count", "COUNT"), ("interval", "INTERVAL")] {
        if let Some(n) = rule.get(key).and_then(Value::as_i64) {
            parts.push(format!("{name}={n}"));
        }
    }
    if let Some(days) = rule.get("byDay").and_then(Value::as_array) {
        let days: Vec<String> = days
            .iter()
            .filter_map(|d| {
                let day = d.get("day")?.as_str()?.to_uppercase();
                Some(match d.get("nthOfPeriod").and_then(Value::as_i64) {
                    Some(nth) => format!("{nth}{day}"),
                    None => day,
                })
            })
            .collect();
        parts.push(format!("BYDAY={}", days.join(",")));
    }
    for (key, name) in [
        ("byMonth", "BYMONTH"),
        ("byMonthDay", "BYMONTHDAY"),
        ("byYearDay", "BYYEARDAY"),
        ("byWeekNo", "BYWEEKNO"),
        ("byHour", "BYHOUR"),
        ("byMinute", "BYMINUTE"),
        ("bySecond", "BYSECOND"),
        ("bySetPosition", "BYSETPOS"),
    ] {
        if let Some(values) = rule.get(key).and_then(Value::as_array) {
            let values: Vec<String> = values
                .iter()
                .map(|v| match v {
                    Value::String(s) => s.clone(),
                    v => v.to_string(),
                })
                .collect();
            parts.push(format!("{name}={}", values.join(",")));
        }
    }
    for (key, name) in [
        ("firstDayOfWeek", "WKST"),
        ("rscale", "RSCALE"),
        ("skip", "SKIP"),
    ] {
        if let Some(value) = text(key) {
            parts.push(format!("{name}={}", value.to_uppercase()));
        }
    }
    Some(format!("RRULE:{}", parts.join(";")))
}

fn alarm_to_json(alarm: &[Prop]) -> Option<Value> {
    let trigger = alarm.iter().find(|p| p.name == "TRIGGER")?;
    let action = match alarm.iter().find(|p| p.name == "ACTION") {
        Some(p) if p.value == "EMAIL" => "email",
        _ => "display",
    };
    let trigger = if trigger.param("VALUE") == Some("DATE-TIME") {
        let when = Moment::from_ical(trigger.value, None, false)?;
        json!({
            "@type": "AbsoluteTrigger",
            "when": format!("{}Z", when.local.format(LOCAL)),
        })
    } else {
        let mut offset = json!({ "@type": "OffsetTrigger", "offset": trigger.value });
        if trigger.param("RELATED") == Some("END") {
            offset["relativeTo"] = json!("end");
        }
        offset
    };
    Some(json!({ "@type": "Alert", "trigger": trigger, "action": action }))
}

fn alert_to_ical(alert: &Value, lines: &mut Vec<String>) {
    let trigger = &alert["trigger"];
    let line = if let Some(when) = trigger["when"].as_str() {
        let Some(when) = parse_local(when) else {
            return;
        };
        format!("TRIGGER;VALUE=DATE-TIME:{}Z", when.format(ICAL_LOCAL))
    } else if let Some(offset) = trigger["offset"].as_str() {
        match trigger["relativeTo"].as_str() {
            Some("end") => format!("TRIGGER;RELATED=END:{offset}"),
            _ => format!("TRIGGER:{offset}"),
        }
    } else {
        return;
    };
    lines.extend(valarm_lines(line));
}

fn participants_to_json(block: &Vevent, event: &mut Map<String, Value>) {
    let mut people: Vec<(String, Map<String, Value>)> = Vec::new();
    for prop in &block.props {
        if prop.name != "ORGANIZER" && prop.name != "ATTENDEE" {
            continue;
        }
        let at = match people.iter().position(|(a, _)| a == prop.value) {
            Some(at) => at,
            None => {
                let mut person = Map::new();
                person.insert("@type".to_string(), json!("Participant"));
                person.insert("calendarAddress".to_string(), json!(prop.value));
                person.insert("roles".to_string(), json!({}));
                people.push((prop.value.to_string(), person));
                people.len() - 1
            }
        };
        let person = &mut people[at].1;
        if let Some(name) = prop.param("CN") {
            person.insert("name".to_string(), json!(name));
        }
        let role = if prop.name == "ORGANIZER" {
            event.insert("organizerCalendarAddress".to_string(), json!(prop.value));
            "owner"
        } else {
            if let Some(status) = prop.param("PARTSTAT") {
                person.insert(
                    "participationStatus".to_string(),
                    json!(status.to_lowercase()),
                );
            }
            match prop.param("ROLE") {
                Some("OPT-PARTICIPANT") => "optional",
                Some("NON-PARTICIPANT") => "informational",
                Some("CHAIR") => "chair",
                Some("REQ-PARTICIPANT") => "required",
                _ => "attendee",
            }
        };
        person["roles"][role] = json!(true);
    }
    if let Some(value) = numbered(people, |(_, person)| Value::Object(person)) {
        event.insert("participants".to_string(), value);
    }
}

fn participants_to_ical(event: &Map<String, Value>, lines: &mut Vec<String>) {
    let Some(people) = event.get("participants").and_then(Value::as_object) else {
        return;
    };
    for person in people.values() {
        let address = person["calendarAddress"]
            .as_str()
            .map(str::to_string)
            .or_else(|| {
                let mail = person["sendTo"]["imip"].as_str()?;
                Some(mail.to_string())
            });
        let Some(address) = address else { continue };
        let cn = person["name"]
            .as_str()
            .map(|n| format!(";CN=\"{}\"", n.replace('"', "'")))
            .unwrap_or_default();
        let roles = &person["roles"];
        let has = |role: &str| roles[role].as_bool() == Some(true);
        if has("owner") {
            lines.push(format!("ORGANIZER{cn}:{address}"));
        }
        let only_owner = has("owner") && roles.as_object().is_some_and(|r| r.len() == 1);
        if only_owner {
            continue;
        }
        let role = if has("chair") {
            "CHAIR"
        } else if has("optional") {
            "OPT-PARTICIPANT"
        } else if has("informational") {
            "NON-PARTICIPANT"
        } else {
            "REQ-PARTICIPANT"
        };
        let status = person["participationStatus"]
            .as_str()
            .map(|s| format!(";PARTSTAT={}", s.to_uppercase()))
            .unwrap_or_default();
        lines.push(format!("ATTENDEE{cn};ROLE={role}{status}:{address}"));
    }
}

/// The properties one VEVENT sets, for the master and its overrides alike.
fn event_props(block: &Vevent) -> Result<Map<String, Value>> {
    let start = block
        .prop("DTSTART")
        .and_then(Prop::moment)
        .ok_or_else(|| Error::ICalParse("event without a valid DTSTART".to_string()))?;
    let mut event = Map::new();
    let mut set = |key: &str, value: Value| {
        event.insert(key.to_string(), value);
    };
    set("start", json!(start.local.format(LOCAL).to_string()));
    if start.date {
        set("showWithoutTime", json!(true));
    } else if let Some(zone) = &start.zone {
        set("timeZone", json!(zone));
    }

    let end = block.prop("DTEND").and_then(Prop::moment);
    let duration = match (&end, block.prop("DURATION")) {
        (Some(end), _) if start.date => {
            Some(format!("P{}D", (end.local - start.local).num_days().max(1)))
        }
        (Some(end), _) => {
            let secs = match (to_utc_of(&start), to_utc_of(end)) {
                (Some(from), Some(to)) => (to - from).num_seconds(),
                _ => (end.local - start.local).num_seconds(),
            };
            (secs > 0).then(|| format_duration(secs))
        }
        (None, Some(duration)) => Some(duration.value.to_string()),
        (None, None) => start.date.then(|| "P1D".to_string()),
    };
    if let Some(duration) = duration {
        set("duration", json!(duration));
    }

    let of = |name: &'static str| block.props.iter().filter(move |p| p.name == name);
    for (name, key) in [("SUMMARY", "title"), ("DESCRIPTION", "description")] {
        if let Some(prop) = block.prop(name) {
            set(key, json!(prop.text()));
        }
    }
    if let Some(prop) = block.prop("STATUS") {
        set("status", json!(prop.value.to_lowercase()));
    }
    if let Some(prop) = block.prop("TRANSP") {
        let free = prop.value == "TRANSPARENT";
        set("freeBusyStatus", json!(if free { "free" } else { "busy" }));
    }
    if let Some(prop) = block.prop("CLASS") {
        let privacy = match prop.value {
            "PRIVATE" => "private",
            "CONFIDENTIAL" => "secret",
            _ => "public",
        };
        set("privacy", json!(privacy));
    }
    if let Some(n) = block
        .prop("PRIORITY")
        .and_then(|p| p.value.parse::<i64>().ok())
    {
        set("priority", json!(n));
    }
    if let Some(prop) = block.prop("COLOR") {
        set("color", json!(prop.value));
    }
    if let Some(value) = numbered(
        of("LOCATION"),
        |p| json!({ "@type": "Location", "name": p.text() }),
    ) {
        set("locations", value);
    }
    if let Some(value) = numbered(
        of("URL"),
        |p| json!({ "@type": "Link", "href": p.value, "rel": "describedby" }),
    ) {
        set("links", value);
    }
    if let Some(value) = numbered(of("CONFERENCE"), |p| {
        let mut location = json!({ "@type": "VirtualLocation", "uri": p.value });
        if let Some(label) = p.param("LABEL") {
            location["name"] = json!(label);
        }
        location
    }) {
        set("virtualLocations", value);
    }
    let keywords: Map<String, Value> = of("CATEGORIES")
        .flat_map(|p| split_text_list(p.value))
        .map(|k| (k, json!(true)))
        .collect();
    if !keywords.is_empty() {
        set("keywords", Value::Object(keywords));
    }
    if let Some(value) = numbered(block.alarms.iter().filter_map(|a| alarm_to_json(a)), |a| a) {
        set("alerts", value);
    }
    if let Some(prop) = block.prop("RRULE") {
        set("recurrenceRule", rrule_to_json(prop.value, &start));
    }
    participants_to_json(block, &mut event);
    Ok(event)
}

fn to_utc_of(moment: &Moment) -> Option<NaiveDateTime> {
    to_utc(moment.local, moment.zone.as_deref()?)
}

/// The items of a comma-separated TEXT list, unescaped.
fn split_text_list(value: &str) -> Vec<String> {
    let (mut items, mut current, mut escaped) = (Vec::new(), String::new(), false);
    for c in value.chars() {
        match c {
            ',' if !escaped => items.push(std::mem::take(&mut current)),
            c => {
                current.push(c);
                escaped = c == '\\' && !escaped;
                continue;
            }
        }
        escaped = false;
    }
    items.push(current);
    items.iter().map(|i| unescape_text(i)).collect()
}

/// The JSCalendar event for what the pipeline wrote: its master VEVENT, with
/// EXDATE, RDATE and the other VEVENTs as `recurrenceOverrides`.
pub fn from_ical(ical: &str) -> Result<Map<String, Value>> {
    let text = resolve_timezones(&unfold(ical));
    let blocks = vevents(&text);
    let master_at = blocks
        .iter()
        .position(|b| b.prop("RECURRENCE-ID").is_none())
        .unwrap_or(0);
    let master = blocks
        .get(master_at)
        .ok_or_else(|| Error::ICalParse("no VEVENT".to_string()))?;
    let uid = master
        .prop("UID")
        .ok_or_else(|| Error::ICalParse("event without UID".to_string()))?;

    let mut event = event_props(master)?;
    let zone = event
        .get("timeZone")
        .and_then(Value::as_str)
        .map(str::to_string);
    let date = event.contains_key("showWithoutTime");
    let key = |prop: &Prop, value: &str| {
        let moment = prop.moment_of(value)?;
        // An all-day series names its occurrences by day
        let local = if date {
            moment.local.date().and_hms_opt(0, 0, 0)?
        } else {
            moment.in_zone(zone.as_deref())
        };
        Some(local.format(LOCAL).to_string())
    };

    let mut overrides = Map::new();
    for prop in &master.props {
        let patch = match prop.name {
            "EXDATE" => json!({ "excluded": true }),
            "RDATE" => json!({}),
            _ => continue,
        };
        for value in prop.value.split(',') {
            // A PERIOD of an RDATE counts by its start
            let value = value.split('/').next().unwrap_or(value);
            if let Some(key) = key(prop, value) {
                overrides.insert(key, patch.clone());
            }
        }
    }
    for (i, block) in blocks.iter().enumerate() {
        if i == master_at {
            continue;
        }
        let Some(key) = block.prop("RECURRENCE-ID").and_then(|p| key(p, p.value)) else {
            continue;
        };
        let full = event_props(block)?;
        let mut patch = Map::new();
        // Neither may be set per occurrence
        let per_occurrence =
            |n: &&&str| !["recurrenceRule", "organizerCalendarAddress"].contains(n);
        for name in MANAGED.iter().filter(per_occurrence) {
            let (new, old) = (full.get(*name), event.get(*name));
            if new != old {
                patch.insert(name.to_string(), new.cloned().unwrap_or(Value::Null));
            }
        }
        // Without a start an occurrence is where its key says
        if patch.get("start").and_then(Value::as_str) == Some(key.as_str()) {
            patch.remove("start");
        }
        overrides.insert(key, Value::Object(patch));
    }
    if !overrides.is_empty() {
        event.insert("recurrenceOverrides".to_string(), Value::Object(overrides));
    }

    event.insert("@type".to_string(), json!("Event"));
    event.insert("uid".to_string(), json!(uid.value));
    for (marker, vendor) in [(MARKER_SOURCE, VENDOR_SOURCE), (MARKER_SYNC, VENDOR_SYNC)] {
        if let Some(prop) = master.prop(marker) {
            event.insert(vendor.to_string(), json!(prop.value));
        }
    }
    Ok(event)
}

fn parse_local(text: &str) -> Option<NaiveDateTime> {
    let text = text.strip_suffix('Z').unwrap_or(text);
    let plain = text.split('.').next().unwrap_or(text);
    NaiveDateTime::parse_from_str(plain, LOCAL).ok()
}

fn start_of(event: &Map<String, Value>) -> Option<Moment> {
    let local = parse_local(event.get("start")?.as_str()?)?;
    let date = event.get("showWithoutTime").and_then(Value::as_bool) == Some(true);
    let zone = event
        .get("timeZone")
        .and_then(Value::as_str)
        .filter(|z| !date && tz(z).is_some())
        .map(str::to_string);
    Some(Moment { local, zone, date })
}

/// `event` with a JSCalendar patch applied: its keys may be paths into the
/// event, a null removes.
fn patched(event: &Map<String, Value>, patch: &Map<String, Value>) -> Map<String, Value> {
    let mut out = event.clone();
    for (path, value) in patch {
        let parts: Vec<&str> = path.split('/').collect();
        set_path(&mut out, &parts, value);
    }
    out
}

fn set_path(at: &mut Map<String, Value>, path: &[&str], value: &Value) {
    match path {
        [] => {}
        [last] if value.is_null() => {
            at.remove(*last);
        }
        [last] => {
            at.insert(last.to_string(), value.clone());
        }
        [first, rest @ ..] => {
            let next = at.entry(first.to_string()).or_insert_with(|| json!({}));
            if let Some(next) = next.as_object_mut() {
                set_path(next, rest, value);
            }
        }
    }
}

fn utc_stamp(text: &str) -> Option<String> {
    Some(format!("{}Z", parse_local(text)?.format(ICAL_LOCAL)))
}

/// The properties of a VEVENT, after those only the master has.
fn vevent_lines(event: &Map<String, Value>, uid: &str, head: Vec<String>) -> Result<Vec<String>> {
    let start =
        start_of(event).ok_or_else(|| Error::Jmap(format!("event {uid} without a valid start")))?;
    let text = |key: &str| event.get(key).and_then(Value::as_str);
    let mut lines = vec!["BEGIN:VEVENT".to_string(), format!("UID:{uid}")];
    // A fixed value for a server without `updated`: the etag hashes this
    let stamp = text("updated")
        .and_then(utc_stamp)
        .unwrap_or_else(|| "20000101T000000Z".to_string());
    lines.push(format!("DTSTAMP:{stamp}"));
    if let Some(created) = text("created").and_then(utc_stamp) {
        lines.push(format!("CREATED:{created}"));
    }
    lines.push(start.line("DTSTART"));
    let (days, secs) = text("duration").and_then(parse_duration).unwrap_or((0, 0));
    if days != 0 || secs != 0 {
        lines.push(start.plus(days, secs).line("DTEND"));
    } else if start.date {
        lines.push(start.plus(1, 0).line("DTEND"));
    }
    lines.extend(head);
    for (key, name) in [("title", "SUMMARY"), ("description", "DESCRIPTION")] {
        if let Some(value) = text(key) {
            lines.push(format!("{name}:{}", escape_text(value)));
        }
    }
    if let Some(status) = text("status") {
        lines.push(format!("STATUS:{}", status.to_uppercase()));
    }
    lines.push(
        if text("freeBusyStatus") == Some("free") {
            "TRANSP:TRANSPARENT"
        } else {
            "TRANSP:OPAQUE"
        }
        .to_string(),
    );
    match text("privacy") {
        Some("private") => lines.push("CLASS:PRIVATE".to_string()),
        Some("secret") => lines.push("CLASS:CONFIDENTIAL".to_string()),
        _ => {}
    }
    if let Some(n) = event
        .get("priority")
        .and_then(Value::as_i64)
        .filter(|n| *n > 0)
    {
        lines.push(format!("PRIORITY:{n}"));
    }
    if let Some(n) = event
        .get("sequence")
        .and_then(Value::as_i64)
        .filter(|n| *n > 0)
    {
        lines.push(format!("SEQUENCE:{n}"));
    }
    if let Some(color) = text("color") {
        lines.push(format!("COLOR:{color}"));
    }
    let values = |key: &str| event.get(key).and_then(Value::as_object);
    if let Some(name) = values("locations")
        .into_iter()
        .flat_map(|m| m.values())
        .find_map(|l| l["name"].as_str())
    {
        lines.push(format!("LOCATION:{}", escape_text(name)));
    }
    if let Some(link) = values("links")
        .into_iter()
        .flat_map(|m| m.values())
        .find(|l| l["rel"].as_str() != Some("enclosure"))
        .and_then(|l| l["href"].as_str())
    {
        lines.push(format!("URL:{link}"));
    }
    for location in values("virtualLocations")
        .into_iter()
        .flat_map(|m| m.values())
    {
        let Some(uri) = location["uri"].as_str() else {
            continue;
        };
        let label = location["name"]
            .as_str()
            .map(|n| format!(";LABEL=\"{}\"", n.replace('"', "'")))
            .unwrap_or_default();
        lines.push(format!("CONFERENCE;VALUE=URI{label}:{uri}"));
    }
    if let Some(keywords) = values("keywords").filter(|k| !k.is_empty()) {
        let list: Vec<String> = keywords.keys().map(|k| escape_text(k)).collect();
        lines.push(format!("CATEGORIES:{}", list.join(",")));
    }
    participants_to_ical(event, &mut lines);
    for (vendor, marker) in [(VENDOR_SOURCE, MARKER_SOURCE), (VENDOR_SYNC, MARKER_SYNC)] {
        if let Some(value) = text(vendor) {
            lines.push(format!("{marker}:{value}"));
        }
    }
    // What a server keeps of unknown iCal properties. Not escaped: the markers
    // of a copy written over CalDAV must stay as they are
    for property in event
        .get("iCalendar")
        .and_then(|i| i["properties"].as_array())
        .into_iter()
        .flatten()
    {
        if let (Some(name), Some(value)) = (property[0].as_str(), property[3].as_str()) {
            let name = name.to_uppercase();
            if !lines.iter().any(|l| ical::property_name(l) == name) {
                lines.push(format!("{name}:{value}"));
            }
        }
    }
    for alert in values("alerts").into_iter().flat_map(|m| m.values()) {
        alert_to_ical(alert, &mut lines);
    }
    lines.push("END:VEVENT".to_string());
    Ok(lines)
}

/// The iCal of a JSCalendar event: its master VEVENT with RRULE, EXDATE and
/// RDATE, and a VEVENT per changed occurrence.
pub fn to_ical(event: &Value) -> Result<String> {
    let event = event
        .as_object()
        .ok_or_else(|| Error::Jmap("event is no object".to_string()))?;
    let uid = event
        .get("uid")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Jmap("event without uid".to_string()))?;
    let start =
        start_of(event).ok_or_else(|| Error::Jmap(format!("event {uid} without a valid start")))?;

    let mut head = Vec::new();
    let rules = event
        .get("recurrenceRule")
        .into_iter()
        .chain(
            // RFC 8984 has a list, JMAP Calendars one rule
            event
                .get("recurrenceRules")
                .and_then(Value::as_array)
                .into_iter()
                .flatten(),
        )
        .filter(|r| r.is_object());
    head.extend(rules.filter_map(|r| rrule_to_ical(r, &start)));

    let mut changed = Vec::new();
    for (key, patch) in event
        .get("recurrenceOverrides")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        let (Some(local), Some(patch)) = (parse_local(key), patch.as_object()) else {
            continue;
        };
        let at = Moment {
            local,
            ..start.clone()
        };
        if patch.get("excluded").and_then(Value::as_bool) == Some(true) {
            head.push(at.line("EXDATE"));
        } else if patch.is_empty() {
            head.push(at.line("RDATE"));
        } else {
            // Without a start in the patch, the occurrence is where its key says
            let mut base = event.clone();
            base.insert("start".to_string(), json!(key));
            changed.push((at, patched(&base, patch)));
        }
    }

    let mut lines = vec![
        "BEGIN:VCALENDAR".to_string(),
        "VERSION:2.0".to_string(),
        format!("PRODID:{}", ical::PRODID),
    ];
    lines.extend(vevent_lines(event, uid, head)?);
    for (at, full) in changed {
        lines.extend(vevent_lines(&full, uid, vec![at.line("RECURRENCE-ID")])?);
    }
    lines.push("END:VCALENDAR".to_string());
    Ok(lines.join("\r\n") + "\r\n")
}

/// A sync's marker on a stored event: a vendor property, or the X- property
/// of a copy written over CalDAV to the same server.
pub fn marker(event: &Value, ical_name: &str) -> Option<String> {
    let vendor = match ical_name {
        MARKER_SOURCE => VENDOR_SOURCE,
        _ => VENDOR_SYNC,
    };
    if let Some(value) = event[vendor].as_str() {
        return Some(value.to_string());
    }
    event["iCalendar"]["properties"]
        .as_array()?
        .iter()
        .find(|p| {
            p[0].as_str()
                .is_some_and(|n| n.eq_ignore_ascii_case(ical_name))
        })?[3]
        .as_str()
        .map(str::to_string)
}

/// The patch that makes an event what `event` says: every property it sets,
/// and null for the managed ones it leaves out.
pub fn update_patch(event: &Map<String, Value>) -> Map<String, Value> {
    let mut patch = event.clone();
    patch.remove("@type");
    patch.remove("uid");
    for name in MANAGED {
        patch.entry(name).or_insert(Value::Null);
    }
    patch
}

fn duration_secs(value: &Value) -> Option<i64> {
    let (days, secs) = parse_duration(value.as_str()?)?;
    Some(days * 86400 + secs)
}

/// Whether the stored `have` holds all of `want`. The fields of an entry may
/// have more: servers add some, like the `rel` of a link.
fn holds(key: &str, have: Option<&Value>, want: Option<&Value>) -> bool {
    let (have, want) = (have.filter(|v| !v.is_null()), want.filter(|v| !v.is_null()));
    match (have, want) {
        (None, None) => true,
        (Some(have), Some(want)) => match (have, want) {
            (Value::Object(have), Value::Object(want)) => {
                if key == "recurrenceRule" {
                    return same_rule(have, want);
                }
                if MAPS.contains(&key) && have.len() != want.len() {
                    return false;
                }
                want.iter()
                    .filter(|(k, _)| *k != "@type")
                    .all(|(k, v)| holds(k, have.get(k), Some(v)))
            }
            _ if key == "duration" || key == "offset" => {
                duration_secs(have).is_some() && duration_secs(have) == duration_secs(want)
            }
            _ => have == want,
        },
        // In an override, a property the master has but the patch clears
        (None, Some(_)) | (Some(_), None) => false,
    }
}

fn same_rule(have: &Map<String, Value>, want: &Map<String, Value>) -> bool {
    let plain = |rule: &Map<String, Value>| {
        let mut rule = rule.clone();
        rule.remove("@type");
        if rule.get("interval") == Some(&json!(1)) {
            rule.remove("interval");
        }
        if rule.get("firstDayOfWeek") == Some(&json!("mo")) {
            rule.remove("firstDayOfWeek");
        }
        for day in rule
            .get_mut("byDay")
            .and_then(Value::as_array_mut)
            .into_iter()
            .flatten()
        {
            if let Some(day) = day.as_object_mut() {
                day.remove("@type");
            }
        }
        rule
    };
    plain(have) == plain(want)
}

/// Whether putting `want` would leave the `stored` event as it is.
pub fn same_event(stored: &Value, want: &Map<String, Value>) -> bool {
    let is_default = |name: &str, value: Option<&Value>| match value {
        None | Some(Value::Null) => true,
        Some(value) => DEFAULTS.iter().any(|(k, d)| *k == name && value == d),
    };
    let managed = MANAGED.iter().copied();
    let keys = managed.chain([VENDOR_SOURCE, VENDOR_SYNC]);
    keys.into_iter().all(|name| {
        let (have, wanted) = (stored.get(name), want.get(name));
        match name {
            _ if is_default(name, have) && is_default(name, wanted) => true,
            "showWithoutTime" => {
                have.and_then(Value::as_bool).unwrap_or(false)
                    == wanted.and_then(Value::as_bool).unwrap_or(false)
            }
            "priority" => {
                have.and_then(Value::as_i64).unwrap_or(0)
                    == wanted.and_then(Value::as_i64).unwrap_or(0)
            }
            "duration" => {
                have.and_then(duration_secs).unwrap_or(0)
                    == wanted.and_then(duration_secs).unwrap_or(0)
            }
            VENDOR_SOURCE | VENDOR_SYNC => {
                let ical = if name == VENDOR_SOURCE {
                    MARKER_SOURCE
                } else {
                    MARKER_SYNC
                };
                marker(stored, ical).as_deref() == wanted.and_then(Value::as_str) && have.is_some()
            }
            _ => holds(name, have, wanted),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SERIES: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\n\
        UID:s1\r\nDTSTAMP:20260101T000000Z\r\n\
        DTSTART;TZID=Europe/Berlin:20261110T090000\r\n\
        DTEND;TZID=Europe/Berlin:20261110T100000\r\n\
        SUMMARY:Standup\\, daily\r\nDESCRIPTION:Line1\\nLine2\r\nLOCATION:Room 1\r\n\
        TRANSP:TRANSPARENT\r\nSTATUS:TENTATIVE\r\n\
        RRULE:FREQ=WEEKLY;COUNT=5;BYDAY=TU\r\n\
        EXDATE;TZID=Europe/Berlin:20261117T090000\r\n\
        X-CAPYCAL-SOURCE:work/src-1\r\nX-CAPYCAL-SYNC:s\r\n\
        BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT15M\r\nDESCRIPTION:r\r\nEND:VALARM\r\n\
        END:VEVENT\r\nBEGIN:VEVENT\r\nUID:s1\r\nDTSTAMP:20260101T000000Z\r\n\
        RECURRENCE-ID;TZID=Europe/Berlin:20261124T090000\r\n\
        DTSTART;TZID=Europe/Berlin:20261124T110000\r\n\
        DTEND;TZID=Europe/Berlin:20261124T120000\r\n\
        SUMMARY:Moved\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

    fn lines_of(ical: &str) -> Vec<&str> {
        ical.lines().collect()
    }

    #[test]
    fn test_series_to_jscalendar() {
        let event = Value::Object(from_ical(SERIES).unwrap());
        assert_eq!(event["@type"], "Event");
        assert_eq!(event["uid"], "s1");
        assert_eq!(event["title"], "Standup, daily");
        assert_eq!(event["description"], "Line1\nLine2");
        assert_eq!(event["start"], "2026-11-10T09:00:00");
        assert_eq!(event["timeZone"], "Europe/Berlin");
        assert_eq!(event["duration"], "PT1H");
        assert_eq!(event["status"], "tentative");
        assert_eq!(event["freeBusyStatus"], "free");
        assert_eq!(event["locations"]["1"]["name"], "Room 1");
        assert_eq!(event["alerts"]["1"]["trigger"]["offset"], "-PT15M");
        assert_eq!(event["recurrenceRule"]["frequency"], "weekly");
        assert_eq!(event["recurrenceRule"]["count"], 5);
        assert_eq!(event["recurrenceRule"]["byDay"][0]["day"], "tu");
        assert_eq!(event["capycal.app:source"], "work/src-1");
        assert_eq!(event["capycal.app:sync"], "s");
        let overrides = &event["recurrenceOverrides"];
        assert_eq!(overrides["2026-11-17T09:00:00"]["excluded"], true);
        let moved = &overrides["2026-11-24T09:00:00"];
        assert_eq!(moved["start"], "2026-11-24T11:00:00");
        assert_eq!(moved["title"], "Moved");
        // Not in the override, so the patch clears them
        assert!(moved["locations"].is_null() && moved.get("locations").is_some());
        assert!(moved.get("timeZone").is_none());
    }

    #[test]
    fn test_series_round_trip() {
        let event = Value::Object(from_ical(SERIES).unwrap());
        let ical = to_ical(&event).unwrap();
        let lines = lines_of(&ical);
        for expected in [
            "UID:s1",
            "DTSTART;TZID=Europe/Berlin:20261110T090000",
            "DTEND;TZID=Europe/Berlin:20261110T100000",
            "RRULE:FREQ=WEEKLY;COUNT=5;BYDAY=TU",
            "EXDATE;TZID=Europe/Berlin:20261117T090000",
            "SUMMARY:Standup\\, daily",
            "DESCRIPTION:Line1\\nLine2",
            "LOCATION:Room 1",
            "TRANSP:TRANSPARENT",
            "STATUS:TENTATIVE",
            "X-CAPYCAL-SOURCE:work/src-1",
            "X-CAPYCAL-SYNC:s",
            "TRIGGER:-PT15M",
            "RECURRENCE-ID;TZID=Europe/Berlin:20261124T090000",
            "DTSTART;TZID=Europe/Berlin:20261124T110000",
            "SUMMARY:Moved",
        ] {
            assert!(lines.contains(&expected), "{expected} missing in {ical}");
        }
        assert_eq!(ical.matches("BEGIN:VEVENT").count(), 2);
        // The cleared location of the override stays cleared, and only the
        // master has the marker-independent recurrence
        let moved = &ical[ical.rfind("BEGIN:VEVENT").unwrap()..];
        assert!(
            !moved.contains("LOCATION") && !moved.contains("RRULE"),
            "{moved}"
        );
    }

    #[test]
    fn test_times_all_day_utc_floating() {
        let at = |line: &str| {
            let ical = format!("BEGIN:VEVENT\nUID:u\n{line}\nEND:VEVENT\n");
            Value::Object(from_ical(&ical).unwrap())
        };
        let day = at("DTSTART;VALUE=DATE:20261112");
        assert_eq!(day["start"], "2026-11-12T00:00:00");
        assert_eq!(day["showWithoutTime"], true);
        assert_eq!(day["duration"], "P1D");
        assert!(day.get("timeZone").is_none());
        let ical = to_ical(&day).unwrap();
        assert!(ical.contains("DTSTART;VALUE=DATE:20261112\r\n"), "{ical}");
        assert!(ical.contains("DTEND;VALUE=DATE:20261113\r\n"), "{ical}");

        let two = at("DTSTART;VALUE=DATE:20261112\nDTEND;VALUE=DATE:20261114");
        assert_eq!(two["duration"], "P2D");

        let utc = at("DTSTART:20261110T090000Z\nDTEND:20261110T103000Z");
        assert_eq!(utc["timeZone"], "Etc/UTC");
        assert_eq!(utc["duration"], "PT1H30M");
        let ical = to_ical(&utc).unwrap();
        assert!(ical.contains("DTSTART:20261110T090000Z\r\n"), "{ical}");
        assert!(ical.contains("DTEND:20261110T103000Z\r\n"), "{ical}");

        let floating = at("DTSTART:20261110T090000\nDURATION:PT30M");
        assert!(floating.get("timeZone").is_none());
        assert_eq!(floating["duration"], "PT30M");
        let ical = to_ical(&floating).unwrap();
        assert!(ical.contains("DTSTART:20261110T090000\r\n"), "{ical}");
        assert!(ical.contains("DTEND:20261110T093000\r\n"), "{ical}");
    }

    #[test]
    fn test_duration_across_a_dst_change_is_exact() {
        // 2026-03-29 02:00 Berlin jumps to 03:00: 01:00 to 04:00 is two hours
        let ical = "BEGIN:VEVENT\nUID:u\nDTSTART;TZID=Europe/Berlin:20260329T010000\n\
                    DTEND;TZID=Europe/Berlin:20260329T040000\nEND:VEVENT\n";
        let event = Value::Object(from_ical(ical).unwrap());
        assert_eq!(event["duration"], "PT2H");
        let back = to_ical(&event).unwrap();
        assert!(
            back.contains("DTEND;TZID=Europe/Berlin:20260329T040000"),
            "{back}"
        );
    }

    #[test]
    fn test_windows_and_unknown_time_zones() {
        let windows = "BEGIN:VEVENT\nUID:u\nDTSTART;TZID=W. Europe Standard Time:20261110T090000\nEND:VEVENT\n";
        let event = from_ical(windows).unwrap();
        assert_eq!(event["timeZone"], "Europe/Berlin");
        let unknown = "BEGIN:VEVENT\nUID:u\nDTSTART;TZID=Nowhere:20261110T090000\nEND:VEVENT\n";
        assert!(!from_ical(unknown).unwrap().contains_key("timeZone"));
    }

    #[test]
    fn test_rule_until_is_local_and_back_in_utc() {
        let ical = "BEGIN:VEVENT\nUID:u\nDTSTART;TZID=Europe/Berlin:20261110T090000\n\
                    RRULE:FREQ=MONTHLY;INTERVAL=2;UNTIL=20271201T080000Z;BYDAY=2TU,-1FR;\
                    BYMONTH=1,3;WKST=SU\nEND:VEVENT\n";
        let event = Value::Object(from_ical(ical).unwrap());
        let rule = &event["recurrenceRule"];
        assert_eq!(rule["until"], "2027-12-01T09:00:00");
        assert_eq!(rule["interval"], 2);
        assert_eq!(rule["byDay"][1], json!({"day": "fr", "nthOfPeriod": -1}));
        assert_eq!(rule["byMonth"], json!(["1", "3"]));
        let back = to_ical(&event).unwrap();
        assert!(
            back.contains(
                "RRULE:FREQ=MONTHLY;UNTIL=20271201T080000Z;INTERVAL=2;BYDAY=2TU,-1FR;BYMONTH=1,3;WKST=SU"
            ),
            "{back}"
        );
    }

    #[test]
    fn test_rule_until_as_date_or_utc() {
        let timed = "BEGIN:VEVENT\nUID:u\nDTSTART;TZID=Europe/Berlin:20261110T090000\n\
                     RRULE:FREQ=DAILY;UNTIL=20261120\nEND:VEVENT\n";
        let event = Value::Object(from_ical(timed).unwrap());
        assert_eq!(event["recurrenceRule"]["until"], "2026-11-20T23:59:59");
        let back = to_ical(&event).unwrap();
        assert!(back.contains("UNTIL=20261120T225959Z"), "{back}");

        let all_day = "BEGIN:VEVENT\nUID:u\nDTSTART;VALUE=DATE:20261110\n\
                       RRULE:FREQ=DAILY;UNTIL=20261120T080000Z\nEND:VEVENT\n";
        let event = Value::Object(from_ical(all_day).unwrap());
        assert_eq!(event["recurrenceRule"]["until"], "2026-11-20T08:00:00");
        let back = to_ical(&event).unwrap();
        assert!(
            back.contains("RRULE:FREQ=DAILY;UNTIL=20261120\r\n"),
            "{back}"
        );
    }

    #[test]
    fn test_durations() {
        assert_eq!(parse_duration("P1W"), Some((7, 0)));
        assert_eq!(parse_duration("-PT15M"), Some((0, -900)));
        assert_eq!(parse_duration("P1DT2H"), Some((1, 7200)));
        assert_eq!(parse_duration("PT1.5S"), Some((0, 1)));
        assert_eq!(parse_duration("P1X"), None);
        assert_eq!(format_duration(86400 + 7200), "P1DT2H");
        assert_eq!(format_duration(86400), "P1D");
        assert_eq!(format_duration(90), "PT1M30S");
        assert_eq!(format_duration(0), "PT0S");
    }

    #[test]
    fn test_override_in_another_time_zone() {
        let ical = "BEGIN:VEVENT\nUID:u\nDTSTART;TZID=Europe/Berlin:20261110T090000\n\
                    DTEND;TZID=Europe/Berlin:20261110T100000\nRRULE:FREQ=WEEKLY;COUNT=3\n\
                    END:VEVENT\nBEGIN:VEVENT\nUID:u\n\
                    RECURRENCE-ID;TZID=America/New_York:20261117T030000\n\
                    DTSTART;TZID=America/New_York:20261117T050000\n\
                    DTEND;TZID=America/New_York:20261117T060000\nEND:VEVENT\n";
        let event = Value::Object(from_ical(ical).unwrap());
        // The key is in the zone of the series: 03:00 in New York is 09:00 in Berlin
        let patch = &event["recurrenceOverrides"]["2026-11-17T09:00:00"];
        assert_eq!(patch["start"], "2026-11-17T05:00:00", "{event}");
        assert_eq!(patch["timeZone"], "America/New_York");
        let back = to_ical(&event).unwrap();
        assert!(
            back.contains("RECURRENCE-ID;TZID=Europe/Berlin:20261117T090000"),
            "{back}"
        );
        assert!(
            back.contains("DTSTART;TZID=America/New_York:20261117T050000"),
            "{back}"
        );
    }

    #[test]
    fn test_content_properties_both_ways() {
        let ical = "BEGIN:VEVENT\nUID:u\nDTSTART:20261110T090000Z\nURL:https://x.test/page\n\
                    CATEGORIES:A\\,1,B\nCLASS:CONFIDENTIAL\nCOLOR:red\nPRIORITY:2\n\
                    CONFERENCE;VALUE=URI;LABEL=Join:https://meet.test/1\n\
                    ORGANIZER;CN=\"Org, Inc\":mailto:o@x.test\n\
                    ATTENDEE;CN=Att;PARTSTAT=ACCEPTED;ROLE=OPT-PARTICIPANT:mailto:a@x.test\n\
                    BEGIN:VALARM\nACTION:EMAIL\nTRIGGER;RELATED=END:PT5M\nEND:VALARM\n\
                    BEGIN:VALARM\nACTION:DISPLAY\nTRIGGER;VALUE=DATE-TIME:20261110T080000Z\nEND:VALARM\n\
                    END:VEVENT\n";
        let event = Value::Object(from_ical(ical).unwrap());
        assert_eq!(event["links"]["1"]["href"], "https://x.test/page");
        assert_eq!(event["keywords"], json!({"A,1": true, "B": true}));
        assert_eq!(event["privacy"], "secret");
        assert_eq!(event["virtualLocations"]["1"]["name"], "Join");
        assert_eq!(event["organizerCalendarAddress"], "mailto:o@x.test");
        assert_eq!(event["participants"]["1"]["roles"]["owner"], true);
        assert_eq!(event["participants"]["1"]["name"], "Org, Inc");
        assert_eq!(event["participants"]["2"]["roles"]["optional"], true);
        assert_eq!(
            event["participants"]["2"]["participationStatus"],
            "accepted"
        );
        assert_eq!(event["alerts"]["1"]["action"], "email");
        assert_eq!(event["alerts"]["1"]["trigger"]["relativeTo"], "end");
        assert_eq!(
            event["alerts"]["2"]["trigger"]["when"],
            "2026-11-10T08:00:00Z"
        );

        let back = to_ical(&event).unwrap();
        for expected in [
            "URL:https://x.test/page",
            "CATEGORIES:A\\,1,B",
            "CLASS:CONFIDENTIAL",
            "COLOR:red",
            "PRIORITY:2",
            "CONFERENCE;VALUE=URI;LABEL=\"Join\":https://meet.test/1",
            "ORGANIZER;CN=\"Org, Inc\":mailto:o@x.test",
            "ATTENDEE;CN=\"Att\";ROLE=OPT-PARTICIPANT;PARTSTAT=ACCEPTED:mailto:a@x.test",
            "TRIGGER;RELATED=END:PT5M",
            "TRIGGER;VALUE=DATE-TIME:20261110T080000Z",
        ] {
            assert!(
                lines_of(&back).contains(&expected),
                "{expected} missing in {back}"
            );
        }
    }

    #[test]
    fn test_patches_with_paths_and_rdate() {
        let event = json!({
            "uid": "u", "start": "2026-11-10T09:00:00", "timeZone": "Etc/UTC",
            "title": "Base", "duration": "PT1H",
            "locations": {"a": {"name": "Old"}},
            "recurrenceRules": [{"frequency": "daily", "count": 3}],
            "recurrenceOverrides": {
                "2026-11-11T09:00:00": {"locations/a/name": "New", "title": null},
                "2026-11-20T09:00:00": {}
            }
        });
        let ical = to_ical(&event).unwrap();
        assert!(ical.contains("RRULE:FREQ=DAILY;COUNT=3\r\n"), "{ical}");
        assert!(ical.contains("RDATE:20261120T090000Z\r\n"), "{ical}");
        let moved = &ical[ical.rfind("BEGIN:VEVENT").unwrap()..];
        assert!(moved.contains("RECURRENCE-ID:20261111T090000Z"), "{moved}");
        // No start in the patch: the occurrence stays where its key says
        assert!(moved.contains("DTSTART:20261111T090000Z"), "{moved}");
        assert!(moved.contains("DTEND:20261111T100000Z"), "{moved}");
        assert!(moved.contains("LOCATION:New"), "{moved}");
        assert!(!moved.contains("SUMMARY"), "{moved}");
    }

    #[test]
    fn test_override_at_its_own_time_has_no_start_in_the_patch() {
        let ical = "BEGIN:VEVENT\nUID:u\nDTSTART:20261110T090000Z\nRRULE:FREQ=DAILY;COUNT=3\n\
                    SUMMARY:A\nORGANIZER:mailto:o@x.test\nEND:VEVENT\nBEGIN:VEVENT\nUID:u\n\
                    RECURRENCE-ID:20261111T090000Z\nDTSTART:20261111T090000Z\nSUMMARY:B\n\
                    END:VEVENT\n";
        let event = Value::Object(from_ical(ical).unwrap());
        let patch = &event["recurrenceOverrides"]["2026-11-11T09:00:00"];
        assert_eq!(patch["title"], "B");
        assert!(patch.get("start").is_none(), "{patch}");
        // Servers refuse an organizer per occurrence
        assert!(patch.get("organizerCalendarAddress").is_none(), "{patch}");
    }

    #[test]
    fn test_markers_of_copies_written_over_caldav() {
        let event = json!({
            "uid": "u", "start": "2026-11-10T09:00:00",
            "iCalendar": {"properties": [
                ["x-capycal-source", {}, "unknown", "work/a,b"],
                ["x-capycal-sync", {}, "unknown", "s"]
            ]}
        });
        assert_eq!(marker(&event, MARKER_SOURCE).as_deref(), Some("work/a,b"));
        assert_eq!(marker(&event, MARKER_SYNC).as_deref(), Some("s"));
        let ical = to_ical(&event).unwrap();
        assert!(ical.contains("X-CAPYCAL-SOURCE:work/a,b\r\n"), "{ical}");
        // Over CalDAV they are no vendor properties yet, so a put moves them
        let want = from_ical(&ical).unwrap();
        assert!(!same_event(&event, &want));
    }

    #[test]
    fn test_update_patch_clears_what_is_gone() {
        let event =
            from_ical("BEGIN:VEVENT\nUID:u\nDTSTART:20261110T090000Z\nSUMMARY:T\nEND:VEVENT\n")
                .unwrap();
        let patch = update_patch(&event);
        assert_eq!(patch["title"], "T");
        assert_eq!(patch["description"], Value::Null);
        assert_eq!(patch["recurrenceRule"], Value::Null);
        assert_eq!(patch["organizerCalendarAddress"], Value::Null);
        assert!(!patch.contains_key("uid"));
    }

    #[test]
    fn test_same_event_ignores_what_a_server_adds() {
        let want = from_ical(SERIES).unwrap();
        // As a server returns it: without some @type, with its own fields
        let mut stored = Value::Object(want.clone());
        stored["id"] = json!("b");
        stored["updated"] = json!("2026-11-06T15:45:56Z");
        stored["privacy"] = json!("public");
        stored["duration"] = json!("PT60M");
        stored["recurrenceRule"]["interval"] = json!(1);
        stored["recurrenceRule"]["@type"] = json!("RecurrenceRule");
        stored["locations"]["1"]
            .as_object_mut()
            .unwrap()
            .remove("@type");
        stored["alerts"]["1"]["extra"] = json!(true);
        let moved = &mut stored["recurrenceOverrides"]["2026-11-24T09:00:00"];
        moved.as_object_mut().unwrap().remove("locations");
        assert!(same_event(&stored, &want));

        let changed = |f: &dyn Fn(&mut Value)| {
            let mut other = stored.clone();
            f(&mut other);
            !same_event(&other, &want)
        };
        assert!(changed(&|e| e["title"] = json!("Other")));
        assert!(changed(&|e| e["duration"] = json!("PT2H")));
        assert!(changed(&|e| e["recurrenceRule"]["count"] = json!(6)));
        assert!(changed(
            &|e| e["recurrenceRule"]["until"] = json!("2027-01-01T00:00:00")
        ));
        assert!(changed(&|e| e["locations"]["2"] = json!({"name": "More"})));
        assert!(changed(&|e| e["color"] = json!("red")));
        assert!(changed(&|e| {
            e.as_object_mut().unwrap().remove("capycal.app:sync");
        }));
        assert!(changed(&|e| {
            e["recurrenceOverrides"]["2026-11-24T09:00:00"]["title"] = json!("Other");
        }));
    }

    #[test]
    fn test_unknown_rule_fields_of_a_server_count_as_a_change() {
        let want = from_ical(SERIES).unwrap();
        let mut stored = Value::Object(want.clone());
        assert!(same_event(&stored, &want));
        // Only the defaults Stalwart fills in are known to be harmless
        stored["recurrenceRule"]["rscale"] = json!("gregorian");
        assert!(!same_event(&stored, &want));
    }

    #[test]
    fn test_invalid_events_are_errors() {
        assert!(from_ical("BEGIN:VCALENDAR\nEND:VCALENDAR\n").is_err());
        assert!(from_ical("BEGIN:VEVENT\nDTSTART:20261110T090000Z\nEND:VEVENT\n").is_err());
        assert!(from_ical("BEGIN:VEVENT\nUID:u\nEND:VEVENT\n").is_err());
        assert!(to_ical(&json!({"start": "2026-11-10T09:00:00"})).is_err());
        assert!(to_ical(&json!({"uid": "u", "start": "soon"})).is_err());
    }
}
