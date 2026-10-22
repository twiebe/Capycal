//! Transform forms of the Settings window: the fields of each action, their
//! texts and validation, and a readable title and summary per transform.

use capycal::{ConditionConfig, TransformAction, TransformConfig, duration_text};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    /// An iCal property such as SUMMARY, stored upper case.
    Property,
    Text,
    Regex,
    Duration,
    /// A whole number above 0.
    Count,
    /// HH:MM.
    Time,
}

#[derive(Debug)]
pub struct FieldSpec {
    pub key: &'static str,
    pub label: &'static str,
    pub kind: FieldKind,
    pub required: bool,
}

#[derive(Debug)]
pub struct ActionSpec {
    pub action: TransformAction,
    pub title: &'static str,
    /// What it does, under its title in the plus menu.
    pub description: &'static str,
    pub fields: &'static [FieldSpec],
}

const fn field(
    key: &'static str,
    label: &'static str,
    kind: FieldKind,
    required: bool,
) -> FieldSpec {
    FieldSpec {
        key,
        label,
        kind,
        required,
    }
}

const PROPERTY: FieldSpec = field("field", "Field", FieldKind::Property, true);

/// The fields offered while typing a field name.
pub const PROPERTY_SUGGESTIONS: [&str; 12] = [
    "SUMMARY",
    "DESCRIPTION",
    "LOCATION",
    "URL",
    "CATEGORIES",
    "CLASS",
    "TRANSP",
    "PRIORITY",
    "COMMENT",
    "ATTENDEE",
    "ORGANIZER",
    "COLOR",
];
const REPLACEMENT: FieldSpec = field("replacement", "Replace with", FieldKind::Text, true);
const PATTERN: FieldSpec = field("pattern", "Pattern", FieldKind::Regex, true);

/// Every action in the order of the plus menu.
pub const ACTIONS: [ActionSpec; 7] = [
    ActionSpec {
        action: TransformAction::SetField,
        title: "Set field",
        description: "Gives a field a fixed text, such as the title \"Busy\"",
        fields: &[PROPERTY, field("value", "Value", FieldKind::Text, true)],
    },
    ActionSpec {
        action: TransformAction::ReplaceField,
        title: "Replace text",
        description: "Replaces a text in a field with another",
        fields: &[
            PROPERTY,
            field("search", "Find", FieldKind::Text, true),
            REPLACEMENT,
            field("count", "Times (empty = all)", FieldKind::Count, false),
        ],
    },
    ActionSpec {
        action: TransformAction::ReplaceFieldRegex,
        title: "Replace regex",
        description: "Replaces what a regex matches in a field",
        fields: &[PROPERTY, PATTERN, REPLACEMENT],
    },
    ActionSpec {
        action: TransformAction::RemoveField,
        title: "Remove field",
        description: "Leaves a field out of the copy",
        fields: &[PROPERTY],
    },
    ActionSpec {
        action: TransformAction::AddBuffer,
        title: "Add buffer",
        description: "Starts the copy earlier and ends it later",
        fields: &[
            field("pre_event", "Before", FieldKind::Duration, false),
            field("post_event", "After", FieldKind::Duration, false),
        ],
    },
    ActionSpec {
        action: TransformAction::AddAlarm,
        title: "Add alarm",
        description: "Adds a reminder before the start",
        fields: &[field("trigger", "Before start", FieldKind::Duration, true)],
    },
    ActionSpec {
        action: TransformAction::Skip,
        title: "Skip events",
        description: "Leaves out events that match its conditions",
        fields: &[],
    },
];

/// A common step, filled in, at the end of the plus menu.
pub struct Template {
    pub title: &'static str,
    pub description: &'static str,
    pub action: TransformAction,
    /// Field keys and their texts; "{source}" stands for the source calendar.
    pub texts: &'static [(&'static str, &'static str)],
    pub when: fn() -> Vec<ConditionConfig>,
}

fn no_conditions() -> Vec<ConditionConfig> {
    Vec::new()
}

fn on_days(days: &[&str], start: Option<&str>, end: Option<&str>) -> Vec<ConditionConfig> {
    vec![ConditionConfig {
        days: Some(days.iter().map(|d| d.to_string()).collect()),
        start_time: start.map(str::to_string),
        end_time: end.map(str::to_string),
        ..ConditionConfig::default()
    }]
}

pub const TEMPLATES: [Template; 6] = [
    Template {
        title: "Hide the details: title \"Busy (source)\"",
        description: "Shows only the time, titled after the calendar",
        action: TransformAction::SetField,
        texts: &[("field", "SUMMARY"), ("value", "Busy ({source})")],
        when: no_conditions,
    },
    Template {
        title: "Travel time: 30 min before and after",
        description: "Blocks 30 minutes before and after each event",
        action: TransformAction::AddBuffer,
        texts: &[("pre_event", "30m"), ("post_event", "30m")],
        when: no_conditions,
    },
    Template {
        title: "Travel time for events at a place",
        description: "Blocks 30 min around events with a location",
        action: TransformAction::AddBuffer,
        texts: &[("pre_event", "30m"), ("post_event", "30m")],
        when: || {
            vec![ConditionConfig {
                field: "LOCATION".to_string(),
                empty: Some(false),
                ..ConditionConfig::default()
            }]
        },
    },
    Template {
        title: "Skip weekends",
        description: "Leaves out events on Saturday and Sunday",
        action: TransformAction::Skip,
        texts: &[],
        when: || on_days(&["sat", "sun"], None, None),
    },
    Template {
        title: "Skip outside working hours: Mon-Fri 8:00-18:00",
        description: "Leaves out weekday events before 8:00 and from 18:00",
        action: TransformAction::Skip,
        texts: &[],
        when: || {
            on_days(
                &["mon", "tue", "wed", "thu", "fri"],
                Some("18:00"),
                Some("07:59"),
            )
        },
    },
    Template {
        title: "Reminder 10 min before",
        description: "Adds a reminder 10 minutes before the start",
        action: TransformAction::AddAlarm,
        texts: &[("trigger", "10m")],
        when: no_conditions,
    },
];

pub const DAYS: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];
pub const DAY_NAMES: [&str; 7] = [
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
];

/// The form of `action`. Old action names never reach a form: the config
/// rejects them.
pub fn spec(action: &TransformAction) -> &'static ActionSpec {
    ACTIONS
        .iter()
        .find(|a| a.action == *action)
        .expect("every current action has a form")
}

