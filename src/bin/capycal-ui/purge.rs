//! The "Remove Synced Events" dialog: counting, choosing the syncs and
//! confirming, removing and the result, as texts and buttons for the window.

use crate::backend::{PurgeOutcome, PurgePreview};
use crate::dialog::{Choice, FIRST_CHOICE, View};

/// What the dialog is for, shown while counting and with the question.
const PURPOSE: &str = "Removes the events Capycal copied into the target calendars: when \
    you no longer want a sync's copies, or to copy a source again from scratch. Settings \
    does this by itself when you rename or remove a sync.";

/// The note under the question, for one sync or several.
fn confirm_note(one: bool) -> String {
    let after = if one {
        "The sync is switched off afterwards; switch it on again in Settings."
    } else {
        "The syncs are switched off afterwards; switch them on again in Settings."
    };
    format!(
        "Capycal deletes only events it created and forgets what it synced. The source \
         calendars are not touched. {after}"
    )
}

/// The note under the result, for one sync or several.
fn disabled_note(one: bool) -> String {
    if one {
        "The sync is switched off. Switch it on again in Settings; it then copies its \
         source from scratch."
    } else {
        "The syncs are switched off. Switch them on again in Settings; they then copy \
         their source from scratch."
    }
    .to_string()
}

/// What the app does after a button press.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    Close,
    /// Remove the events of these syncs; the dialog shows the progress.
    Remove(Vec<String>),
    ShowLog,
    /// The view changed; show it again.
    Show,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Stage {
    Counting,
    /// `selected` is 0 for all syncs, else the index of a purgeable sync
    /// plus 1, as in the choice.
    Confirm {
        preview: PurgePreview,
        selected: usize,
    },
    Nothing,
    Removing,
    Done,
}

pub struct PurgeDialog {
    stage: Stage,
    view: View,
    /// The counted events per sync to remove.
    totals: Vec<(String, usize)>,
    /// The events removed so far per sync.
    done: Vec<(String, usize)>,
    /// The one sync to offer, when opened from its page in Settings.
    only: Option<String>,
}

impl PurgeDialog {
    pub fn counting(only: Option<String>) -> Self {
        Self {
            stage: Stage::Counting,
            view: View {
                heading: "Counting synced events".to_string(),
                message: format!(
                    "{PURPOSE}\n\ncapycal is counting them. Nothing is removed until you confirm."
                ),
                lines: Vec::new(),
                buttons: &["Cancel"],
                links: Vec::new(),
                busy: true,
                destructive: false,
                progress: None,
                choice: None,
                warning: None,
                entry: None,
                last_disabled: false,
                footnote: None,
            },
            totals: Vec::new(),
            done: Vec::new(),
            only,
        }
    }

    pub fn view(&self) -> &View {
        &self.view
    }

    pub fn counted(&mut self, preview: &PurgePreview) {
        let only;
        let preview = match &self.only {
            Some(sync) => {
                only = PurgePreview {
                    counts: (preview.counts.iter())
                        .filter(|c| c.sync == *sync)
                        .cloned()
                        .collect(),
                };
                &only
            }
            None => preview,
        };
        self.totals = preview
            .counts
            .iter()
            .filter_map(|c| Some((c.sync.clone(), *c.events.as_ref().ok()?)))
            .collect();
        if preview.purgeable().is_empty() {
            self.stage = Stage::Nothing;
            self.view = View {
                heading: "Nothing can be removed".to_string(),
                message: String::new(),
                lines: preview.lines(),
                buttons: &["Done"],
                links: Vec::new(),
                busy: false,
                destructive: false,
                progress: None,
                choice: None,
                warning: None,
                entry: None,
                last_disabled: false,
                footnote: None,
            };
        } else {
            self.confirm(preview.clone(), 0);
        }
    }

