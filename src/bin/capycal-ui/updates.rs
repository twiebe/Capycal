//! Update check of the macOS app: the latest GitHub release, when to check
//! and when to offer it, the dialog that offers it and the note on General.
//! An app installed by a package manager (`InstalledBy`, set by the Homebrew
//! cask) is not updated in place: the dialog says how to update it there.

use crate::dialog::{Link, View};

const DAY: i64 = 24 * 60 * 60;
/// How often the automatic check looks.
const CHECK_EVERY: i64 = DAY;
/// How long Remind Me Later waits.
const REMIND_AFTER: i64 = 3 * DAY;

/// A release version `x.y.z`, optionally with a leading `v`. Anything else,
/// such as a pre-release suffix, is no version to update to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(u64, u64, u64);

impl Version {
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.strip_prefix('v').unwrap_or(text);
        let mut parts = text.split('.').map(|p| {
            (!p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
                .then(|| p.parse().ok())
                .flatten()
        });
        let version = Self(parts.next()??, parts.next()??, parts.next()??);
        parts.next().is_none().then_some(version)
    }
}

/// The latest release on GitHub.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// Without the `v` of the tag.
    pub version: String,
    /// The release page.
    pub page: String,
}

/// Read the answer of `GET /repos/{owner}/{repo}/releases/latest`.
pub fn parse_release(json: &str) -> Result<Release, String> {
    #[derive(serde::Deserialize)]
    struct Latest {
        tag_name: String,
        html_url: String,
    }
    let latest: Latest =
        serde_json::from_str(json).map_err(|e| format!("unexpected answer from GitHub: {e}"))?;
    let version = latest
        .tag_name
        .strip_prefix('v')
        .unwrap_or(&latest.tag_name);
    Ok(Release {
        version: version.to_string(),
        page: latest.html_url,
    })
}

/// The release is a newer version than `current` (the crate version).
pub fn newer(release: &Release, current: &str) -> bool {
    match (Version::parse(&release.version), Version::parse(current)) {
        (Some(release), Some(current)) => release > current,
        _ => false,
    }
}

/// What the app remembers, in seconds since the epoch.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UpdateState {
    /// The switch on General, once the user set it.
    pub check: Option<bool>,
    /// The package manager that installed the app, such as `homebrew`.
    pub installed_by: Option<String>,
    pub last_check: Option<i64>,
    /// Remind Me Later: no offer before this time.
    pub remind_after: Option<i64>,
    /// Skip This Version: never offer this version again.
    pub skipped: Option<String>,
}

/// An answer of the dialog that is remembered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    Skip,
    Later,
}

impl UpdateState {
    /// The automatic check runs: as set on General, else on unless a
    /// package manager installed the app.
    pub fn enabled(&self) -> bool {
        self.check.unwrap_or(self.installed_by.is_none())
    }

    /// Time for the automatic check. A last check in the future (a clock
    /// set back) does not block it.
    pub fn due(&self, now: i64) -> bool {
        self.enabled()
            && self
                .last_check
                .is_none_or(|last| now >= last + CHECK_EVERY || last > now)
    }

    /// The automatic check shows this release.
    pub fn offer(&self, release: &Release, current: &str, now: i64) -> bool {
        newer(release, current)
            && self.skipped.as_deref() != Some(&release.version)
            && self.remind_after.is_none_or(|after| now >= after)
    }

    pub fn answer(&mut self, answer: Answer, release: &Release, now: i64) {
        match answer {
            Answer::Skip => self.skipped = Some(release.version.clone()),
            Answer::Later => self.remind_after = Some(now + REMIND_AFTER),
        }
    }
}

/// The name of a package manager as `InstalledBy` names it.
pub fn manager_name(installed_by: &str) -> &str {
    match installed_by {
        "homebrew" => "Homebrew",
        other => other,
    }
}

/// The command that updates the app with this package manager.
pub fn update_command(installed_by: &str) -> Option<&'static str> {
    (installed_by == "homebrew").then_some("brew upgrade --cask capycal-app")
}