/// What a field of `t` shows.
pub fn field_text(t: &TransformConfig, key: &str) -> String {
    let text = |v: &Option<String>| v.clone().unwrap_or_default();
    let time = |v: Option<std::time::Duration>| v.map(duration_text).unwrap_or_default();
    match key {
        "field" => t.field.clone(),
        "value" => text(&t.value),
        "search" => text(&t.search),
        "replacement" => text(&t.replacement),
        "pattern" => text(&t.pattern),
        "count" => t.count.map(|c| c.to_string()).unwrap_or_default(),
        "pre_event" => time(t.pre_event),
        "post_event" => time(t.post_event),
        "trigger" => time(t.trigger),
        _ => String::new(),
    }
}

/// One line per transform for the list, as in "Set SUMMARY to \"Busy\"",
/// with its conditions.
pub fn summary(t: &TransformConfig) -> String {
    let groups: Vec<String> = t
        .when
        .iter()
        .map(|group| {
            let tests: Vec<String> = group.tests().iter().map(condition_summary).collect();
            let either = or_list(&tests.iter().map(String::as_str).collect::<Vec<_>>());
            // Brackets only where an "and" follows or comes before
            if group.any.is_empty() || t.when.len() == 1 {
                either
            } else {
                format!("({either})")
            }
        })
        .collect();
    let conditions = and_list(&groups.iter().map(String::as_str).collect::<Vec<_>>());
    if t.action == TransformAction::Skip {
        let conditions = if conditions.is_empty() {
            "\u{2026}".to_string()
        } else {
            conditions
        };
        return format!("Skip events if {conditions}");
    }
    let action = action_summary(t);
    if conditions.is_empty() {
        action
    } else {
        format!("{action}, only if {conditions}")
    }
}

fn action_summary(t: &TransformConfig) -> String {
    // What is not filled in yet reads as "…" or "a field"
    let quoted = |v: &Option<String>| v.as_ref().map_or("…".to_string(), |v| format!("\"{v}\""));
    let pattern = t
        .pattern
        .as_deref()
        .filter(|p| !p.is_empty())
        .unwrap_or("…");
    let field = if t.field.is_empty() {
        "a field"
    } else {
        &t.field
    };
    match t.action {
        TransformAction::SetField => format!("Set {field} to {}", quoted(&t.value)),
        TransformAction::ReplaceField => format!(
            "Replace {} in {field} with {}",
            quoted(&t.search),
            quoted(&t.replacement)
        ),
        TransformAction::ReplaceFieldRegex => {
            format!(
                "Replace /{pattern}/ in {field} with {}",
                quoted(&t.replacement)
            )
        }
        TransformAction::RemoveField => format!("Remove {field}"),
        TransformAction::AddBuffer => {
            let parts: Vec<String> = [(t.pre_event, "before"), (t.post_event, "after")]
                .into_iter()
                .filter_map(|(d, side)| d.map(|d| format!("{} {side}", readable(d))))
                .collect();
            if parts.is_empty() {
                "Add buffer".to_string()
            } else {
                format!("Add buffer: {}", parts.join(", "))
            }
        }
        TransformAction::AddAlarm => match t.trigger {
            Some(d) => format!("Add alarm {} before", readable(d)),
            None => "Add alarm".to_string(),
        },
        _ => t.action.as_str().to_string(),
    }
}

/// A condition in words, as in "the title contains \"Lunch\"".
fn condition_summary(c: &ConditionConfig) -> String {
    let test = condition_test(c);
    match condition_subject(c) {
        Subject::Start => {
            let days = days_summary(c.days.as_deref().unwrap_or_default());
            // Padded to HH:MM, text order is time order
            let padded = |t: &str| match parse(FieldKind::Time, "", t) {
                Ok(Value::Text(t)) => t,
                _ => t.to_string(),
            };
            let range = match (&c.start_time, &c.end_time) {
                (Some(start), Some(end)) if padded(start) > padded(end) => {
                    format!(" until {end} and from {start}")
                }
                (Some(start), Some(end)) => format!(" {start}-{end}"),
                (Some(start), None) => format!(" from {start}"),
                (None, Some(end)) => format!(" until {end}"),
                (None, None) => String::new(),
            };
            let starts = if test == 0 {
                "starts"
            } else {
                "does not start"
            };
            format!("it {starts} {days}{range}")
        }
        Subject::Event => {
            let is = if test == 0 { "is" } else { "is not" };
            format!("it {is} all-day")
        }
        subject => {
            let name = match SUBJECTS.iter().find(|(s, _)| *s == subject) {
                Some((Subject::Property(_), label)) => format!("the {}", label.to_lowercase()),
                _ if c.field.is_empty() => "a field".to_string(),
                _ => c.field.clone(),
            };
            let quoted = c
                .contains
                .as_deref()
                .filter(|v| !v.is_empty())
                .map_or("…".to_string(), |v| format!("\"{v}\""));
            let pattern = c
                .matches
                .as_deref()
                .filter(|v| !v.is_empty())
                .unwrap_or("…");
            let test = match test {
                0 => format!("contains {quoted}"),
                1 => format!("does not contain {quoted}"),
                2 => format!("matches /{pattern}/"),
                3 => format!("does not match /{pattern}/"),
                4 => "is empty".to_string(),
                _ => "is not empty".to_string(),
            };
            format!("{name} {test}")
        }
    }
}

/// What a condition looks at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subject {
    Property(&'static str),
    OtherField,
    /// The start: days and times.
    Start,
    /// Whether the event is all-day.
    Event,
}

/// The subjects in the order of their popup, with their labels.
pub const SUBJECTS: [(Subject, &str); 6] = [
    (Subject::Property("SUMMARY"), "Title"),
    (Subject::Property("LOCATION"), "Location"),
    (Subject::Property("DESCRIPTION"), "Notes"),
    (Subject::OtherField, "Other field"),
    (Subject::Start, "Start"),
    (Subject::Event, "Event"),
];

/// The tests of a subject in the order of their popup.
pub fn tests(subject: Subject) -> &'static [&'static str] {
    match subject {
        Subject::Start => &["is on", "is not on"],
        Subject::Event => &["is all-day", "is not all-day"],
        _ => &[
            "contains",
            "does not contain",
            "matches regex",
            "does not match regex",
            "is empty",
            "is not empty",
        ],
    }
}

