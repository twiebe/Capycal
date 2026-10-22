//! The "Remove All Events" dialog: counting, a warning, a second warning
//! confirmed by typing the calendar's name, removing and the result.

use crate::dialog::View;

/// What the app does after a button press.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    Close,
    /// Remove every event of this calendar; the dialog shows the progress.
    Remove(String),
    ShowLog,
    /// The view changed; show it again.
    Show,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Counting,
    Warn,
    Confirm,
    /// Nothing to remove, or the calendar cannot be read.
    Nothing,
    Removing,
    Done,
}

pub struct PurgeAllDialog {
    /// The calendar's key in the config.
    calendar: String,
    /// How Settings calls it, which the user types to confirm.
    name: String,
    /// The syncs writing into it, switched off afterwards.
    syncs: Vec<String>,
    stage: Stage,
    total: usize,
    typed: String,
    view: View,
}

impl PurgeAllDialog {
    pub fn counting(calendar: String, name: String, syncs: Vec<String>) -> Self {
        let view = View {
            heading: format!("Counting the events in {name}"),
            message: "Nothing is removed until you confirm twice.".to_string(),
            buttons: &["Cancel"],
            busy: true,
            ..empty()
        };
        Self {
            calendar,
            name,
            syncs,
            stage: Stage::Counting,
            total: 0,
            typed: String::new(),
            view,
        }
    }

    pub fn calendar(&self) -> &str {
        &self.calendar
    }

    pub fn view(&self) -> &View {
        &self.view
    }

    /// The count of `Update::CalendarCounted`.
    pub fn counted(&mut self, events: &Result<usize, String>) {
        if self.stage != Stage::Counting {
            return;
        }
        match events {
            Ok(0) => self.nothing(
                "Nothing to remove",
                format!("{} holds no events.", self.name),
            ),
            Ok(n) => {
                self.total = *n;
                self.warn();
            }
            Err(e) => self.nothing(&format!("Cannot read {}", self.name), e.clone()),
        }
    }

    fn nothing(&mut self, heading: &str, message: String) {
        self.stage = Stage::Nothing;
        self.view = View {
            heading: heading.to_string(),
            message,
            buttons: &["Done"],
            ..empty()
        };
    }

    fn warn(&mut self) {
        self.stage = Stage::Warn;
        self.view = View {
            heading: format!("Remove every event in {}?", self.name),
            warning: Some(format!(
                "This deletes {} in {}, also what Capycal did not create: your own \
                 events, invitations, everything.",
                if self.total == 1 {
                    "the one event".to_string()
                } else {
                    format!("all {}", events(self.total))
                },
                self.name
            )),
            message: "Only use this on a calendar that holds nothing but Capycal's copies."
                .to_string(),
            lines: self.switched_off(),
            buttons: &["Cancel", "Continue…"],
            ..empty()
        };
    }

    fn confirm(&mut self) {
        self.stage = Stage::Confirm;
        self.view = View {
            heading: "This cannot be undone".to_string(),
            warning: Some(format!(
                "{} in {} {} deleted for good.",
                capitalized(&events(self.total)),
                self.name,
                if self.total == 1 { "is" } else { "are" }
            )),
            message: format!("Type {} to confirm.", self.name),
            entry: Some(self.name.clone()),
            buttons: &["Cancel", "Remove All Events"],
            destructive: true,
            last_disabled: self.typed.trim() != self.name,
            ..empty()
        };
    }

    /// The syncs writing into the calendar, as lines of the warning.
    fn switched_off(&self) -> Vec<(String, String)> {
        self.syncs
            .iter()
            .map(|s| (s.clone(), "switched off afterwards".to_string()))
            .collect()
    }

    /// The text in the field of the confirmation changed.
    pub fn typed(&mut self, text: &str) {
        if self.stage == Stage::Confirm {
            self.typed = text.to_string();
            self.view.last_disabled = self.typed.trim() != self.name;
        }
    }

    /// `done` events are removed (see `Update::CalendarPurgeProgress`).
    pub fn progress(&mut self, done: usize) {
        if self.stage == Stage::Removing {
            self.view = self.removing_view(done);
        }
    }

    fn removing_view(&self, done: usize) -> View {
        let done = done.min(self.total);
        View {
            heading: format!("Removing all events of {}", self.name),
            message: format!(
                "{done} of {} removed.\n\nYou can close this window, Capycal keeps removing \
                 the events. The menu shows the progress.",
                events(self.total)
            ),
            buttons: &["Show Log", "Close"],
            progress: Some((done, self.total)),
            ..empty()
        }
    }

    /// The result of `Update::CalendarPurged`.
    pub fn finished(&mut self, result: &Result<(usize, usize), String>) {
        self.stage = Stage::Done;
        let (heading, message) = match result {
            Ok((deleted, 0)) => (
                "All events removed".to_string(),
                format!(
                    "{} removed from {}.",
                    capitalized(&events(*deleted)),
                    self.name
                ),
            ),
            Ok((deleted, errors)) => (
                "Some events were not removed".to_string(),
                format!(
                    "{} removed, {} not removed. The log says why.",
                    capitalized(&events(*deleted)),
                    events(*errors)
                ),
            ),
            Err(e) => ("Events not removed".to_string(), e.clone()),
        };
        let footnote = match (result, self.syncs.len()) {
            (Err(_), _) | (_, 0) => None,
            (_, 1) => Some(format!(
                "{} is switched off. Switch it on again in Settings; it then copies its \
                 source from scratch.",
                self.syncs[0]
            )),
            _ => Some(format!(
                "{} are switched off. Switch them on again in Settings; they then copy \
                 their source from scratch.",
                self.syncs.join(", ")
            )),
        };
        self.view = View {
            heading,
            message,
            buttons: &["Show Log", "Done"],
            footnote,
            ..empty()
        };
    }