/// How far an update is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Bytes loaded and, if GitHub said, the size.
    Downloading(u64, Option<u64>),
    Checking,
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Installing,
}

/// What the app does after an input of the dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reply {
    /// Close the dialog, remembering the answer if any.
    Close(Option<Answer>),
    /// Open the release page; the dialog stays.
    ViewRelease,
    /// Start the update; the dialog shows its steps.
    Install,
    /// Stop the running update and close the dialog.
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Stage {
    Offer,
    Updating,
    Failed,
}

const OFFER_BUTTONS: &[&str] = &["Skip This Version", "Remind Me Later", "Update Now"];
const MANAGED_BUTTONS: &[&str] = &["Skip This Version", "Remind Me Later"];

pub struct UpdateDialog {
    stage: Stage,
    release: Release,
    current: String,
    managed: bool,
    view: View,
}

impl UpdateDialog {
    /// The offer of `release` to an app of version `current`.
    pub fn new(release: Release, current: &str, installed_by: Option<&str>) -> Self {
        let mut message = format!("You have Capycal {current}. ");
        match installed_by {
            Some(by) => {
                message.push_str(&format!(
                    "It was installed with {}, so update it there",
                    manager_name(by)
                ));
                match update_command(by) {
                    Some(command) => message.push_str(&format!(":\n\n{command}")),
                    None => message.push('.'),
                }
            }
            None => message.push_str(
                "Update Now downloads the new version, replaces this one and starts it. \
                 macOS may then ask again for access to your calendars and the Keychain.",
            ),
        }
        let view = View {
            heading: format!("Capycal {} is available", release.version),
            message,
            lines: Vec::new(),
            buttons: if installed_by.is_some() {
                MANAGED_BUTTONS
            } else {
                OFFER_BUTTONS
            },
            links: vec![view_release()],
            busy: false,
            destructive: false,
            progress: None,
            choice: None,
            warning: None,
            entry: None,
            last_disabled: false,
            footnote: None,
        };
        Self {
            stage: Stage::Offer,
            release,
            current: current.to_string(),
            managed: installed_by.is_some(),
            view,
        }
    }

    pub fn view(&self) -> &View {
        &self.view
    }

    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub fn release(&self) -> &Release {
        &self.release
    }

    /// A button or link of the window.
    pub fn press(&mut self, index: usize) -> Reply {
        if index == crate::dialog::FIRST_LINK {
            return Reply::ViewRelease;
        }
        match (&self.stage, index) {
            (Stage::Offer, 0) => Reply::Close(Some(Answer::Skip)),
            (Stage::Offer, 2) if !self.managed => {
                self.step(Step::Downloading(0, None));
                Reply::Install
            }
            (Stage::Offer, _) => Reply::Close(Some(Answer::Later)),
            (Stage::Updating, _) => Reply::Cancel,
            (Stage::Failed, _) => Reply::Close(None),
        }
    }

    /// The window was closed (Esc, the close button).
    pub fn closed(&self) -> Reply {
        match self.stage {
            Stage::Offer => Reply::Close(Some(Answer::Later)),
            Stage::Updating => Reply::Cancel,
            Stage::Failed => Reply::Close(None),
        }
    }

    pub fn step(&mut self, step: Step) {
        self.stage = Stage::Updating;
        let version = &self.release.version;
        let (message, progress) = match step {
            Step::Downloading(done, Some(total)) => (
                format!(
                    "Downloading Capycal {version}… {} of {}",
                    megabytes(done),
                    megabytes(total)
                ),
                Some((done as usize, total as usize)),
            ),
            Step::Downloading(..) => (format!("Downloading Capycal {version}…"), None),
            Step::Checking => ("Checking the download…".to_string(), None),
            Step::Installing => (
                "Replacing this version. Capycal then quits and starts again.".to_string(),
                None,
            ),
        };
        self.view.heading = format!("Updating to Capycal {version}");
        self.view.message = message;
        self.view.busy = progress.is_none();
        self.view.progress = progress;
        self.view.buttons = &["Cancel"];
    }