fn condition_subject(c: &ConditionConfig) -> Subject {
    if c.days.is_some() || c.start_time.is_some() || c.end_time.is_some() {
        return Subject::Start;
    }
    if c.all_day.is_some() {
        return Subject::Event;
    }
    SUBJECTS
        .iter()
        .find(|(s, _)| matches!(s, Subject::Property(p) if *p == c.field))
        .map_or(Subject::OtherField, |(s, _)| *s)
}

/// The index of a condition's test in `tests(subject)`.
fn condition_test(c: &ConditionConfig) -> usize {
    let not = usize::from(c.not);
    match condition_subject(c) {
        Subject::Start => not,
        Subject::Event => usize::from(c.all_day == Some(false)),
        _ if c.matches.is_some() => 2 + not,
        _ if c.empty.is_some() => 4 + usize::from(c.empty == Some(false)),
        _ => not,
    }
}

/// "1 h 30 min", "2 d", "30 s".
fn readable(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    let parts: Vec<String> = [
        (secs / 86400, "d"),
        (secs % 86400 / 3600, "h"),
        (secs % 3600 / 60, "min"),
        (secs % 60, "s"),
    ]
    .into_iter()
    .filter(|(n, _)| *n > 0)
    .map(|(n, unit)| format!("{n} {unit}"))
    .collect();
    if parts.is_empty() {
        "0 s".to_string()
    } else {
        parts.join(" ")
    }
}

/// "Mon-Fri" for three or more days in a row, else "Sat, Sun".
fn days_summary(days: &[String]) -> String {
    let mut index: Vec<usize> = days
        .iter()
        .filter_map(|d| DAYS.iter().position(|known| known == d))
        .collect();
    index.sort_unstable();
    index.dedup();
    let name = |i: usize| {
        let d = DAYS[i];
        format!("{}{}", d[..1].to_uppercase(), &d[1..])
    };
    match (index.first(), index.last()) {
        (Some(&first), Some(&last)) if index.len() >= 3 && last - first + 1 == index.len() => {
            format!("{}-{}", name(first), name(last))
        }
        // No day chosen yet
        _ if index.is_empty() => "\u{2026}".to_string(),
        _ => index.into_iter().map(name).collect::<Vec<_>>().join(", "),
    }
}

