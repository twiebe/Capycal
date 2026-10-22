//! The ways to sponsor capycal, when to ask, and the dialog that asks: 30 days
//! after the first good sync, then a year after each answer. A first "Not Now"
//! asks again after half a year, "I'd Rather Not" ends it.

use crate::backend::Snapshot;
use crate::dialog::{FIRST_LINK, Link, View};

/// A way to sponsor capycal, shown in the dialog and the About panel.
pub struct SponsorOption {
    pub title: &'static str,
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub url: &'static str,
    /// The logo as a monochrome PDF (macOS).
    pub image: &'static [u8],
    /// The logo as a symbolic SVG, recolored by GTK (Linux).
    pub svg: &'static [u8],
}

pub const OPTIONS: &[SponsorOption] = &[
    SponsorOption {
        title: "Sponsor on GitHub",
        url: "https://github.com/sponsors/twiebe",
        image: include_bytes!("assets/github-mark.pdf"),
        svg: include_bytes!("assets/github-mark-symbolic.svg"),
    },
    SponsorOption {
        title: "Support on Ko-fi",
        url: "https://ko-fi.com/thomaswiebe",
        image: include_bytes!("assets/kofi.pdf"),
        svg: include_bytes!("assets/kofi-symbolic.svg"),
    },
];

const DAY: i64 = 24 * 60 * 60;
const FIRST_ASK_AFTER: i64 = 30 * DAY;
const REMIND_AFTER: i64 = 182 * DAY;
const ASK_AGAIN_AFTER: i64 = 365 * DAY;

/// What the app remembers about asking, in seconds since the epoch.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SponsorState {
    /// The first good sync the app saw.
    pub first_seen: Option<i64>,
    /// When to ask again, once the dialog was answered.
    pub next: Option<i64>,
    /// An option or "Already Sponsoring" was chosen.
    pub sponsoring: bool,
    /// "I'd Rather Not": never ask again.
    pub never: bool,
}

/// A button of the dialog, in the order of `view().buttons`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    NotNow,
    RatherNot,
    AlreadySponsoring,
    /// The option at this index of `OPTIONS`.
    Sponsor(usize),
}

impl Answer {
    pub fn from_button(index: usize) -> Self {
        match index {
            0 => Self::RatherNot,
            1 => Self::AlreadySponsoring,
            i if i >= FIRST_LINK && i - FIRST_LINK < OPTIONS.len() => Self::Sponsor(i - FIRST_LINK),
            _ => Self::NotNow,
        }
    }
}

impl SponsorState {
    /// Remember the first good sync, then say whether to ask now.
    pub fn due(&mut self, now: i64) -> bool {
        let first_seen = *self.first_seen.get_or_insert(now);
        !self.never && now >= self.next.unwrap_or(first_seen + FIRST_ASK_AFTER)
    }

    /// The state as `key=seconds` lines, for a file (the GTK app; macOS
    /// keeps it in its user defaults).
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    pub fn to_text(self) -> String {
        let mut text = String::new();
        for (key, value) in [("first_seen", self.first_seen), ("next", self.next)] {
            if let Some(value) = value {
                text.push_str(&format!("{key}={value}\n"));
            }
        }
        for (key, set) in [("sponsoring", self.sponsoring), ("never", self.never)] {
            if set {
                text.push_str(&format!("{key}=1\n"));
            }
        }
        text
    }

    /// Read `to_text`; unknown or broken lines are ignored.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    pub fn from_text(text: &str) -> Self {
        let mut state = Self::default();
        for (key, value) in text.lines().filter_map(|l| l.split_once('=')) {
            let value = value.trim().parse::<i64>().ok();
            match key.trim() {
                "first_seen" => state.first_seen = value,
                "next" => state.next = value,
                "sponsoring" => state.sponsoring = value == Some(1),
                "never" => state.never = value == Some(1),
                _ => {}
            }
        }
        state
    }

    pub fn answer(&mut self, answer: Answer, now: i64) {
        let wait = match answer {
            Answer::RatherNot => {
                self.never = true;
                return;
            }
            Answer::NotNow => self.not_now_wait(),
            Answer::AlreadySponsoring | Answer::Sponsor(_) => {
                self.sponsoring = true;
                ASK_AGAIN_AFTER
            }
        };
        self.next = Some(now + wait);
    }

    /// How long "Not Now" waits: half a year after the first ask, else a year.
    fn not_now_wait(&self) -> i64 {
        if self.next.is_none() {
            REMIND_AFTER
        } else {
            ASK_AGAIN_AFTER
        }
    }
}