    /// The update failed; this version stays.
    pub fn failed(&mut self, error: &str) {
        self.stage = Stage::Failed;
        self.view.heading = "The update failed".to_string();
        self.view.message = format!(
            "{error}\n\nCapycal {} stays installed. You can download the new version from \
             the release page instead.",
            self.current
        );
        self.view.busy = false;
        self.view.progress = None;
        self.view.buttons = &["Close"];
    }
}

/// The link to the release page, with the GitHub logo.
fn view_release() -> Link {
    let github = &crate::sponsor::OPTIONS[0];
    Link {
        title: "View Release on GitHub",
        image: github.image,
        svg: github.svg,
    }
}

fn megabytes(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1_000_000.0)
}

/// The note below Check Now on General, after a check.
pub fn note(result: &Result<Release, String>, current: &str) -> String {
    match result {
        Ok(release) if newer(release, current) => {
            format!("Capycal {} is available.", release.version)
        }
        Ok(_) => format!("Capycal {current} is the latest version."),
        Err(e) => format!("Cannot check for updates: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialog::FIRST_LINK;

    fn release(version: &str) -> Release {
        Release {
            version: version.to_string(),
            page: format!("https://github.com/twiebe/capycal/releases/tag/v{version}"),
        }
    }

    #[test]
    fn test_versions_compare_numerically() {
        let v = |s| Version::parse(s).unwrap();
        assert!(v("0.10.0") > v("0.9.9"));
        assert!(v("1.0.0") > v("0.99.99"));
        assert_eq!(v("v0.2.0"), v("0.2.0"));
        for broken in [
            "0.2",
            "0.2.0-rc1",
            "0.2.0.1",
            "",
            "v",
            "a.b.c",
            "0..1",
            "+1.0.0",
        ] {
            assert_eq!(Version::parse(broken), None, "{broken}");
        }
        assert!(newer(&release("0.2.1"), "0.2.0"));
        assert!(!newer(&release("0.2.0"), "0.2.0"));
        assert!(!newer(&release("0.1.9"), "0.2.0"));
        assert!(!newer(&release("0.3.0-beta"), "0.2.0"));
    }

    #[test]
    fn test_parse_release() {
        let json = r#"{"tag_name": "v0.2.1", "html_url": "https://github.com/twiebe/capycal/releases/tag/v0.2.1", "draft": false, "assets": []}"#;
        assert_eq!(parse_release(json).unwrap(), release("0.2.1"));
        let error = parse_release(r#"{"message": "API rate limit exceeded"}"#).unwrap_err();
        assert!(
            error.starts_with("unexpected answer from GitHub"),
            "{error}"
        );
    }

    #[test]
    fn test_due_after_a_day_only_when_enabled() {
        let mut state = UpdateState::default();
        assert!(state.due(1000));
        state.last_check = Some(1000);
        assert!(!state.due(1000 + DAY - 1));
        assert!(state.due(1000 + DAY));
        // A clock set back does not stop the checks
        assert!(state.due(500));
        state.check = Some(false);
        assert!(!state.due(1000 + 10 * DAY));
    }

    #[test]
    fn test_default_is_off_for_homebrew_and_an_explicit_value_wins() {
        let mut state = UpdateState::default();
        assert!(state.enabled());
        state.installed_by = Some("homebrew".into());
        assert!(!state.enabled());
        assert!(!state.due(0));
        state.check = Some(true);
        assert!(state.enabled());
        state.installed_by = None;
        state.check = Some(false);
        assert!(!state.enabled());
    }

    #[test]
    fn test_later_waits_three_days_skip_only_this_version() {
        let mut state = UpdateState::default();
        let next = release("0.2.1");
        assert!(state.offer(&next, "0.2.0", 0));
        assert!(!state.offer(&next, "0.2.1", 0));
        state.answer(Answer::Later, &next, 100);
        assert!(!state.offer(&next, "0.2.0", 100 + REMIND_AFTER - 1));
        assert!(state.offer(&next, "0.2.0", 100 + REMIND_AFTER));
        state.answer(Answer::Skip, &next, 100 + REMIND_AFTER);
        assert!(!state.offer(&next, "0.2.0", 100 * DAY));
        assert!(state.offer(&release("0.2.2"), "0.2.0", 100 * DAY));
    }

    #[test]
    fn test_dialog_offers_update_unless_managed() {
        let mut dialog = UpdateDialog::new(release("0.2.1"), "0.2.0", None);
        let view = dialog.view();
        assert_eq!(view.heading, "Capycal 0.2.1 is available");
        assert!(view.message.starts_with("You have Capycal 0.2.0."));
        assert!(view.message.contains("may then ask again"));
        assert_eq!(view.buttons, OFFER_BUTTONS);
        assert_eq!(view.links[0].title, "View Release on GitHub");
        // View Release keeps the dialog
        assert_eq!(dialog.press(FIRST_LINK), Reply::ViewRelease);
        assert_eq!(dialog.closed(), Reply::Close(Some(Answer::Later)));
        assert_eq!(dialog.press(0), Reply::Close(Some(Answer::Skip)));
        assert_eq!(dialog.press(1), Reply::Close(Some(Answer::Later)));

        let mut managed = UpdateDialog::new(release("0.2.1"), "0.2.0", Some("homebrew"));
        let view = managed.view();
        assert_eq!(view.buttons, ["Skip This Version", "Remind Me Later"]);
        assert!(view.message.contains("installed with Homebrew"));
        assert!(view.message.ends_with("brew upgrade --cask capycal-app"));
        assert!(!view.message.contains("Keychain"));
        assert_eq!(managed.press(2), Reply::Close(Some(Answer::Later)));
        let other = UpdateDialog::new(release("0.2.1"), "0.2.0", Some("macports"));
        assert!(
            other
                .view()
                .message
                .ends_with("installed with macports, so update it there.")
        );
    }

    #[test]
    fn test_dialog_steps_and_failure_keep_view_release() {
        let mut dialog = UpdateDialog::new(release("0.2.1"), "0.2.0", None);
        assert_eq!(dialog.press(2), Reply::Install);
        assert!(dialog.view().busy);
        assert_eq!(dialog.view().buttons, ["Cancel"]);
        dialog.step(Step::Downloading(3_100_000, Some(12_000_000)));
        let view = dialog.view();
        assert_eq!(view.message, "Downloading Capycal 0.2.1… 3.1 MB of 12.0 MB");
        assert_eq!(view.progress, Some((3_100_000, 12_000_000)));
        assert!(!view.busy);
        dialog.step(Step::Checking);
        assert_eq!(dialog.view().progress, None);
        assert_eq!(dialog.closed(), Reply::Cancel);
        assert_eq!(dialog.press(0), Reply::Cancel);

        dialog.failed("The checksum does not match.");
        let view = dialog.view();
        assert_eq!(view.heading, "The update failed");
        assert!(view.message.starts_with("The checksum does not match."));
        assert!(view.message.contains("Capycal 0.2.0 stays installed"));
        assert_eq!(view.buttons, ["Close"]);
        assert_eq!(dialog.press(FIRST_LINK), Reply::ViewRelease);
        assert_eq!(dialog.press(0), Reply::Close(None));
        assert_eq!(dialog.closed(), Reply::Close(None));
    }

    #[test]
    fn test_note_for_general() {
        assert_eq!(
            note(&Ok(release("0.2.0")), "0.2.0"),
            "Capycal 0.2.0 is the latest version."
        );
        assert_eq!(
            note(&Ok(release("0.2.1")), "0.2.0"),
            "Capycal 0.2.1 is available."
        );
        assert_eq!(
            note(&Err("GitHub cannot be reached".into()), "0.2.0"),
            "Cannot check for updates: GitHub cannot be reached"
        );
    }
}