/// A transform being edited: the typed texts, their errors and the
/// transform with every valid value.
#[derive(Debug, Clone)]
pub struct TransformForm {
    pub transform: TransformConfig,
    texts: Vec<(&'static str, String)>,
    errors: Vec<(&'static str, String)>,
    /// The conditions one after the other; `transform.when` holds them
    /// grouped, as written to the config.
    flat: Vec<ConditionConfig>,
    /// Per joint between two conditions of `flat`: whether it is "or",
    /// which puts both into one `any` group.
    or: Vec<bool>,
    /// Per condition: the typed texts and their errors, as for the fields.
    conditions: Vec<ConditionTexts>,
}

#[derive(Debug, Clone, Default)]
struct ConditionTexts {
    texts: Vec<(&'static str, String)>,
    errors: Vec<(&'static str, String)>,
}

/// The keys of a condition's texts.
const CONDITION_KEYS: [&str; 4] = ["field", "value", "start_time", "end_time"];

fn condition_text(c: &ConditionConfig, key: &str) -> String {
    let text = |v: &Option<String>| v.clone().unwrap_or_default();
    match key {
        "field" => c.field.clone(),
        "value" => c
            .matches
            .clone()
            .or_else(|| c.contains.clone())
            .unwrap_or_default(),
        "start_time" => text(&c.start_time),
        "end_time" => text(&c.end_time),
        _ => String::new(),
    }
}

/// A place a dragged condition can land: before condition `gap` (the
/// count for the end), joined by "or" to the condition before it when
/// `or_before`, to the one after it when `or_after`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ConditionDrop {
    pub gap: usize,
    pub or_before: bool,
    pub or_after: bool,
}

impl TransformForm {
    pub fn new(transform: TransformConfig) -> Self {
        let texts = spec(&transform.action)
            .fields
            .iter()
            .map(|f| (f.key, field_text(&transform, f.key)))
            .collect();
        let mut flat = Vec::new();
        let mut or = Vec::new();
        for group in &transform.when {
            for (i, c) in group.tests().iter().enumerate() {
                if !flat.is_empty() {
                    or.push(i > 0);
                }
                flat.push(c.clone());
            }
        }
        let conditions = flat
            .iter()
            .map(|c| ConditionTexts {
                texts: CONDITION_KEYS
                    .iter()
                    .map(|k| (*k, condition_text(c, k)))
                    .collect(),
                errors: Vec::new(),
            })
            .collect();
        Self {
            transform,
            texts,
            errors: Vec::new(),
            flat,
            or,
            conditions,
        }
    }

    /// Write `flat` grouped by its "or" joints into `transform.when`.
    fn regroup(&mut self) {
        let mut when: Vec<ConditionConfig> = Vec::new();
        let mut group: Vec<ConditionConfig> = Vec::new();
        for (i, c) in self.flat.iter().enumerate() {
            group.push(c.clone());
            if !self.or.get(i).copied().unwrap_or(false) {
                when.push(match group.len() {
                    1 => group.remove(0),
                    _ => ConditionConfig {
                        any: std::mem::take(&mut group),
                        ..ConditionConfig::default()
                    },
                });
            }
        }
        self.transform.when = when;
    }

    /// The conditions one after the other.
    pub fn conditions(&self) -> &[ConditionConfig] {
        &self.flat
    }

    /// Whether condition `i` (from 1) and the one before it are joined by
    /// "or", so one of them is enough.
    pub fn joined_by_or(&self, i: usize) -> bool {
        i > 0 && self.or.get(i - 1).copied().unwrap_or(false)
    }

    /// Join condition `i` (from 1) to the one before it by "or" or "and".
    pub fn set_or(&mut self, i: usize, or: bool) {
        if let Some(joint) = i.checked_sub(1).and_then(|j| self.or.get_mut(j)) {
            *joint = or;
            self.regroup();
        }
    }

    /// Drop the joints around a removed condition `i` for one: its
    /// neighbours stay in one group only if both were in its group.
    fn remove_joint(&mut self, i: usize) {
        if self.or.is_empty() {
            return;
        }
        if i > 0 && i < self.or.len() {
            self.or[i - 1] &= self.or[i];
        }
        self.or.remove(i.min(self.or.len() - 1));
    }

    /// Where a dragged condition can land, top to bottom: each gap, and
    /// at the edge of an "or" group a second place inside it (its start
    /// below the gap, its end above).
    pub fn drops(&self) -> Vec<ConditionDrop> {
        let count = self.flat.len();
        let mut drops = Vec::new();
        for gap in 0..=count {
            let within = gap > 0 && gap < count && self.or[gap - 1];
            if !within && gap >= 2 && self.or[gap - 2] {
                drops.push(ConditionDrop {
                    gap,
                    or_before: true,
                    or_after: false,
                });
            }
            drops.push(ConditionDrop {
                gap,
                or_before: within,
                or_after: within,
            });
            if !within && gap + 1 < count && self.or[gap] {
                drops.push(ConditionDrop {
                    gap,
                    or_before: false,
                    or_after: true,
                });
            }
        }
        drops
    }

    /// Move condition `from` to `drop`. It leaves its group as when
    /// removed and is joined to its new neighbours as `drop` says.
    pub fn move_condition(&mut self, from: usize, drop: ConditionDrop) {
        if from >= self.flat.len() || drop.gap > self.flat.len() {
            return;
        }
        let condition = self.flat.remove(from);
        let texts = self.conditions.remove(from);
        self.remove_joint(from);
        let at = if drop.gap > from {
            drop.gap - 1
        } else {
            drop.gap
        };
        self.flat.insert(at, condition);
        self.conditions.insert(at, texts);
        let last = self.flat.len() - 1;
        if last > 0 {
            if at == 0 {
                self.or.insert(0, drop.or_after);
            } else if at == last {
                self.or.push(drop.or_before);
            } else {
                self.or[at - 1] = drop.or_before;
                self.or.insert(at, drop.or_after);
            }
        }
        self.regroup();
    }

    /// Swap condition `i` with the one before it; the joints stay where
    /// they are, so it takes the place of the other in its group.
    pub fn move_condition_up(&mut self, i: usize) {
        if i > 0 && i < self.flat.len() {
            self.flat.swap(i - 1, i);
            self.conditions.swap(i - 1, i);
            self.regroup();
        }
    }

    /// Replace the conditions, for a common step.
    pub fn set_when(&mut self, when: Vec<ConditionConfig>) {
        let mut transform = self.transform.clone();
        transform.when = when;
        let texts = std::mem::take(&mut self.texts);
        let errors = std::mem::take(&mut self.errors);
        *self = Self::new(transform);
        self.texts = texts;
        self.errors = errors;
    }

    /// Add a condition on the title, joined by "and".
    pub fn add_condition(&mut self) {
        if !self.flat.is_empty() {
            self.or.push(false);
        }
        self.flat.push(ConditionConfig {
            field: "SUMMARY".to_string(),
            contains: Some(String::new()),
            ..ConditionConfig::default()
        });
        self.conditions.push(ConditionTexts::default());
        self.regroup();
    }

    /// Remove condition `i` and the joint after it (before it for the
    /// last), so its neighbours keep how they were joined to the rest.
    pub fn remove_condition(&mut self, i: usize) {
        if i < self.flat.len() {
            self.flat.remove(i);
            self.conditions.remove(i);
            self.remove_joint(i);
            self.regroup();
        }
    }

    pub fn subject(&self, i: usize) -> Subject {
        self.flat
            .get(i)
            .map_or(Subject::Property("SUMMARY"), condition_subject)
    }

    pub fn test(&self, i: usize) -> usize {
        self.flat.get(i).map_or(0, condition_test)
    }

    /// Look at `subject`. Another property keeps the test and its text,
    /// any other change starts with the subject's first test.
    pub fn set_subject(&mut self, i: usize, subject: Subject) {
        self.set_subject_flat(i, subject);
        self.regroup();
    }

    fn set_subject_flat(&mut self, i: usize, subject: Subject) {
        let (Some(c), Some(texts)) = (self.flat.get_mut(i), self.conditions.get_mut(i)) else {
            return;
        };
        let before = condition_subject(c);
        // AppKit sends the current item again when it is picked
        if before == subject {
            return;
        }
        let fields = |s: Subject| matches!(s, Subject::Property(_) | Subject::OtherField);
        if fields(before) && fields(subject) {
            c.field = match subject {
                Subject::Property(p) => p.to_string(),
                _ => String::new(),
            };
            texts.texts.retain(|(k, _)| *k != "field");
            texts.errors.retain(|(k, _)| *k != "field");
            return;
        }
        *c = match subject {
            Subject::Start => ConditionConfig {
                days: Some(Vec::new()),
                ..ConditionConfig::default()
            },
            Subject::Event => ConditionConfig {
                all_day: Some(true),
                ..ConditionConfig::default()
            },
            Subject::Property(p) => ConditionConfig {
                field: p.to_string(),
                contains: Some(String::new()),
                ..ConditionConfig::default()
            },
            Subject::OtherField => ConditionConfig {
                contains: Some(String::new()),
                ..ConditionConfig::default()
            },
        };
        *texts = ConditionTexts::default();
    }

    /// Use test `index` of `tests(subject)`; a text test keeps its text.
    pub fn set_test(&mut self, i: usize, index: usize) {
        let subject = self.subject(i);
        let value = self.condition_text(i, "value");
        let Some(c) = self.flat.get_mut(i) else {
            return;
        };
        match subject {
            Subject::Start => c.not = index == 1,
            Subject::Event => c.all_day = Some(index == 0),
            _ => {
                c.contains = None;
                c.matches = None;
                c.empty = None;
                c.not = matches!(index, 1 | 3);
                match index {
                    0 | 1 => c.contains = Some(value.clone()),
                    2 | 3 => c.matches = Some(value.clone()),
                    _ => c.empty = Some(index == 4),
                }
            }
        }
        if !matches!(subject, Subject::Start | Subject::Event) && index < 4 {
            self.set_condition(i, "value", &value);
        } else if let Some(texts) = self.conditions.get_mut(i) {
            texts.errors.retain(|(k, _)| *k != "value");
        }
        self.regroup();
    }

    /// Take the text of a condition's `key` ("field", "value",
    /// "start_time", "end_time").
    pub fn set_condition(&mut self, i: usize, key: &str, text: &str) {
        self.set_condition_flat(i, key, text);
        self.regroup();
    }

    fn set_condition_flat(&mut self, i: usize, key: &str, text: &str) {
        let Some(key) = CONDITION_KEYS.into_iter().find(|k| *k == key) else {
            return;
        };
        let (Some(c), Some(texts)) = (self.flat.get_mut(i), self.conditions.get_mut(i)) else {
            return;
        };
        let regex = c.matches.is_some();
        let kind = match key {
            "field" => FieldKind::Property,
            "value" if regex => FieldKind::Regex,
            "value" => FieldKind::Text,
            _ => FieldKind::Time,
        };
        let shown = if kind == FieldKind::Property {
            text.to_uppercase()
        } else {
            text.to_string()
        };
        texts.texts.retain(|(k, _)| *k != key);
        texts.texts.push((key, shown));
        texts.errors.retain(|(k, _)| *k != key);
        let value = match parse(kind, key, text) {
            Ok(Value::Text(v)) => Some(v),
            Ok(_) => None,
            Err(e) => {
                texts.errors.push((key, e));
                None
            }
        };
        match key {
            "field" => c.field = value.unwrap_or_default(),
            "value" if regex => c.matches = Some(value.unwrap_or_default()),
            "value" if c.contains.is_some() => c.contains = Some(value.unwrap_or_default()),
            "start_time" => c.start_time = value,
            "end_time" => c.end_time = value,
            _ => {}
        }
    }

    /// Switch weekday `day` of a start condition on or off.
    pub fn toggle_day(&mut self, i: usize, day: &str, on: bool) {
        let Some(c) = self.flat.get_mut(i) else {
            return;
        };
        let current = c.days.take().unwrap_or_default();
        c.days = Some(
            DAYS.iter()
                .filter(|d| {
                    if **d == day {
                        on
                    } else {
                        current.iter().any(|c| c == *d)
                    }
                })
                .map(|d| d.to_string())
                .collect(),
        );
        self.regroup();
    }

    pub fn condition_text(&self, i: usize, key: &str) -> String {
        let typed = self
            .conditions
            .get(i)
            .and_then(|t| t.texts.iter().find(|(k, _)| *k == key));
        match typed {
            Some((_, text)) => text.clone(),
            None => self
                .flat
                .get(i)
                .map(|c| condition_text(c, key))
                .unwrap_or_default(),
        }
    }

    pub fn condition_error(&self, i: usize, key: &str) -> Option<&str> {
        self.conditions
            .get(i)?
            .errors
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, e)| e.as_str())
    }

    /// The first condition that is not complete yet, from 1, and whether
    /// it has a wrong text.
    fn incomplete_condition(&self) -> Option<(usize, bool)> {
        self.flat.iter().enumerate().find_map(|(i, c)| {
            if self.conditions.get(i).is_some_and(|t| !t.errors.is_empty()) {
                return Some((i + 1, true));
            }
            let empty = |v: &Option<String>| v.as_deref().is_some_and(str::is_empty);
            let incomplete = match condition_subject(c) {
                Subject::Start => c.days.as_ref().is_none_or(Vec::is_empty),
                Subject::Event => false,
                _ => c.field.is_empty() || empty(&c.contains) || empty(&c.matches),
            };
            incomplete.then_some((i + 1, false))
        })
    }

    pub fn title(&self) -> &'static str {
        spec(&self.transform.action).title
    }

