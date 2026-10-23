//! The sync form of the Settings window: a sync being edited as texts, its
//! errors, and what saving it does to the copies already made.

use capycal::{DEFAULT_CONCURRENCY, SyncConfig, TransformAction, TransformConfig, duration_text};

use crate::transform_form::{TransformForm, summary};

/// The content properties offered as checkboxes, with a hint each.
pub const COMMON_FIELDS: [(&str, &str); 8] = [
    ("SUMMARY", "Title"),
    ("DESCRIPTION", "Notes"),
    ("LOCATION", "Location"),
    ("VALARM", "Reminders of the source event"),
    ("URL", "Link"),
    ("CATEGORIES", "Categories"),
    ("ATTENDEE", "Attendees"),
    ("ORGANIZER", "Organizer"),
];

pub const STRUCTURAL_NOTE: &str = "Times, recurrence, status and free/busy are always copied.";

/// Shown while attendees or the organizer are copied.
pub const SCHEDULING_WARNING: &str =
    "A server that handles invitations may email the attendees of the copies.";

pub const VALARM_HINT: &str = "VALARM copies the reminders of the source events. \
    \"Add alarm\" adds a reminder of its own, with or without VALARM.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncField {
    Name,
    Interval,
    DaysPast,
    DaysFuture,
    /// `concurrency` of the target calendar.
    Concurrency,
}

/// The texts and choices of a form, for comparing.
#[derive(Debug, Clone, PartialEq)]
struct Texts {
    values: [String; 7],
    flags: (Option<bool>, bool),
    common: Vec<String>,
    transforms: Vec<TransformConfig>,
}

#[derive(Debug, Clone)]
pub struct SyncForm {
    original: Option<SyncConfig>,
    /// What a new form showed at first, to tell whether it was touched.
    initial: Option<Texts>,
    name: String,
    source: String,
    target: String,
    interval: String,
    days_past: String,
    days_future: String,
    /// Unset follows the source: on for all but Calendar.app.
    all_events: Option<bool>,
    enabled: bool,
    /// The target calendar's `concurrency` as typed, and as saved.
    concurrency: String,
    saved_concurrency: usize,
    common: Vec<String>,
    other: String,
    transforms: Vec<TransformForm>,
    /// The name was not typed: picking the calendars names the sync.
    auto_name: bool,
}

impl SyncForm {
    /// A new sync named "sync", "sync_2", ... with the defaults.
    pub fn new_sync(existing_names: &[String]) -> Self {
        let name = (1..)
            .map(|n| match n {
                1 => "sync".to_string(),
                n => format!("sync_{n}"),
            })
            .find(|name| !existing_names.contains(name))
            .unwrap_or_default();
        let mut form = Self::from_config(&SyncConfig::new(&name, "", ""));
        form.original = None;
        form.auto_name = true;
        form.initial = Some(form.texts());
        form
    }

    /// A new sync like `sync`, named "<name>-copy", "<name>-copy-2", ...
    pub fn duplicate(sync: &SyncConfig, existing_names: &[String]) -> Self {
        let name = (1..)
            .map(|n| match n {
                1 => format!("{}-copy", sync.name),
                n => format!("{}-copy-{n}", sync.name),
            })
            .find(|name| !existing_names.contains(name))
            .unwrap_or_default();
        let mut form = Self::from_config(sync);
        form.name = name;
        form.original = None;
        form
    }

    pub fn edit(sync: &SyncConfig) -> Self {
        Self::from_config(sync)
    }

    fn from_config(sync: &SyncConfig) -> Self {
        let common = COMMON_FIELDS
            .iter()
            .map(|(p, _)| p.to_string())
            .filter(|p| sync.fields.contains(p))
            .collect();
        let other = sync
            .fields
            .iter()
            .filter(|f| !COMMON_FIELDS.iter().any(|(p, _)| p == f))
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        Self {
            original: Some(sync.clone()),
            initial: None,
            name: sync.name.clone(),
            source: sync.source.clone(),
            target: sync.target.clone(),
            interval: duration_text(sync.interval),
            days_past: sync.days_past.to_string(),
            days_future: sync.days_future.to_string(),
            all_events: sync.all_events,
            enabled: sync.enabled,
            concurrency: DEFAULT_CONCURRENCY.to_string(),
            saved_concurrency: DEFAULT_CONCURRENCY,
            common,
            other,
            transforms: sync
                .transforms
                .iter()
                .cloned()
                .map(TransformForm::new)
                .collect(),
            auto_name: false,
        }
    }

    pub fn original_name(&self) -> Option<&str> {
        self.original.as_ref().map(|s| s.name.as_str())
    }