    fn confirm(&mut self, preview: PurgePreview, selected: usize) {
        let syncs = preview.purgeable();
        let chosen = chosen(&preview, selected);
        let total: usize = chosen
            .counts
            .iter()
            .filter_map(|c| c.events.as_ref().ok())
            .sum();
        // With a single sync there is nothing to choose
        let choice = (syncs.len() > 1).then(|| Choice {
            items: std::iter::once("All syncs".to_string())
                .chain(syncs)
                .collect(),
            selected,
        });
        self.view = View {
            heading: format!("Remove {}?", events(total)),
            message: PURPOSE.to_string(),
            lines: chosen.lines(),
            buttons: &["Cancel", "Remove Events"],
            links: Vec::new(),
            busy: false,
            destructive: true,
            progress: None,
            choice,
            warning: None,
            entry: None,
            last_disabled: false,
            footnote: Some(confirm_note(chosen.purgeable().len() == 1)),
        };
        self.stage = Stage::Confirm { preview, selected };
    }

    pub fn finished(&mut self, outcomes: &[PurgeOutcome]) {
        let failed = outcomes.iter().any(|o| !matches!(o.result, Ok((_, 0))));
        let lines = outcomes
            .iter()
            .map(|o| {
                let text = match &o.result {
                    Ok((deleted, 0)) => format!("{} removed", events(*deleted)),
                    Ok((deleted, errors)) => format!(
                        "{} removed, {} not removed",
                        events(*deleted),
                        events(*errors)
                    ),
                    Err(e) => format!("failed: {e}"),
                };
                (o.sync.clone(), text)
            })
            .collect();
        self.stage = Stage::Done;
        self.view = View {
            heading: if failed {
                "Some events were not removed".to_string()
            } else {
                "Synced events removed".to_string()
            },
            message: String::new(),
            lines,
            buttons: &["Show Log", "Done"],
            links: Vec::new(),
            busy: false,
            destructive: false,
            progress: None,
            choice: None,
            warning: None,
            entry: None,
            last_disabled: false,
            footnote: Some(disabled_note(outcomes.len() == 1)),
        };
    }

    /// `done` events of `sync` are removed (see `Update::PurgeProgress`).
    pub fn progress(&mut self, sync: &str, done: usize) {
        if self.stage != Stage::Removing {
            return;
        }
        // The syncs are removed one after the other: the ones before `sync`
        // are through, even if their last progress was not reported
        for (earlier, total) in self.totals.iter().take_while(|(s, _)| s != sync) {
            set(&mut self.done, earlier, *total);
        }
        set(&mut self.done, sync, done);
        self.view = self.removing_view();
    }

    fn removing_view(&self) -> View {
        let total: usize = self.totals.iter().map(|(_, n)| n).sum();
        // A sync may have more events than counted; it cannot pass its count
        let done: usize = self
            .totals
            .iter()
            .map(|(sync, n)| {
                let done = self.done.iter().find(|(s, _)| s == sync).map(|(_, d)| *d);
                done.unwrap_or(0).min(*n)
            })
            .sum();
        View {
            heading: "Removing synced events".to_string(),
            message: format!(
                "{done} of {} removed.\n\nYou can close this window, Capycal keeps removing \
                 the events. The menu shows the progress.",
                events(total)
            ),
            lines: Vec::new(),
            buttons: &["Show Log", "Close"],
            links: Vec::new(),
            busy: false,
            destructive: false,
            progress: Some((done, total)),
            choice: None,
            warning: None,
            entry: None,
            last_disabled: false,
            footnote: None,
        }
    }

    pub fn press(&mut self, button: usize) -> Reply {
        match (&self.stage, button) {
            (Stage::Confirm { preview, selected }, 1) => {
                let syncs = chosen(preview, *selected).purgeable();
                self.totals.retain(|(sync, _)| syncs.contains(sync));
                self.stage = Stage::Removing;
                self.view = self.removing_view();
                Reply::Remove(syncs)
            }
            (Stage::Confirm { preview, .. }, b)
                if b >= FIRST_CHOICE && b - FIRST_CHOICE <= preview.purgeable().len() =>
            {
                self.confirm(preview.clone(), b - FIRST_CHOICE);
                Reply::Show
            }
            (Stage::Removing | Stage::Done, 0) => Reply::ShowLog,
            _ => Reply::Close,
        }
    }
}

