//! Contents of a dialog window, shared by the dialog models (`purge`,
//! `purge_all`, `sponsor`, `keychain_notice`).

use crate::sponsor::{OPTIONS, SponsorOption};

/// Taglines for the About dialog, one picked at random each time.
pub const QUIPS: [&str; 9] = [
    "Runs in the background, judges you silently.",
    "Copies your meetings. Not your excuses.",
    "Busy (work). Probably.",
    "Knows where you are. Won't tell.",
    "Moves your meetings between calendars. Sadly not out of them.",
    "Copies when, not what.",
    "Likes to share. Just not your details.",
    "Copies your appointments, forgets their names.",
    "Professionally vague since 0.0.1.",
];

/// The description in About: what capycal is, and a tagline below.
// The Linux build of capycal-ui is a stub; capycal-gtk and macOS use it
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn about_description() -> String {
    format!("Calendar sync.\n{}", quip())
}

/// The next tagline from `QUIPS`: all of them in a random order, then all
/// again in a new order, so a tagline only comes back after every other one.
pub fn quip() -> &'static str {
    static BAG: std::sync::Mutex<Bag> = std::sync::Mutex::new(Bag {
        left: Vec::new(),
        last: None,
    });
    let mut bag = BAG.lock().unwrap_or_else(|e| e.into_inner());
    QUIPS[bag.draw(QUIPS.len())]
}

/// Indices not drawn yet in this round and the last one drawn.
struct Bag {
    left: Vec<usize>,
    last: Option<usize>,
}

impl Bag {
    fn draw(&mut self, len: usize) -> usize {
        if self.left.is_empty() {
            self.left = (0..len).collect();
            // Fisher-Yates
            for i in (1..len).rev() {
                self.left.swap(i, random() as usize % (i + 1));
            }
            // A new round does not start with the end of the last one
            if len > 1 && self.left.last() == self.last.as_ref() {
                self.left.swap(0, len - 1);
            }
        }
        let next = self.left.pop().unwrap_or(0);
        self.last = Some(next);
        next
    }
}

/// The clock is no source: macOS has only microseconds, so its nanoseconds
/// are always a multiple of 1000.
fn random() -> u64 {
    use std::hash::BuildHasher;
    // Every RandomState has new random keys
    std::collections::hash_map::RandomState::new().hash_one(0u8)
}

/// The links of the About window in the order of their buttons: title, URL
/// and the sponsor option it belongs to (for its logo).
// The Linux build of capycal-ui is a stub; capycal-gtk and macOS use it
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn about_links() -> Vec<(&'static str, String, Option<&'static SponsorOption>)> {
    let repository = env!("CARGO_PKG_REPOSITORY");
    let mut links = vec![
        ("Website", env!("CARGO_PKG_HOMEPAGE").to_string(), None),
        ("Report an Issue", format!("{repository}/issues"), None),
    ];
    links.extend(
        OPTIONS
            .iter()
            .map(|o| (o.title, o.url.to_string(), Some(o))),
    );
    links
}

/// The developer below the app name in About.
// The Linux build of capycal-ui is a stub; capycal-gtk and macOS use it
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub const ABOUT_DEVELOPER: &str = "Thomas Wiebe";

/// The license line at the bottom of About.
// The Linux build of capycal-ui is a stub; capycal-gtk and macOS use it
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub const ABOUT_LICENSE: &str = "Copyright 2026 Thomas Wiebe\nGNU GPL v3";

/// A click on Acknowledgements in About reports this as button.
// The Linux build of capycal-ui is a stub; macOS uses it
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub const ABOUT_ACKNOWLEDGEMENTS: usize = 301;

/// Title and header of the Acknowledgements window.
// The Linux build of capycal-ui is a stub; capycal-gtk and macOS use them
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub const ACKNOWLEDGEMENTS: (&str, &str) = (
    "Acknowledgements",
    "Capycal's license and the software it is built from",
);