/// A good moment to ask: the config runs, nothing is going on and every
/// sync that ran succeeded.
pub fn calm(snapshot: &Snapshot) -> bool {
    snapshot.loaded
        && snapshot.problem.is_none()
        && snapshot.activity.is_none()
        && snapshot.syncs.iter().all(|s| !s.running)
        && snapshot
            .syncs
            .iter()
            .all(|s| !matches!(s.last_result, Some(Err(_))))
        && snapshot
            .syncs
            .iter()
            .any(|s| matches!(s.last_result, Some(Ok(_))))
}

pub fn view(state: &SponsorState) -> View {
    let (heading, intro) = if state.sponsoring {
        (
            "Still enjoying Capycal?",
            "Thank you for supporting Capycal! If your sponsorship was a one-time one \
            or has ended, a new one helps to keep it going.",
        )
    } else {
        (
            "Enjoying Capycal?",
            "Capycal is free and developed in spare time. If it saves you time, \
            a sponsorship helps to keep it going.",
        )
    };
    let footnote = if state.not_now_wait() == REMIND_AFTER {
        "Capycal asks about once a year. Not Now asks again in half a year, \
        I'd Rather Not never again."
    } else {
        "Capycal asks about once a year. I'd Rather Not stops it."
    };
    View {
        heading: heading.to_string(),
        message: intro.to_string(),
        lines: Vec::new(),
        buttons: &["I'd Rather Not", "Already Sponsoring", "Not Now"],
        links: OPTIONS
            .iter()
            .map(|o| Link {
                title: o.title,
                image: o.image,
                svg: o.svg,
            })
            .collect(),
        busy: false,
        destructive: false,
        progress: None,
        choice: None,
        warning: None,
        entry: None,
        last_disabled: false,
        footnote: Some(footnote.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use capycal::{CycleStats, SyncStatus};

    #[test]
    fn test_asks_30_days_after_the_first_good_sync() {
        let mut state = SponsorState::default();
        assert!(!state.due(1000));
        assert_eq!(state.first_seen, Some(1000));
        assert!(!state.due(1000 + FIRST_ASK_AFTER - 1));
        assert!(state.due(1000 + FIRST_ASK_AFTER));
    }

    #[test]
    fn test_not_now_reminds_after_half_a_year_then_yearly() {
        let mut state = SponsorState {
            first_seen: Some(0),
            ..Default::default()
        };
        let asked = FIRST_ASK_AFTER;
        state.answer(Answer::NotNow, asked);
        assert!(!state.due(asked + REMIND_AFTER - 1));
        assert!(state.due(asked + REMIND_AFTER));
        let asked = asked + REMIND_AFTER;
        state.answer(Answer::NotNow, asked);
        assert!(!state.due(asked + ASK_AGAIN_AFTER - 1));
        assert!(state.due(asked + ASK_AGAIN_AFTER));
        assert!(!state.sponsoring);
    }

    #[test]
    fn test_sponsoring_asks_again_after_a_year() {
        for answer in [Answer::Sponsor(0), Answer::AlreadySponsoring] {
            let mut state = SponsorState {
                first_seen: Some(0),
                ..Default::default()
            };
            state.answer(answer, FIRST_ASK_AFTER);
            assert!(state.sponsoring);
            assert!(!state.due(FIRST_ASK_AFTER + ASK_AGAIN_AFTER - 1));
            assert!(state.due(FIRST_ASK_AFTER + ASK_AGAIN_AFTER));
            // A later Not Now waits a year, not half a year
            state.answer(Answer::NotNow, FIRST_ASK_AFTER + ASK_AGAIN_AFTER);
            assert!(!state.due(FIRST_ASK_AFTER + ASK_AGAIN_AFTER + REMIND_AFTER));
        }
    }

    #[test]
    fn test_rather_not_ends_it() {
        let mut state = SponsorState {
            first_seen: Some(0),
            next: Some(FIRST_ASK_AFTER),
            sponsoring: true,
            never: false,
        };
        state.answer(Answer::RatherNot, FIRST_ASK_AFTER);
        assert!(state.never);
        assert!(!state.due(10 * ASK_AGAIN_AFTER));
    }

    #[test]
    fn test_view_thanks_sponsors_and_says_when_it_asks_again() {
        let mut state = SponsorState::default();
        let first = view(&state);
        assert_eq!(first.heading, "Enjoying Capycal?");
        let footnote = |view: &View| view.footnote.clone().unwrap_or_default();
        assert!(footnote(&first).contains("Not Now asks again in half a year"));
        state.answer(Answer::AlreadySponsoring, 0);
        let again = view(&state);
        assert_eq!(again.heading, "Still enjoying Capycal?");
        assert!(again.message.starts_with("Thank you"));
        assert!(!footnote(&again).contains("half a year"));
    }

    #[test]
    fn test_state_as_text() {
        let state = SponsorState {
            first_seen: Some(100),
            next: Some(200),
            sponsoring: true,
            never: true,
        };
        assert_eq!(
            state.to_text(),
            "first_seen=100\nnext=200\nsponsoring=1\nnever=1\n"
        );
        assert_eq!(SponsorState::from_text(&state.to_text()), state);
        assert_eq!(
            SponsorState::from_text("junk\nfirst_seen=x\n"),
            SponsorState::default()
        );
    }

    #[test]
    fn test_buttons_and_links_map_to_answers() {
        let view = view(&SponsorState::default());
        // Not Now is last, so Return picks it; Esc closes, which is Not Now too
        assert_eq!(
            view.buttons,
            ["I'd Rather Not", "Already Sponsoring", "Not Now"]
        );
        assert_eq!(Answer::from_button(0), Answer::RatherNot);
        assert_eq!(Answer::from_button(1), Answer::AlreadySponsoring);
        assert_eq!(Answer::from_button(2), Answer::NotNow);
        assert_eq!(view.links.len(), OPTIONS.len());
        assert_eq!(view.links[0].title, "Sponsor on GitHub");
        assert_eq!(Answer::from_button(FIRST_LINK), Answer::Sponsor(0));
        assert_eq!(view.links[1].title, "Support on Ko-fi");
        assert_eq!(OPTIONS[1].url, "https://ko-fi.com/thomaswiebe");
        assert_eq!(Answer::from_button(FIRST_LINK + 1), Answer::Sponsor(1));
        assert_eq!(
            Answer::from_button(FIRST_LINK + OPTIONS.len()),
            Answer::NotNow
        );
    }

    #[test]
    fn test_options_link_to_https_with_a_pdf_logo() {
        for option in OPTIONS {
            assert!(option.url.starts_with("https://"), "{}", option.url);
            assert!(option.image.starts_with(b"%PDF"));
            assert!(option.svg.starts_with(b"<svg"));
        }
    }

    #[test]
    fn test_calm_needs_a_good_sync_and_nothing_going_on() {
        let mut sync = SyncStatus::new("work");
        let mut snapshot = Snapshot {
            loaded: true,
            problem: None,
            activity: None,
            syncs: vec![sync.clone()],
        };
        assert!(!calm(&snapshot));
        sync.last_result = Some(Ok(CycleStats::default()));
        snapshot.syncs = vec![sync.clone()];
        assert!(calm(&snapshot));

        snapshot.activity = Some("Counting synced events".into());
        assert!(!calm(&snapshot));
        snapshot.activity = None;

        sync.running = true;
        snapshot.syncs = vec![sync.clone()];
        assert!(!calm(&snapshot));
        sync.running = false;

        let mut failed = SyncStatus::new("home");
        failed.last_result = Some(Err("offline".into()));
        snapshot.syncs = vec![sync, failed];
        assert!(!calm(&snapshot));
    }
}