/// The part of `preview` for the choice `selected` (see `Stage::Confirm`).
fn chosen(preview: &PurgePreview, selected: usize) -> PurgePreview {
    let Some(sync) = selected
        .checked_sub(1)
        .and_then(|i| preview.purgeable().into_iter().nth(i))
    else {
        return preview.clone();
    };
    PurgePreview {
        counts: preview
            .counts
            .iter()
            .filter(|c| c.sync == sync)
            .cloned()
            .collect(),
    }
}

fn set(done: &mut Vec<(String, usize)>, sync: &str, n: usize) {
    match done.iter_mut().find(|(s, _)| s == sync) {
        Some(entry) => entry.1 = n,
        None => done.push((sync.to_string(), n)),
    }
}

fn events(n: usize) -> String {
    if n == 1 {
        "1 event".to_string()
    } else {
        format!("{n} events")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::PurgeCount;

    /// Message and lines as plain text.
    fn text(view: &View) -> String {
        view.runs().into_iter().map(|(s, _)| s).collect()
    }

    fn preview(counts: &[(&str, Result<usize, String>)]) -> PurgePreview {
        PurgePreview {
            counts: counts
                .iter()
                .map(|(sync, events)| PurgeCount {
                    sync: sync.to_string(),
                    target: "b".to_string(),
                    events: events.clone(),
                })
                .collect(),
        }
    }

    #[test]
    fn test_counting_can_only_be_cancelled() {
        let mut dialog = PurgeDialog::counting(None);
        assert!(dialog.view().busy);
        assert_eq!(dialog.view().buttons, ["Cancel"]);
        assert_eq!(dialog.press(0), Reply::Close);
    }

    #[test]
    fn test_confirm_removes_the_readable_syncs() {
        let mut dialog = PurgeDialog::counting(None);
        dialog.counted(&preview(&[
            ("work", Ok(3)),
            ("home", Err("offline".into())),
            ("gym", Ok(1)),
        ]));
        let view = dialog.view();
        assert_eq!(view.heading, "Remove 4 events?");
        assert_eq!(
            text(view),
            format!("{PURPOSE}\n\n")
                + "work: 3 events in calendar b\n\
             home: cannot read calendar b, skipped: offline\n\
             gym: 1 event in calendar b"
        );
        assert!(!view.busy);
        assert_eq!(view.buttons, ["Cancel", "Remove Events"]);

        let reply = dialog.press(1);
        assert_eq!(reply, Reply::Remove(vec!["work".into(), "gym".into()]));
        assert_eq!(dialog.view().heading, "Removing synced events");
        assert_eq!(dialog.view().progress, Some((0, 4)));
        assert!(dialog.view().message.starts_with("0 of 4 events removed."));
        assert_eq!(dialog.press(0), Reply::ShowLog);
        assert_eq!(dialog.press(1), Reply::Close);
    }

    #[test]
    fn test_choose_one_sync() {
        let mut dialog = PurgeDialog::counting(None);
        dialog.counted(&preview(&[
            ("work", Ok(3)),
            ("home", Err("offline".into())),
            ("gym", Ok(1)),
        ]));
        let choice = dialog.view().choice.clone().unwrap();
        assert_eq!(choice.items, ["All syncs", "work", "gym"]);
        assert_eq!(choice.selected, 0);

        assert_eq!(dialog.press(FIRST_CHOICE + 2), Reply::Show);
        let view = dialog.view();
        assert_eq!(view.heading, "Remove 1 event?");
        assert_eq!(
            text(view),
            format!("{PURPOSE}\n\ngym: 1 event in calendar b")
        );
        assert!(view.footnote.as_ref().unwrap().contains("not touched"));
        assert_eq!(view.choice.as_ref().unwrap().selected, 2);
        assert_eq!(dialog.press(1), Reply::Remove(vec!["gym".into()]));
        assert_eq!(dialog.view().progress, Some((0, 1)));
    }

    #[test]
    fn test_from_settings_only_the_one_sync() {
        let mut dialog = PurgeDialog::counting(Some("gym".to_string()));
        dialog.counted(&preview(&[("work", Ok(3)), ("gym", Ok(1))]));
        let view = dialog.view();
        assert_eq!(view.heading, "Remove 1 event?");
        assert_eq!(view.choice, None);
        assert_eq!(dialog.press(1), Reply::Remove(vec!["gym".into()]));
    }

    #[test]
    fn test_no_choice_for_a_single_sync() {
        let mut dialog = PurgeDialog::counting(None);
        dialog.counted(&preview(&[
            ("work", Ok(3)),
            ("home", Err("offline".into())),
        ]));
        assert_eq!(dialog.view().choice, None);
        assert_eq!(dialog.press(FIRST_CHOICE + 2), Reply::Close);
    }

    #[test]
    fn test_progress_sums_the_syncs_up_to_their_counts() {
        let mut dialog = PurgeDialog::counting(None);
        dialog.counted(&preview(&[("work", Ok(3)), ("gym", Ok(1))]));
        dialog.progress("work", 1);
        assert_eq!(dialog.view().progress, None, "not removing yet");
        dialog.press(1);
        dialog.progress("work", 2);
        assert_eq!(dialog.view().progress, Some((2, 4)));
        assert_eq!(dialog.view().fraction(), Some(0.5));
        dialog.progress("gym", 0);
        assert_eq!(dialog.view().progress, Some((3, 4)), "work is through");
        dialog.progress("work", 5);
        dialog.progress("gym", 1);
        assert_eq!(dialog.view().progress, Some((4, 4)));
        assert!(dialog.view().message.starts_with("4 of 4 events removed."));
        dialog.finished(&[]);
        assert_eq!(dialog.view().progress, None);
    }

    #[test]
    fn test_cancel_at_confirm_removes_nothing() {
        let mut dialog = PurgeDialog::counting(None);
        dialog.counted(&preview(&[("work", Ok(1))]));
        assert_eq!(dialog.view().heading, "Remove 1 event?");
        assert_eq!(dialog.press(0), Reply::Close);
    }

    #[test]
    fn test_nothing_to_remove() {
        let mut dialog = PurgeDialog::counting(None);
        dialog.counted(&preview(&[("work", Err("offline".into()))]));
        assert_eq!(dialog.view().heading, "Nothing can be removed");
        assert_eq!(dialog.view().buttons, ["Done"]);
        assert_eq!(dialog.press(0), Reply::Close);
    }

    #[test]
    fn test_result_lists_each_sync() {
        let mut dialog = PurgeDialog::counting(None);
        dialog.counted(&preview(&[("work", Ok(3))]));
        dialog.press(1);
        dialog.finished(&[PurgeOutcome {
            sync: "work".into(),
            result: Ok((3, 0)),
        }]);
        assert_eq!(dialog.view().heading, "Synced events removed");
        assert_eq!(text(dialog.view()), "work: 3 events removed");
        assert!(
            dialog
                .view()
                .footnote
                .as_ref()
                .unwrap()
                .contains("Switch it on again")
        );
        assert!(!dialog.view().busy);
        assert_eq!(dialog.press(0), Reply::ShowLog);

        dialog.finished(&[
            PurgeOutcome {
                sync: "work".into(),
                result: Ok((2, 1)),
            },
            PurgeOutcome {
                sync: "home".into(),
                result: Err("offline".into()),
            },
        ]);
        assert_eq!(dialog.view().heading, "Some events were not removed");
        assert_eq!(
            text(dialog.view()),
            "work: 2 events removed, 1 event not removed\nhome: failed: offline"
        );
    }
}