/// The text of the Acknowledgements window: Capycal's copyright, the
/// contributors, the icons and crates it includes with their licenses, and
/// the GPL last.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn acknowledgements() -> String {
    let rule = format!("\n\n{}\n\n", "=".repeat(80));
    [
        "Capycal\nCopyright 2026 Thomas Wiebe\n\nCapycal is licensed under the GNU \
         General Public License version 3, printed at the end.",
        include_str!("../../../CONTRIBUTORS.md").trim(),
        include_str!("assets/OCTICONS-LICENSE").trim(),
        include_str!("assets/SIMPLE-ICONS-LICENSE").trim(),
        include_str!("../../../THIRD-PARTY-NOTICES.txt").trim(),
        include_str!("../../../LICENSE").trim(),
    ]
    .join(&rule)
}

/// Button index of the first link; links follow the buttons.
pub const FIRST_LINK: usize = 100;

/// Choosing an item of `View::choice` reports this plus its index as button.
pub const FIRST_CHOICE: usize = 200;

/// A change in `View::entry` reports this as button.
// The Linux build of capycal-ui is a stub; capycal-gtk and macOS use it
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub const TYPED: usize = 400;

/// A click on the description in About reports this as button.
// The Linux build of capycal-ui is a stub; macOS uses it
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub const ABOUT_DESCRIPTION: usize = 300;

/// What the window shows. The last button is the default one (Return);
/// Esc closes the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct View {
    pub heading: String,
    pub message: String,
    /// Lines after the message, each "label: text" with a bold label.
    pub lines: Vec<(String, String)>,
    pub buttons: &'static [&'static str],
    /// Buttons below the message, such as the sponsoring options. They are
    /// set when the window opens and stay.
    pub links: Vec<Link>,
    /// Show a spinner: the backend is still working.
    pub busy: bool,
    /// The last button deletes something: it is shown in red and Return
    /// picks the first button instead (Apple and GNOME HIG).
    pub destructive: bool,
    /// Show a progress bar instead of the spinner: (done, total).
    pub progress: Option<(usize, usize)>,
    /// A small, dimmed note below the buttons.
    pub footnote: Option<String>,
    /// A popup menu above the buttons, such as the syncs to remove.
    pub choice: Option<Choice>,
    /// A bold warning in a box with the warning sign, above the message.
    pub warning: Option<String>,
    /// A text field above the buttons with this placeholder; a change is
    /// reported as button `TYPED` and the window has the text.
    pub entry: Option<String>,
    /// The last button is off, such as until the right text is typed.
    pub last_disabled: bool,
}

impl View {
    /// Message and lines as runs of text, each bold or not.
    pub fn runs(&self) -> Vec<(String, bool)> {
        let mut runs = Vec::new();
        if !self.message.is_empty() {
            let gap = if self.lines.is_empty() { "" } else { "\n\n" };
            runs.push((format!("{}{gap}", self.message), false));
        }
        for (i, (label, text)) in self.lines.iter().enumerate() {
            let end = if i + 1 < self.lines.len() { "\n" } else { "" };
            runs.push((label.clone(), true));
            runs.push((format!(": {text}{end}"), false));
        }
        runs
    }

    /// How far the progress bar is, from 0 to 1.
    pub fn fraction(&self) -> Option<f64> {
        self.progress.map(|(done, total)| {
            if total == 0 {
                1.0
            } else {
                done.min(total) as f64 / total as f64
            }
        })
    }
}