    /// Take the text of field `key`: its value when valid, else an error
    /// for the field and no value.
    pub fn set(&mut self, key: &str, text: &str) {
        let Some(spec) = spec(&self.transform.action)
            .fields
            .iter()
            .find(|f| f.key == key)
        else {
            return;
        };
        let key = spec.key;
        // iCal names ignore case; the field shows them as they are stored
        let upper;
        let text = if spec.kind == FieldKind::Property {
            upper = text.to_uppercase();
            upper.as_str()
        } else {
            text
        };
        match self.texts.iter_mut().find(|(k, _)| *k == key) {
            Some((_, t)) => *t = text.to_string(),
            None => self.texts.push((key, text.to_string())),
        }
        self.errors.retain(|(k, _)| *k != key);
        let t = &mut self.transform;
        let parsed = parse(spec.kind, key, text);
        if let Err(e) = &parsed {
            self.errors.push((key, e.clone()));
        }
        let value = parsed.unwrap_or(Value::None);
        match (key, value) {
            ("field", Value::Text(v)) => t.field = v,
            ("field", _) => t.field.clear(),
            ("count", Value::Count(n)) => t.count = Some(n),
            ("count", _) => t.count = None,
            ("pre_event" | "post_event" | "trigger", v) => {
                let d = match v {
                    Value::Duration(d) => Some(d),
                    _ => None,
                };
                match key {
                    "pre_event" => t.pre_event = d,
                    "post_event" => t.post_event = d,
                    _ => t.trigger = d,
                }
            }
            (_, v) => {
                let s = match v {
                    Value::Text(s) => Some(s),
                    _ => None,
                };
                match key {
                    "value" => t.value = s,
                    "search" => t.search = s,
                    "replacement" => t.replacement = s,
                    "pattern" => t.pattern = s,
                    _ => {}
                }
            }
        }
    }

    pub fn text(&self, key: &str) -> String {
        self.texts
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, t)| t.clone())
            .unwrap_or_default()
    }

    pub fn error(&self, key: &str) -> Option<&str> {
        self.errors
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, e)| e.as_str())
    }

    /// What is still to do before the transform can be saved: a field
    /// error, an empty required field, a buffer without a side.
    pub fn missing(&self) -> Option<String> {
        let t = &self.transform;
        let fields = spec(&t.action).fields;
        let label = |key: &'static str| {
            fields
                .iter()
                .find(|f| f.key == key)
                .map_or(key, |f| f.label)
        };
        if !self.errors.is_empty() {
            let wrong: Vec<&str> = self.errors.iter().map(|(k, _)| label(k)).collect();
            return Some(format!("fix {}", and_list(&wrong)));
        }
        let empty: Vec<&str> = fields
            .iter()
            .filter(|f| f.required)
            .filter(|f| match f.key {
                "field" => t.field.is_empty(),
                "replacement" => t.replacement.is_none(),
                key => field_text(t, key).is_empty(),
            })
            .map(|f| f.label)
            .collect();
        if !empty.is_empty() {
            return Some(format!("fill in {}", and_list(&empty)));
        }
        if t.action == TransformAction::AddBuffer && t.pre_event.is_none() && t.post_event.is_none()
        {
            return Some("set a time before or after".to_string());
        }
        if t.action == TransformAction::Skip && t.when.is_empty() {
            return Some("add a condition".to_string());
        }
        self.incomplete_condition().map(|(n, wrong)| {
            if wrong {
                format!("fix condition {n}")
            } else {
                format!("complete condition {n}")
            }
        })
    }
}