    /// Name a new sync after its calendars, unless its name was typed.
    pub fn propose_name(&mut self, existing_names: &[String]) {
        if !self.auto_name || self.source.is_empty() || self.target.is_empty() {
            return;
        }
        let base = format!("{}-to-{}", self.source, self.target);
        self.name = (1..)
            .map(|n| match n {
                1 => base.clone(),
                n => format!("{base}_{n}"),
            })
            .find(|name| !existing_names.contains(name))
            .unwrap_or(base);
    }

    pub fn set_name(&mut self, text: &str) {
        self.auto_name = false;
        self.name = text.trim().to_string();
    }

    pub fn set_source(&mut self, key: &str) {
        self.source = key.to_string();
    }

    pub fn set_target(&mut self, key: &str) {
        self.target = key.to_string();
    }

    pub fn set_interval(&mut self, text: &str) {
        self.interval = text.to_string();
    }

    pub fn set_days(&mut self, past: &str, future: &str) {
        self.days_past = past.to_string();
        self.days_future = future.to_string();
    }

    /// Choose all events; the source's default is left unset, so it follows
    /// a change of the source.
    pub fn set_all_events(&mut self, on: bool, eventkit_source: bool) {
        self.all_events = (on == eventkit_source).then_some(on);
    }

    pub fn set_enabled(&mut self, on: bool) {
        self.enabled = on;
    }

    /// The `concurrency` the target calendar has; drops what was typed.
    pub fn set_target_concurrency(&mut self, concurrency: usize) {
        self.concurrency = concurrency.to_string();
        self.saved_concurrency = concurrency;
    }

    pub fn set_concurrency(&mut self, text: &str) {
        self.concurrency = text.to_string();
    }

    /// The new `concurrency` of the target calendar, if it was changed.
    pub fn concurrency_change(&self) -> Option<usize> {
        self.concurrency
            .trim()
            .parse()
            .ok()
            .filter(|n| *n > 0 && *n != self.saved_concurrency)
    }