/// Layout shared by both apps, in points (macOS) and pixels (GTK).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub mod layout {
    /// Space around the content of dialogs and windows.
    pub const MARGIN: f64 = 24.0;
    /// Width of a dialog, margins included.
    pub const DIALOG_WIDTH: f64 = 460.0;
    /// Default size of a window showing text (Problems, Discover).
    pub const TEXT_WINDOW: (f64, f64) = (760.0, 520.0);
    /// Default size of the log window.
    pub const LOG_WINDOW: (f64, f64) = (960.0, 600.0);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub items: Vec<String>,
    pub selected: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub title: &'static str,
    /// A monochrome PDF, drawn in the text color (macOS).
    pub image: &'static [u8],
    /// A symbolic SVG, drawn in the text color (GTK).
    pub svg: &'static [u8],
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view() -> View {
        View {
            heading: "Heading".to_string(),
            message: String::new(),
            lines: Vec::new(),
            buttons: &["Cancel", "OK"],
            links: Vec::new(),
            busy: false,
            destructive: false,
            progress: None,
            choice: None,
            warning: None,
            entry: None,
            last_disabled: false,
            footnote: None,
        }
    }

    #[test]
    fn test_runs_make_the_line_labels_bold() {
        let mut view = view();
        view.message = "Intro".to_string();
        view.lines = vec![("a".into(), "1".into()), ("b".into(), "2".into())];
        let runs = view.runs();
        let runs: Vec<(&str, bool)> = runs.iter().map(|(s, b)| (s.as_str(), *b)).collect();
        assert_eq!(
            runs,
            [
                ("Intro\n\n", false),
                ("a", true),
                (": 1\n", false),
                ("b", true),
                (": 2", false)
            ]
        );
        view.lines.clear();
        assert_eq!(view.runs(), [("Intro".to_string(), false)]);
    }

    #[test]
    fn test_quips_are_picked_at_random() {
        let picked: std::collections::HashSet<&str> = (0..100).map(|_| quip()).collect();
        assert!(picked.iter().all(|q| QUIPS.contains(q)));
        assert_eq!(picked.len(), QUIPS.len());
    }

    #[test]
    fn test_about_description_puts_the_quip_below_what_capycal_is() {
        let description = about_description();
        let (first, quip) = description.split_once('\n').unwrap();
        assert_eq!(first, "Calendar sync.");
        assert!(QUIPS.contains(&quip));
    }

    #[test]
    fn test_every_quip_comes_once_per_round_and_never_twice_in_a_row() {
        let mut bag = Bag {
            left: Vec::new(),
            last: None,
        };
        let drawn: Vec<usize> = (0..5 * 9).map(|_| bag.draw(9)).collect();
        for round in drawn.chunks(9) {
            let mut sorted = round.to_vec();
            sorted.sort();
            assert_eq!(sorted, (0..9).collect::<Vec<_>>());
        }
        assert!(drawn.windows(2).all(|w| w[0] != w[1]), "{drawn:?}");
        // Rounds differ in order
        assert!(drawn.chunks(9).any(|r| r != &drawn[..9]), "{drawn:?}");
    }

    #[test]
    fn test_acknowledgements_name_license_contributors_and_dependencies() {
        let text = acknowledgements();
        for part in [
            "Copyright 2026 Thomas Wiebe",
            "# Contributors",
            "Octicons",
            "Simple Icons",
            "Used by: ",
            "tokio ",
            "GNU GENERAL PUBLIC LICENSE",
        ] {
            assert!(text.contains(part), "{part}");
        }
        // The GPL comes last, after what it does not cover
        assert!(text.find("tokio ") < text.find("GNU GENERAL PUBLIC LICENSE"));
    }

    #[test]
    fn test_about_links_end_with_the_sponsor_options() {
        let links = about_links();
        assert_eq!(links[0].0, "Website");
        assert_eq!(links[0].1, "https://capycal.app");
        assert_eq!(links.len(), 2 + OPTIONS.len());
        assert!(links.iter().all(|(_, url, _)| url.starts_with("https://")));
        assert!(links[2].2.is_some());
    }

    #[test]
    fn test_fraction_of_the_progress() {
        let mut view = view();
        assert_eq!(view.fraction(), None);
        view.progress = Some((1, 4));
        assert_eq!(view.fraction(), Some(0.25));
        view.progress = Some((0, 0));
        assert_eq!(view.fraction(), Some(1.0));
    }
}