/// "A", "A and B", "A, B and C".
fn and_list(items: &[&str]) -> String {
    joined(items, "and")
}

/// "A", "A or B", "A, B or C".
fn or_list(items: &[&str]) -> String {
    joined(items, "or")
}

fn joined(items: &[&str], word: &str) -> String {
    match items {
        [] => String::new(),
        [one] => one.to_string(),
        [rest @ .., last] => format!("{} {word} {last}", rest.join(", ")),
    }
}

enum Value {
    None,
    Text(String),
    Count(usize),
    Duration(std::time::Duration),
}

/// The value of a field text; an empty text is no value, except for a
/// replacement, where empty means "remove the match". Texts and patterns
/// keep their spaces.
fn parse(kind: FieldKind, key: &str, raw: &str) -> Result<Value, String> {
    let text = raw.trim();
    let empty = match kind {
        FieldKind::Text => raw.is_empty(),
        _ => text.is_empty(),
    };
    if empty {
        return Ok(match key {
            "replacement" => Value::Text(String::new()),
            _ => Value::None,
        });
    }
    match kind {
        FieldKind::Property => Ok(Value::Text(text.to_uppercase())),
        FieldKind::Text => Ok(Value::Text(raw.to_string())),
        FieldKind::Regex => regex::Regex::new(raw)
            .map(|_| Value::Text(raw.to_string()))
            .map_err(|e| {
                let detail = e.to_string();
                let last = detail.lines().last().unwrap_or_default().trim().to_string();
                format!("Not a valid regex: {last}")
            }),
        FieldKind::Duration => humantime::parse_duration(text)
            .map(Value::Duration)
            .map_err(|_| "Use a duration such as 15m or 1h 30m".to_string()),
        FieldKind::Count => match text.parse::<usize>() {
            Ok(n) if n > 0 => Ok(Value::Count(n)),
            _ => Err("Use a whole number above 0".to_string()),
        },
        FieldKind::Time => {
            let valid = text.split_once(':').is_some_and(|(h, m)| {
                let digits = |s: &str| {
                    !s.is_empty() && s.len() <= 2 && s.bytes().all(|b| b.is_ascii_digit())
                };
                digits(h)
                    && digits(m)
                    && h.parse::<u32>().is_ok_and(|h| h < 24)
                    && m.parse::<u32>().is_ok_and(|m| m < 60)
            });
            if let (true, Some((h, m))) = (valid, text.split_once(':')) {
                Ok(Value::Text(format!("{h:0>2}:{m:0>2}")))
            } else {
                Err("Use HH:MM, such as 08:30".to_string())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use capycal::{TransformAction as A, TransformConfig};
    use std::time::Duration;

    fn t(action: A, set: &[(&str, &str)]) -> TransformConfig {
        let mut form = TransformForm::new(TransformConfig::new(action));
        for (k, v) in set {
            form.set(k, v);
        }
        assert!(form.missing().is_none(), "{set:?}: {:?}", form.errors);
        form.transform
    }

    #[test]
    fn test_titles_follow_the_spec_order() {
        let titles: Vec<&str> = ACTIONS.iter().map(|a| a.title).collect();
        assert_eq!(
            titles,
            [
                "Set field",
                "Replace text",
                "Replace regex",
                "Remove field",
                "Add buffer",
                "Add alarm",
                "Skip events",
            ]
        );
    }

    #[test]
    fn test_summaries() {
        let cases = [
            (TransformConfig::new(A::SetField), "Set a field to …"),
            (
                TransformConfig::new(A::ReplaceFieldRegex),
                "Replace /…/ in a field with …",
            ),
            (TransformConfig::new(A::AddBuffer), "Add buffer"),
            (
                t(A::SetField, &[("field", "summary"), ("value", "Busy")]),
                "Set SUMMARY to \"Busy\"",
            ),
            (
                t(
                    A::ReplaceField,
                    &[
                        ("field", "SUMMARY"),
                        ("search", "foo"),
                        ("replacement", "bar"),
                    ],
                ),
                "Replace \"foo\" in SUMMARY with \"bar\"",
            ),
            (
                t(
                    A::ReplaceFieldRegex,
                    &[
                        ("field", "SUMMARY"),
                        ("pattern", "^Re: "),
                        ("replacement", ""),
                    ],
                ),
                "Replace /^Re: / in SUMMARY with \"\"",
            ),
            (
                t(A::RemoveField, &[("field", "LOCATION")]),
                "Remove LOCATION",
            ),
            (
                t(A::AddBuffer, &[("pre_event", "15m"), ("post_event", "10m")]),
                "Add buffer: 15 min before, 10 min after",
            ),
            (
                t(A::AddBuffer, &[("post_event", "1h 30m")]),
                "Add buffer: 1 h 30 min after",
            ),
            (
                t(A::AddAlarm, &[("trigger", "15m")]),
                "Add alarm 15 min before",
            ),
        ];
        for (transform, expected) in cases {
            assert_eq!(summary(&transform), expected);
        }
    }

    #[test]
    fn test_invalid_input_stays_visible_with_an_error() {
        let mut form = TransformForm::new(TransformConfig::new(A::ReplaceFieldRegex));
        form.set("field", "SUMMARY");
        form.set("pattern", "(");
        form.set("replacement", "x");
        assert!(form.missing().is_some());
        assert_eq!(form.text("pattern"), "(");
        assert!(
            form.error("pattern")
                .unwrap()
                .starts_with("Not a valid regex")
        );
        form.set("pattern", "a+");
        assert!(form.missing().is_none());
        assert_eq!(form.transform.pattern.as_deref(), Some("a+"));

        let mut alarm = TransformForm::new(TransformConfig::new(A::AddAlarm));
        alarm.set("trigger", "abc");
        assert_eq!(
            alarm.error("trigger"),
            Some("Use a duration such as 15m or 1h 30m")
        );
        assert_eq!(alarm.transform.trigger, None);
        alarm.set("trigger", "1h");
        assert_eq!(alarm.transform.trigger, Some(Duration::from_secs(3600)));
    }

    #[test]
    fn test_required_fields_and_buffer_needs_one_side() {
        let empty = TransformForm::new(TransformConfig::new(A::SetField));
        assert!(empty.missing().is_some());
        let buffer = TransformForm::new(TransformConfig::new(A::AddBuffer));
        assert!(buffer.missing().is_some());
        let mut cleared =
            TransformForm::new(t(A::SetField, &[("field", "SUMMARY"), ("value", "x")]));
        cleared.set("value", "");
        assert!(cleared.missing().is_some(), "an emptied required field");
        assert_eq!(cleared.transform.value, None);
        // What is missing, in words
        assert_eq!(empty.missing().unwrap(), "fill in Field and Value");
        assert_eq!(cleared.missing().unwrap(), "fill in Value");
        assert_eq!(buffer.missing().unwrap(), "set a time before or after");
        let mut wrong = TransformForm::new(TransformConfig::new(A::ReplaceFieldRegex));
        wrong.set("pattern", "(");
        assert_eq!(wrong.missing().unwrap(), "fix Pattern");
    }

    #[test]
    fn test_field_texts_of_an_existing_transform() {
        let mut existing = TransformConfig::new(A::AddBuffer);
        existing.pre_event = Some(Duration::from_secs(2 * 86400));
        existing.post_event = Some(Duration::from_secs(90));
        let form = TransformForm::new(existing);
        assert_eq!(form.text("pre_event"), "2d");
        assert_eq!(form.text("post_event"), "1m 30s");
        assert!(form.missing().is_none());
    }

    #[test]
    fn test_spaces_are_values_and_times_are_padded() {
        let mut form = TransformForm::new(TransformConfig::new(A::ReplaceField));
        form.set("field", "SUMMARY");
        form.set("search", "_");
        form.set("replacement", " ");
        assert!(form.missing().is_none());
        assert_eq!(form.transform.replacement.as_deref(), Some(" "));
        let mut set = TransformForm::new(TransformConfig::new(A::SetField));
        set.set("field", "SUMMARY");
        set.set("value", " ");
        assert!(set.missing().is_none());
    }

    fn skip() -> TransformForm {
        TransformForm::new(TransformConfig::new(A::Skip))
    }

    #[test]
    fn test_a_new_condition_tests_the_title() {
        let mut form = skip();
        assert_eq!(form.missing().unwrap(), "add a condition");
        form.add_condition();
        assert_eq!(form.subject(0), Subject::Property("SUMMARY"));
        assert_eq!(form.test(0), 0);
        assert_eq!(form.missing().unwrap(), "complete condition 1");
        form.set_condition(0, "value", "Lunch");
        assert!(form.missing().is_none());
        assert_eq!(form.transform.when[0].contains.as_deref(), Some("Lunch"));
        assert_eq!(
            summary(&form.transform),
            "Skip events if the title contains \"Lunch\""
        );
    }

    #[test]
    fn test_condition_tests_write_their_keys() {
        let mut form = skip();
        form.add_condition();
        form.set_condition(0, "value", "^1:1");
        form.set_test(0, 3);
        let c = &form.transform.when[0];
        assert_eq!(
            (c.matches.as_deref(), c.contains.as_deref(), c.not),
            (Some("^1:1"), None, true)
        );
        assert_eq!(form.condition_text(0, "value"), "^1:1");
        form.set_test(0, 5);
        let c = &form.transform.when[0];
        assert_eq!(
            (c.empty, c.matches.as_deref(), c.not),
            (Some(false), None, false)
        );
        assert!(form.missing().is_none());
        form.set_condition(0, "value", "x");
        form.set_test(0, 2);
        form.set_condition(0, "value", "(");
        assert!(
            form.condition_error(0, "value")
                .unwrap()
                .starts_with("Not a valid regex")
        );
        assert_eq!(form.missing().unwrap(), "fix condition 1");
    }

    #[test]
    fn test_choosing_the_same_subject_again_keeps_the_condition() {
        let mut form = skip();
        form.add_condition();
        form.set_subject(0, Subject::OtherField);
        form.set_condition(0, "field", "CATEGORIES");
        form.set_subject(0, Subject::OtherField);
        assert_eq!(form.transform.when[0].field, "CATEGORIES");
        form.set_subject(0, Subject::Start);
        form.toggle_day(0, "sat", true);
        form.set_condition(0, "start_time", "18:00");
        form.set_subject(0, Subject::Start);
        assert_eq!(
            form.transform.when[0].days.as_deref(),
            Some(&["sat".to_string()][..])
        );
        assert_eq!(form.condition_text(0, "start_time"), "18:00");
        form.set_subject(0, Subject::Event);
        form.set_test(0, 1);
        form.set_subject(0, Subject::Event);
        assert_eq!(form.transform.when[0].all_day, Some(false));
    }

    #[test]
    fn test_condition_subjects() {
        let mut form = skip();
        form.add_condition();
        form.set_condition(0, "value", "Zoom");
        // Another property keeps the test and its text
        form.set_subject(0, Subject::Property("LOCATION"));
        assert_eq!(form.transform.when[0].field, "LOCATION");
        assert_eq!(form.transform.when[0].contains.as_deref(), Some("Zoom"));
        form.set_subject(0, Subject::OtherField);
        assert_eq!(form.subject(0), Subject::OtherField);
        assert_eq!(form.missing().unwrap(), "complete condition 1");
        form.set_condition(0, "field", "categories");
        assert_eq!(form.transform.when[0].field, "CATEGORIES");
        assert_eq!(form.subject(0), Subject::OtherField);
        assert!(form.missing().is_none());

        form.set_subject(0, Subject::Start);
        assert_eq!(form.subject(0), Subject::Start);
        assert_eq!(form.missing().unwrap(), "complete condition 1");
        form.toggle_day(0, "sat", true);
        form.toggle_day(0, "sun", true);
        form.set_condition(0, "start_time", "8:5");
        assert_eq!(form.transform.when[0].start_time.as_deref(), Some("08:05"));
        form.set_condition(0, "end_time", "25:00");
        assert_eq!(
            form.condition_error(0, "end_time"),
            Some("Use HH:MM, such as 08:30")
        );
        form.set_condition(0, "end_time", "");
        assert!(form.missing().is_none());
        assert_eq!(form.transform.when[0].field, "");
        assert_eq!(
            summary(&form.transform),
            "Skip events if it starts Sat, Sun from 08:05"
        );
        form.set_test(0, 1);
        assert_eq!(
            summary(&form.transform),
            "Skip events if it does not start Sat, Sun from 08:05"
        );

        form.set_subject(0, Subject::Event);
        assert_eq!(form.transform.when[0].all_day, Some(true));
        assert_eq!(form.transform.when[0].days, None);
        assert_eq!(summary(&form.transform), "Skip events if it is all-day");
        form.set_test(0, 1);
        assert_eq!(form.transform.when[0].all_day, Some(false));
    }

    #[test]
    fn test_conditions_of_other_actions_and_their_summary() {
        let mut form = TransformForm::new(t(A::AddBuffer, &[("pre_event", "30m")]));
        assert!(form.missing().is_none(), "conditions are optional");
        form.add_condition();
        form.set_subject(0, Subject::Property("LOCATION"));
        form.set_test(0, 5);
        form.add_condition();
        form.set_subject(1, Subject::Property("LOCATION"));
        form.set_test(1, 1);
        form.set_condition(1, "value", "zoom.us");
        assert_eq!(
            summary(&form.transform),
            "Add buffer: 30 min before, only if the location is not empty and the location does not contain \"zoom.us\""
        );
        form.remove_condition(0);
        assert_eq!(form.transform.when.len(), 1);
        assert_eq!(form.condition_text(0, "value"), "zoom.us");
        // Start times wrap around midnight like before
        let mut range = skip();
        range.add_condition();
        range.set_subject(0, Subject::Start);
        for day in ["mon", "tue", "wed", "thu", "fri"] {
            range.toggle_day(0, day, true);
        }
        range.set_condition(0, "start_time", "18:00");
        range.set_condition(0, "end_time", "08:00");
        assert_eq!(
            summary(&range.transform),
            "Skip events if it starts Mon-Fri until 08:00 and from 18:00"
        );
        // A config file may leave out the leading zero
        let mut unpadded = TransformConfig::new(A::Skip);
        unpadded.when = vec![capycal::ConditionConfig {
            days: Some(vec!["mon".to_string()]),
            start_time: Some("8:00".to_string()),
            end_time: Some("18:00".to_string()),
            ..Default::default()
        }];
        assert_eq!(
            summary(&unpadded),
            "Skip events if it starts Mon 8:00-18:00"
        );
    }

    #[test]
    fn test_any_groups_load_as_conditions_joined_by_or() {
        let title = |text: &str| capycal::ConditionConfig {
            field: "SUMMARY".to_string(),
            contains: Some(text.to_string()),
            ..capycal::ConditionConfig::default()
        };
        let mut existing = TransformConfig::new(A::Skip);
        existing.when = vec![
            title("A"),
            capycal::ConditionConfig {
                any: vec![title("B"), title("C")],
                ..capycal::ConditionConfig::default()
            },
        ];
        let mut form = TransformForm::new(existing.clone());
        assert_eq!(form.conditions().len(), 3);
        assert!(!form.joined_by_or(1) && form.joined_by_or(2));
        assert_eq!(form.condition_text(2, "value"), "C");
        // Unchanged, it writes the same groups back
        form.set_condition(0, "value", "A");
        assert_eq!(form.transform.when, existing.when);
        // Removing the first keeps B or C together
        form.remove_condition(0);
        assert_eq!(form.transform.when.len(), 1);
        assert_eq!(form.transform.when[0].any.len(), 2);
        // Removing the last of a group of two leaves a plain condition
        form.remove_condition(1);
        assert_eq!(form.transform.when, vec![title("B")]);
        // A new condition joins by "and"
        form.add_condition();
        assert!(!form.joined_by_or(1));
        assert_eq!(form.transform.when.len(), 2);
    }

    #[test]
    fn test_a_dragged_condition_lands_where_it_is_dropped() {
        let mut form = TransformForm::new(TransformConfig::new(A::Skip));
        for text in ["A", "B", "C", "D"] {
            form.add_condition();
            let i = form.conditions().len() - 1;
            form.set_condition(i, "value", text);
        }
        // A and (B or C) and D
        form.set_or(2, true);
        let values = |form: &TransformForm| -> Vec<String> {
            (0..form.conditions().len())
                .map(|i| form.condition_text(i, "value"))
                .collect()
        };
        let ors = |form: &TransformForm| -> Vec<bool> {
            (1..form.conditions().len())
                .map(|i| form.joined_by_or(i))
                .collect()
        };
        let drop = |gap, or_before, or_after| ConditionDrop {
            gap,
            or_before,
            or_after,
        };
        // Around the group: before it, its start, its middle, its end,
        // after it
        assert_eq!(
            form.drops(),
            [
                drop(0, false, false),
                drop(1, false, false),
                drop(1, false, true),
                drop(2, true, true),
                drop(3, true, false),
                drop(3, false, false),
                drop(4, false, false),
            ]
        );
        // D into the middle of the group
        form.move_condition(3, drop(2, true, true));
        assert_eq!(values(&form), ["A", "B", "D", "C"]);
        assert_eq!(ors(&form), [false, true, true]);
        // A to the start of the group
        form.move_condition(0, drop(1, false, true));
        assert_eq!(values(&form), ["A", "B", "D", "C"]);
        assert_eq!(ors(&form), [true, true, true]);
        // C right below the group, out of it
        form.move_condition(3, drop(4, false, false));
        assert_eq!(values(&form), ["A", "B", "D", "C"]);
        assert_eq!(ors(&form), [true, true, false]);
        // A right above the group, out of it
        form.move_condition(0, drop(0, false, false));
        assert_eq!(values(&form), ["A", "B", "D", "C"]);
        assert_eq!(ors(&form), [false, true, false]);
        // C to the end of the group
        form.move_condition(3, drop(3, true, false));
        assert_eq!(values(&form), ["A", "B", "D", "C"]);
        assert_eq!(ors(&form), [false, true, true]);
        assert_eq!(form.transform.when.len(), 2);
    }

    #[test]
    fn test_condition_texts_of_an_existing_transform() {
        let mut existing = TransformConfig::new(A::Skip);
        existing.when = vec![capycal::ConditionConfig {
            field: "X-TAG".to_string(),
            matches: Some("a|b".to_string()),
            not: true,
            ..Default::default()
        }];
        let form = TransformForm::new(existing);
        assert_eq!(form.subject(0), Subject::OtherField);
        assert_eq!(form.test(0), 3);
        assert_eq!(form.condition_text(0, "field"), "X-TAG");
        assert_eq!(form.condition_text(0, "value"), "a|b");
        assert!(form.missing().is_none());
    }
}