    pub fn press(&mut self, button: usize) -> Reply {
        match (self.stage, button) {
            (Stage::Warn, 1) => {
                self.typed.clear();
                self.confirm();
                Reply::Show
            }
            (Stage::Confirm, 1) if self.typed.trim() == self.name => {
                self.stage = Stage::Removing;
                self.view = self.removing_view(0);
                Reply::Remove(self.calendar.clone())
            }
            (Stage::Confirm, 1) => Reply::Show,
            (Stage::Removing | Stage::Done, 0) => Reply::ShowLog,
            _ => Reply::Close,
        }
    }
}

fn empty() -> View {
    View {
        heading: String::new(),
        message: String::new(),
        lines: Vec::new(),
        buttons: &[],
        links: Vec::new(),
        busy: false,
        destructive: false,
        progress: None,
        footnote: None,
        choice: None,
        warning: None,
        entry: None,
        last_disabled: false,
    }
}

fn events(n: usize) -> String {
    if n == 1 {
        "1 event".to_string()
    } else {
        format!("{n} events")
    }
}

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dialog(count: usize) -> PurgeAllDialog {
        let mut d = PurgeAllDialog::counting(
            "b".into(),
            "blockers".into(),
            vec!["work-to-blockers".into()],
        );
        d.counted(&Ok(count));
        d
    }

    #[test]
    fn test_two_warnings_and_the_name_before_anything_is_removed() {
        let mut d = PurgeAllDialog::counting(
            "b".into(),
            "blockers".into(),
            vec!["work-to-blockers".into()],
        );
        assert!(d.view().busy);
        assert_eq!(d.view().buttons, ["Cancel"]);
        assert_eq!(d.calendar(), "b");
        d.counted(&Ok(12));

        let view = d.view();
        assert_eq!(view.heading, "Remove every event in blockers?");
        assert!(
            view.warning
                .as_ref()
                .unwrap()
                .contains("all 12 events in blockers")
        );
        assert_eq!(
            view.lines,
            [("work-to-blockers".into(), "switched off afterwards".into())]
        );
        assert_eq!(view.buttons, ["Cancel", "Continue…"]);
        assert!(!view.destructive && view.entry.is_none());

        assert_eq!(d.press(1), Reply::Show);
        let view = d.view();
        assert_eq!(view.heading, "This cannot be undone");
        assert_eq!(
            view.warning.as_deref(),
            Some("12 events in blockers are deleted for good.")
        );
        let one = dialog(1);
        assert!(
            one.view()
                .warning
                .as_ref()
                .unwrap()
                .contains("the one event in")
        );
        assert_eq!(view.entry.as_deref(), Some("blockers"));
        assert!(view.destructive);
        assert!(view.last_disabled, "off until the name is typed");
        assert_eq!(d.press(1), Reply::Show, "nothing happens while it is off");

        d.typed("block");
        assert!(d.view().last_disabled);
        d.typed("blockers ");
        assert!(!d.view().last_disabled);
        assert_eq!(d.press(1), Reply::Remove("b".into()));
        assert_eq!(d.view().progress, Some((0, 12)));
    }

    #[test]
    fn test_cancel_at_each_step_removes_nothing() {
        let mut d = PurgeAllDialog::counting("b".into(), "blockers".into(), Vec::new());
        assert_eq!(d.press(0), Reply::Close);
        let mut d = dialog(3);
        assert_eq!(d.press(0), Reply::Close);
        let mut d = dialog(3);
        d.press(1);
        d.typed("blockers");
        assert_eq!(d.press(0), Reply::Close);
    }

    #[test]
    fn test_nothing_to_remove_or_unreadable() {
        let mut d = dialog(0);
        assert_eq!(d.view().heading, "Nothing to remove");
        assert_eq!(d.press(0), Reply::Close);
        let mut d = PurgeAllDialog::counting("b".into(), "blockers".into(), Vec::new());
        d.counted(&Err("offline".into()));
        assert_eq!(d.view().heading, "Cannot read blockers");
        assert_eq!(d.view().message, "offline");
    }

    #[test]
    fn test_progress_and_result_with_the_syncs_switched_off() {
        let mut d = dialog(4);
        d.progress(1);
        assert_eq!(d.view().progress, None, "not removing yet");
        d.press(1);
        d.typed("blockers");
        d.press(1);
        d.progress(9);
        assert_eq!(d.view().progress, Some((4, 4)), "not past the count");
        assert_eq!(d.press(0), Reply::ShowLog);

        d.finished(&Ok((4, 0)));
        assert_eq!(d.view().heading, "All events removed");
        assert_eq!(d.view().message, "4 events removed from blockers.");
        assert!(
            d.view()
                .footnote
                .as_ref()
                .unwrap()
                .starts_with("work-to-blockers is switched off.")
        );
        d.finished(&Ok((1, 3)));
        assert_eq!(d.view().heading, "Some events were not removed");
        assert!(
            d.view()
                .message
                .starts_with("1 event removed, 3 events not removed.")
        );
        d.finished(&Err("offline".into()));
        assert_eq!(d.view().footnote, None, "nothing was switched off");
        assert_eq!(d.press(1), Reply::Close);
    }
}