    /// The text of a field as typed.
    pub fn text(&self, field: SyncField) -> &str {
        match field {
            SyncField::Name => &self.name,
            SyncField::Interval => &self.interval,
            SyncField::DaysPast => &self.days_past,
            SyncField::DaysFuture => &self.days_future,
            SyncField::Concurrency => &self.concurrency,
        }
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn target(&self) -> &str {
        &self.target
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn all_events(&self, eventkit_source: bool) -> bool {
        self.all_events.unwrap_or(!eventkit_source)
    }

    pub fn is_selected(&self, property: &str) -> bool {
        self.common.iter().any(|p| p == property)
    }

    pub fn other_fields(&self) -> &str {
        &self.other
    }

    pub fn toggle_field(&mut self, property: &str) {
        match self.common.iter().position(|p| p == property) {
            Some(i) => {
                self.common.remove(i);
            }
            None => self.common.push(property.to_string()),
        }
    }

    /// Copy exactly these common fields and no others.
    pub fn set_fields(&mut self, fields: &[&str]) {
        self.common = fields.iter().map(|f| f.to_string()).collect();
        self.other.clear();
    }

    pub fn set_other_fields(&mut self, text: &str) {
        self.other = text.to_string();
    }

    /// The common fields in their order, then the others as typed, upper
    /// case and without duplicates.
    pub fn fields(&self) -> Vec<String> {
        let mut fields: Vec<String> = COMMON_FIELDS
            .iter()
            .map(|(p, _)| p.to_string())
            .filter(|p| self.common.contains(p))
            .collect();
        for field in self.other.split([',', ' ']).filter(|f| !f.is_empty()) {
            let field = field.to_uppercase();
            if !fields.contains(&field) {
                fields.push(field);
            }
        }
        fields
    }

    /// Insert a new transform at `at` (at most the end); returns its index.
    pub fn insert_transform(&mut self, at: usize, action: TransformAction) -> usize {
        let at = at.min(self.transforms.len());
        let skip = action == TransformAction::Skip;
        let mut form = TransformForm::new(TransformConfig::new(action));
        // A skip needs a condition: it starts with one
        if skip {
            form.add_condition();
        }
        self.transforms.insert(at, form);
        at
    }

    pub fn remove_transform(&mut self, index: usize) {
        if index < self.transforms.len() {
            self.transforms.remove(index);
        }
    }

    /// Move the transform at `from` into the gap before `gap` (the count
    /// for the end); returns where it ends up, None when nothing moved.
    pub fn move_transform_to(&mut self, from: usize, gap: usize) -> Option<usize> {
        let count = self.transforms.len();
        if from >= count || gap > count || gap == from || gap == from + 1 {
            return None;
        }
        let to = if gap > from { gap - 1 } else { gap };
        let transform = self.transforms.remove(from);
        self.transforms.insert(to, transform);
        Some(to)
    }

    pub fn transform(&self, index: usize) -> Option<&TransformForm> {
        self.transforms.get(index)
    }

    pub fn transform_mut(&mut self, index: usize) -> &mut TransformForm {
        &mut self.transforms[index]
    }

    /// One summary per transform, for the list.
    pub fn transform_rows(&self) -> Vec<String> {
        self.transforms
            .iter()
            .map(|t| summary(&t.transform))
            .collect()
    }

    pub fn error(&self, field: SyncField) -> Option<&'static str> {
        let text = self.text(field).trim();
        match field {
            SyncField::Name if text.is_empty() => Some("Name is required"),
            SyncField::Name
                if text.contains('/') || text.contains('\\') || text.starts_with('.') =>
            {
                Some("No slashes and no leading dot")
            }
            SyncField::Name => None,
            SyncField::Interval => match humantime::parse_duration(text) {
                Ok(d) if !d.is_zero() => None,
                _ => Some("Use a duration such as 5m or 1h"),
            },
            SyncField::DaysPast | SyncField::DaysFuture => text
                .parse::<u64>()
                .err()
                .map(|_| "Use a whole number of days"),
            SyncField::Concurrency => match text.parse::<usize>() {
                Ok(n) if n > 0 => None,
                _ => Some("Use a whole number such as 2"),
            },
        }
    }

    /// The sync to save, or the first problem. `other_names` are the names
    /// of the other syncs.
    pub fn build(&self, other_names: &[String]) -> Result<SyncConfig, String> {
        const FIELDS: [SyncField; 5] = [
            SyncField::Name,
            SyncField::Interval,
            SyncField::DaysPast,
            SyncField::DaysFuture,
            SyncField::Concurrency,
        ];
        if let Some(e) = FIELDS.iter().find_map(|f| self.error(*f)) {
            return Err(e.to_string());
        }
        if self.source.is_empty() {
            return Err("Choose a source".to_string());
        }
        if self.target.is_empty() {
            return Err("Choose a target".to_string());
        }
        if self.source == self.target {
            return Err("Source and target must differ".to_string());
        }
        if other_names.contains(&self.name) {
            return Err(format!("A sync named \"{}\" exists already", self.name));
        }
        if let Some((i, t, missing)) = self
            .transforms
            .iter()
            .enumerate()
            .find_map(|(i, t)| t.missing().map(|m| (i, t, m)))
        {
            return Err(format!("{} (step {}): {missing}", t.title(), i + 1));
        }
        let duration = |t: &str| humantime::parse_duration(t.trim()).unwrap_or_default();
        let days = |t: &str| t.trim().parse().unwrap_or_default();
        let sync = SyncConfig {
            name: self.name.clone(),
            source: self.source.clone(),
            target: self.target.clone(),
            interval: duration(&self.interval),
            fields: self.fields(),
            transforms: self
                .transforms
                .iter()
                .map(|t| t.transform.clone())
                .collect(),
            days_past: days(&self.days_past),
            days_future: days(&self.days_future),
            all_events: self.all_events,
            enabled: self.enabled,
        };
        sync.compile_transforms().map_err(|e| {
            let message = e.to_string();
            let prefix = format!("sync '{}': ", sync.name);
            match message.find(&prefix) {
                Some(i) => message[i + prefix.len()..].to_string(),
                None => message,
            }
        })?;
        Ok(sync)
    }

    fn texts(&self) -> Texts {
        Texts {
            values: [
                &self.name,
                &self.source,
                &self.target,
                &self.interval,
                &self.days_past,
                &self.days_future,
                &self.other,
            ]
            .map(|t| t.to_string()),
            flags: (self.all_events, self.enabled),
            common: self.common.clone(),
            transforms: self
                .transforms
                .iter()
                .map(|t| t.transform.clone())
                .collect(),
        }
    }

    /// A new form that was touched, or one that differs from the saved sync.
    pub fn is_dirty(&self) -> bool {
        if self.concurrency_change().is_some() {
            return true;
        }
        match &self.original {
            None => self.initial.as_ref() != Some(&self.texts()),
            // Compared with the form of the saved sync, which orders fields
            // like this one does
            Some(original) => self.build(&[]) != SyncForm::edit(original).build(&[]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::transform_form::Subject;
    use capycal::{SyncConfig, TransformAction};
    use std::time::Duration;

    #[test]
    fn test_all_events_follows_the_source_until_chosen() {
        let mut form = SyncForm::edit(&SyncConfig::new("s", "a", "b"));
        assert!(form.all_events(false) && !form.all_events(true));
        // Off for a CalDAV source stays off for any source
        form.set_all_events(false, false);
        assert!(!form.all_events(false) && !form.all_events(true));
        // On for Calendar.app stays on
        form.set_all_events(true, true);
        assert!(form.all_events(false) && form.all_events(true));
        // The default of the source leaves it unset
        form.set_all_events(true, false);
        assert!(!form.all_events(true));
    }

    fn existing() -> SyncConfig {
        let mut s = SyncConfig::new("work-to-home", "work", "home");
        s.fields = vec!["SUMMARY".to_string(), "X-CUSTOM".to_string()];
        s
    }

    #[test]
    fn test_concurrency_is_a_whole_number_compared_with_the_target() {
        let mut form = SyncForm::edit(&existing());
        form.set_target_concurrency(1);
        assert_eq!(form.concurrency_change(), None);
        for bad in ["", "0", "two", "-1"] {
            form.set_concurrency(bad);
            assert!(form.error(SyncField::Concurrency).is_some(), "{bad}");
            assert_eq!(form.concurrency_change(), None);
        }
        form.set_concurrency(" 3 ");
        assert_eq!(form.error(SyncField::Concurrency), None);
        assert_eq!(form.concurrency_change(), Some(3));
        assert!(form.is_dirty());
        form.set_target_concurrency(3);
        assert!(!form.is_dirty());
    }

    #[test]
    fn test_new_sync_picks_a_free_name_and_the_defaults() {
        let form = SyncForm::new_sync(&["sync".to_string()]);
        let built = {
            let mut f = form;
            f.set_source("a");
            f.set_target("b");
            f.build(&[]).unwrap()
        };
        assert_eq!(built.name, "sync_2");
        assert_eq!(built.interval, Duration::from_secs(300));
        assert!(built.enabled);
    }

    #[test]
    fn test_fields_split_into_common_and_other() {
        let mut form = SyncForm::edit(&existing());
        assert_eq!(form.fields(), ["SUMMARY", "X-CUSTOM"]);
        form.toggle_field("LOCATION");
        form.toggle_field("SUMMARY");
        form.set_other_fields("x-custom, X-OTHER");
        assert_eq!(form.fields(), ["LOCATION", "X-CUSTOM", "X-OTHER"]);
        assert!(form.is_dirty());
    }

    #[test]
    fn test_errors_block_build_and_name_rules() {
        let mut form = SyncForm::edit(&existing());
        form.set_interval("soon");
        form.set_name("../evil");
        assert_eq!(
            form.error(SyncField::Interval),
            Some("Use a duration such as 5m or 1h")
        );
        assert_eq!(
            form.error(SyncField::Name),
            Some("No slashes and no leading dot")
        );
        assert!(form.build(&[]).is_err());
        form.set_interval("10m");
        form.set_name("other");
        let err = form.build(&["other".to_string()]).unwrap_err();
        assert_eq!(err, "A sync named \"other\" exists already");
        form.set_target("work");
        assert_eq!(
            form.build(&[]).unwrap_err(),
            "Source and target must differ"
        );
    }

    #[test]
    fn test_transforms_move_and_must_be_valid() {
        let mut form = SyncForm::edit(&existing());
        let a = form.insert_transform(usize::MAX, TransformAction::Skip);
        assert_eq!(
            form.build(&[]).unwrap_err(),
            "Skip events (step 1): complete condition 1"
        );
        form.transform_mut(a).set_subject(0, Subject::Event);
        let b = form.insert_transform(usize::MAX, TransformAction::SetField);
        assert_eq!((a, b), (0, 1));
        assert_eq!(
            form.build(&[]).unwrap_err(),
            "Set field (step 2): fill in Field and Value"
        );
        form.transform_mut(1).set("field", "SUMMARY");
        form.transform_mut(1).set("value", "Busy");
        assert_eq!(form.move_transform_to(1, 0), Some(0));
        assert_eq!(form.move_transform_to(0, 1), None);
        assert_eq!(
            form.transform_rows(),
            ["Set SUMMARY to \"Busy\"", "Skip events if it is all-day"]
        );
        let built = form.build(&[]).unwrap();
        assert_eq!(built.transforms.len(), 2);
        form.remove_transform(0);
        assert_eq!(form.transform_rows(), ["Skip events if it is all-day"]);
    }

    #[test]
    fn test_cross_transform_rules_of_the_library_apply() {
        let mut form = SyncForm::edit(&SyncConfig::new("s", "a", "b"));
        let i = form.insert_transform(usize::MAX, TransformAction::ReplaceField);
        for (k, v) in [("field", "SUMMARY"), ("search", "a"), ("replacement", "b")] {
            form.transform_mut(i).set(k, v);
        }
        let err = form.build(&[]).unwrap_err();
        assert!(err.contains("SUMMARY is not in fields"), "{err}");
    }

    #[test]
    fn test_field_order_of_the_config_is_not_an_edit() {
        let mut sync = SyncConfig::new("s", "a", "b");
        sync.fields = vec!["X-CUSTOM".into(), "SUMMARY".into()];
        assert!(!SyncForm::edit(&sync).is_dirty());
    }
}
