//! The Settings window as a display-free view: what it shows (`view`) and
//! what the user's input does (`input`, `update`). The macOS glue and the
//! GTK app only draw a `SettingsView` and report `SettingsInput`s.

use capycal::{CalendarConfig, Config, ConfigEditor, Credential};

use crate::account_form::AccountForm;
use crate::backend::{Command, Problem, Update};
use crate::picker::CalendarChoice;
use crate::picker::{
    CALENDAR_APP, CalendarPicker, FEED_INVALID, FEEDS, Listing, Role, feed_key, feed_url_valid,
};
use crate::settings::{Confirm, Selection, Settings};
use crate::settings::{Edit, SettingsChange};
use crate::sync_form::{COMMON_FIELDS, STRUCTURAL_NOTE, SyncField, SyncForm, VALARM_HINT};
use crate::transform_form::{
    ACTIONS, DAY_NAMES, DAYS, FieldKind, PROPERTY_SUGGESTIONS, SUBJECTS, Subject, TEMPLATES, spec,
    tests,
};

/// Stable ids of the controls; the renderer maps them to its widgets.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum FieldId {
    SidebarSync(String),
    /// The sidebar's "Duplicate" of a sync.
    DuplicateSync(String),
    /// Sample or next event in the preview.
    PreviewMode,
    SidebarAccount(String),
    /// The General entry at the top of the sidebar.
    SidebarGeneral,
    StartAtLogin,
    /// The automatic update check (macOS) and its Check Now.
    CheckUpdates,
    CheckUpdatesNow,
    LogLevel,
    OpenConfig,
    RevealConfig,
    AddSync,
    /// A new sync in the editor, without the wizard.
    AddEmptySync,
    AddAccount,
    SyncName,
    Source,
    Target,
    Interval,
    Enabled,
    Field(&'static str),
    OtherFields,
    /// A transform's row: opens or closes its editor.
    TransformRow(usize),
    TransformRowRemove(usize),
    /// The "+" between two steps: inserts a transform at this index.
    InsertTransform(usize),
    /// What the copies show: a preset of fields, or Custom.
    CopyPreset,
    /// How often a sync runs: a preset, or Custom.
    IntervalPreset,
    TransformField(&'static str),
    /// The way to sponsor at this index of `sponsor::OPTIONS`, on General.
    Sponsor(usize),
    /// What condition `n` of the open transform looks at.
    ConditionSubject(usize),
    /// The test of condition `n`, as in "contains".
    ConditionTest(usize),
    /// A text of condition `n`: "field", "value", "start_time", "end_time".
    ConditionText(usize, &'static str),
    /// A weekday toggle of condition `n` (`mon`..`sun`).
    ConditionDay(usize, &'static str),
    ConditionRemove(usize),
    ConditionMoveUp(usize),
    ConditionMoveDown(usize),
    /// Condition `n` as a whole, dragged to a gap.
    Condition(usize),
    /// How condition `i` is joined to the one before it: "and", "or".
    ConditionJoin(usize),
    AddCondition,
    RefreshCalendars,
    /// The choices of a wizard step (calendars, presets).
    WizardList,
    WizardName,
    WizardNext,
    WizardBack,
    WizardCancel,
    WizardDone,
    WizardOpenSettings,
    /// A CalDAV account added from a calendar step of the wizard.
    WizardAddAccount,
    /// A feed's address typed in the source step of the wizard.
    WizardAddFeed,
    /// The address of a feed source.
    FeedUrl,
    Advanced,
    DaysPast,
    DaysFuture,
    AllEvents,
    Concurrency,
    /// The buttons of a problem on General.
    ReloadConfig,
    ShowLog,
    /// Advanced > Troubleshooting of a saved sync.
    FullSyncNow,
    RemoveSyncedEvents,
    RemoveAllEvents,
    AccountName,
    Url,
    Username,
    Password,
    Test,
    Save,
    Revert,
    Delete,
    DeleteKeepCopies,
    Reload,
    Overwrite,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PopupItem {
    pub title: String,
    pub enabled: bool,
    /// A group title, not choosable.
    pub header: bool,
    /// A note with the item: why it is off, or what it does.
    pub hint: Option<String>,
}

/// The large image above a wizard step's title.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeroImage {
    App,
    Done,
}

/// The small icon of a choice card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChoiceIcon {
    CalendarApp,
    CalDav,
}

/// How the items of `Control::Choices` look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChoiceStyle {
    /// Bordered cards with a line below the title.
    Cards,
    /// Plain rows, grouped under header items.
    Rows,
}

/// How a copied event looks, drawn like an event in a calendar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventPreview {
    pub time: String,
    pub title: String,
    pub detail: String,
}

/// One item of `Control::Choices`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Choice {
    pub title: String,
    pub detail: String,
    pub icon: Option<ChoiceIcon>,
    /// A group title, not choosable.
    pub header: bool,
    pub enabled: bool,
    /// Why it is disabled, such as "read-only".
    pub hint: Option<String>,
    pub preview: Option<EventPreview>,
}

/// A button: id, title, enabled.
pub type ButtonSpec = (FieldId, String, bool);

/// A sidebar entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidebarItem {
    pub name: String,
    pub selected: bool,
    /// A sync that runs (accounts are always active).
    pub active: bool,
    /// Its last cycle failed or had failed events.
    pub problem: bool,
    /// The entry shown, with changes not saved yet.
    pub draft: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Control {
    /// The top of an entry: an icon, its title (a field when `name` names
    /// one), a line below and an on/off switch.
    Header {
        /// The account badge left of the title.
        icon: bool,
        title: String,
        name: Option<(FieldId, Option<String>)>,
        subtitle: String,
        switch: Option<(FieldId, bool)>,
        /// How the saved sync is doing; true for a problem.
        status: Option<(String, bool)>,
    },
    /// The controls up to `GroupEnd`, drawn in a rounded box under this
    /// title.
    Group(String),
    /// A step of the pipeline a sync runs: the controls up to `GroupEnd` in
    /// a box with `label` (From, Keep, a transform, Into) in its corner.
    Stage {
        label: String,
        /// The last step, the target.
        accent: bool,
    },
    GroupEnd,
    /// The controls up to `AnyOfEnd` in a box with `label` in its corner,
    /// inside a stage: conditions of which one is enough.
    AnyOf(String),
    AnyOfEnd,
    /// The controls of one condition up to `ConditionEnd` in a card with a
    /// drag handle. Dropped, it reports `Moved(id, slot)` of the nearest
    /// `DropMark`.
    Condition(FieldId),
    ConditionEnd,
    /// A line, shown while a condition is dragged, where it would land;
    /// none while one of the conditions in `stays` is dragged, which stays
    /// as it is there.
    DropMark {
        slot: usize,
        stays: Vec<usize>,
    },
    /// The `Text`s up to `RowEnd` side by side after `label`, each with
    /// its own label after its field.
    Row(String),
    RowEnd,
    /// The line between two steps, with a "+" popup when `id` is set.
    Joint {
        id: Option<FieldId>,
        items: Vec<PopupItem>,
    },
    /// Mutually exclusive buttons in a row, reported as `Selected`.
    Segments {
        id: FieldId,
        /// Drawn left of the buttons, like a field's label.
        label: String,
        items: Vec<String>,
        selected: Option<usize>,
        /// In the middle of the line, between the parts it joins.
        centered: bool,
    },
    /// An event of the source and the copy the sync makes of it; `mode`
    /// picks the sample (0) or the source's next event (1), reported as
    /// `Selected(PreviewMode, _)`, None when there is no source yet.
    Preview {
        mode: Option<usize>,
        source_label: String,
        source: EventPreview,
        copy_label: String,
        /// None: no copy (skipped, or the sync is not valid yet).
        copy: Option<EventPreview>,
    },
    /// A transform: its summary, opened for editing below it, a handle
    /// to drag it elsewhere (reported as `Moved`) and a button to remove it.
    TransformRow {
        index: usize,
        summary: String,
        open: bool,
    },
    /// A wizard step's centered title: an optional image, the title and a
    /// line below.
    Hero {
        image: Option<HeroImage>,
        title: String,
        text: String,
    },
    /// Where the wizard is: step `current` (from 0) of `count`.
    Steps {
        current: usize,
        count: usize,
    },
    /// One of several items, reported as `Selected(id, index)`.
    Choices {
        id: FieldId,
        style: ChoiceStyle,
        items: Vec<Choice>,
        selected: Option<usize>,
    },
    /// The bar at the bottom: buttons on the left, a line of text, buttons
    /// on the right; the last one on the right is the default.
    Footer {
        left: Vec<ButtonSpec>,
        text: String,
        /// The text says what keeps the buttons off (drawn with a
        /// warning sign).
        warning: bool,
        /// A bar next to the text: (done, total).
        progress: Option<(usize, usize)>,
        right: Vec<ButtonSpec>,
    },
    Note(String),
    Error(String),
    Text {
        id: FieldId,
        label: String,
        value: String,
        placeholder: String,
        error: Option<String>,
        secure: bool,
        /// False greys the field out.
        enabled: bool,
        /// Offered while typing; any other text is fine too.
        suggestions: Vec<String>,
    },
    Check {
        id: FieldId,
        label: String,
        on: bool,
        hint: Option<String>,
    },
    Popup {
        id: FieldId,
        label: String,
        items: Vec<PopupItem>,
        selected: Option<usize>,
        /// Buttons right of the popup.
        buttons: Vec<ButtonSpec>,
    },
    /// Id, title, enabled.
    Buttons(Vec<(FieldId, String, bool)>),
    Disclosure {
        id: FieldId,
        label: String,
        open: bool,
    },
}

impl Control {
    pub fn id(&self) -> Option<&FieldId> {
        match self {
            Control::Text { id, .. }
            | Control::Check { id, .. }
            | Control::Popup { id, .. }
            | Control::Segments { id, .. }
            | Control::Choices { id, .. }
            | Control::Header {
                name: Some((id, _)),
                ..
            }
            | Control::Disclosure { id, .. } => Some(id),
            _ => None,
        }
    }

    /// The kind and id, for comparing shapes.
    fn shape(&self) -> String {
        match self {
            Control::Hero { image, .. } => format!("hero {image:?}"),
            Control::Steps { count, .. } => format!("steps {count}"),
            Control::Choices { id, items, .. } => format!("choices {id:?} {}", items.len()),
            Control::Footer { left, right, .. } => format!(
                "footer {:?}",
                left.iter()
                    .chain(right)
                    .map(|(id, _, _)| id)
                    .collect::<Vec<_>>()
            ),
            Control::Header {
                icon, name, switch, ..
            } => format!(
                "header {icon} {:?} {:?}",
                name.as_ref().map(|n| &n.0),
                switch.as_ref().map(|s| &s.0)
            ),
            Control::Group(title) => format!("group {title}"),
            Control::Stage { label, accent } => format!("stage {label} {accent}"),
            Control::Joint { id, .. } => format!("joint {id:?}"),
            Control::GroupEnd => "group end".to_string(),
            Control::AnyOf(label) => format!("any of {label}"),
            Control::AnyOfEnd => "any of end".to_string(),
            Control::Condition(id) => format!("condition {id:?}"),
            Control::DropMark { slot, stays } => format!("drop mark {slot} {stays:?}"),
            Control::ConditionEnd => "condition end".to_string(),
            Control::Row(label) => format!("row {label}"),
            Control::RowEnd => "row end".to_string(),
            Control::Segments { id, items, .. } => format!("segments {id:?} {}", items.len()),
            Control::Preview { .. } => "preview".to_string(),
            Control::TransformRow { index, open, .. } => format!("transform {index} {open}"),
            Control::Note(_) => "note".to_string(),
            Control::Error(_) => "error".to_string(),
            Control::Buttons(buttons) => format!(
                "buttons {:?}",
                buttons.iter().map(|(id, _, _)| id).collect::<Vec<_>>()
            ),
            Control::Popup { id, buttons, .. } => format!(
                "popup {id:?} {:?}",
                buttons.iter().map(|(id, _, _)| id).collect::<Vec<_>>()
            ),
            other => format!("{:?}", other.id()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsView {
    /// The window title: "Settings", or the wizard's.
    pub title: String,
    /// Only the content, no sidebar, in a small window: the wizard.
    pub compact: bool,
    /// Changes when the shown content is replaced (another entry, Revert,
    /// a reload): the renderer then rebuilds instead of keeping a focused
    /// field's text.
    pub generation: u64,
    /// Changes when fields are moved in place (a condition moved): the
    /// renderer then also overwrites the text of a focused field, which
    /// now shows another condition.
    pub moves: u64,
    pub syncs: Vec<SidebarItem>,
    pub accounts: Vec<SidebarItem>,
    /// The General entry, above the syncs.
    pub general: SidebarItem,
    /// Small and grey at the bottom of the sidebar.
    pub version: String,
    pub banner: Option<String>,
    pub controls: Vec<Control>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SettingsInput {
    Pressed(FieldId),
    Text(FieldId, String),
    Toggled(FieldId, bool),
    /// An item of a popup, by its index in `PopupItem`s and, if the
    /// renderer knows it, the title it showed (a list that changed while
    /// the menu was open makes the index point elsewhere).
    Chose(FieldId, usize, Option<String>),
    /// A row of a list.
    Selected(FieldId, usize),
    /// The entry dragged to the gap before the row of this index (the row
    /// count for the end).
    Moved(FieldId, usize),
    /// A keyboard shortcut of the window.
    Shortcut(Shortcut),
    /// Typing in the field ended: the focus left it.
    Left(FieldId),
    CloseRequested,
    /// The user confirmed the question asked for this input.
    Confirmed(Box<SettingsInput>),
}

/// Cmd+S, Cmd+N, Cmd+Delete, Cmd+Z and Cmd+Option+Up / Down, which move
/// the open transform (Ctrl on Linux).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shortcut {
    Save,
    NewSync,
    Delete,
    Undo,
    MoveUp,
    MoveDown,
    /// Cmd+W: close the window, asking about a draft.
    Close,
    /// Cmd+Q: quit the app, asking about a draft.
    Quit,
}

#[derive(Debug)]
pub enum Reply {
    Show,
    Send(Command),
    /// Ask first; confirmed, the app sends `Confirmed(then)`, or
    /// `Confirmed(alternative)` for the alternative's button.
    Ask {
        confirm: Confirm,
        then: Box<SettingsInput>,
        alternative: Option<(&'static str, SettingsInput)>,
    },
    Close,
    /// Quit the app.
    Quit,
    /// Turn starting at login on or off.
    SetStartAtLogin(bool),
    /// Turn the automatic update check on or off.
    SetCheckUpdates(bool),
    /// Look for a new release now.
    CheckUpdatesNow,
    /// Show the config file in the file manager.
    RevealConfig,
    /// Open the Remove Synced Events dialog for this sync.
    Purge(String),
    /// Open the Remove All Events dialog for this calendar (its key, and
    /// its name as Settings shows it) and the syncs writing into it.
    PurgeCalendar {
        calendar: String,
        name: String,
        syncs: Vec<String>,
    },
    ShowLog,
    /// Open this page in the browser.
    OpenUrl(&'static str),
}

/// The update check as General shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdatesInfo {
    pub enabled: bool,
    /// A Check Now runs.
    pub checking: bool,
    /// The outcome of the last Check Now.
    pub note: Option<String>,
    /// The name of the package manager that installed the app.
    pub manager: Option<String>,
}

/// What the copies of a new sync show, the wizard's last question.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    /// Only the time, titled "Busy".
    Busy,
    Title,
    Everything,
    /// Nothing filled in, switched off until it is set up.
    Custom,
}

impl Preset {
    const ALL: [Preset; 4] = [
        Preset::Busy,
        Preset::Title,
        Preset::Everything,
        Preset::Custom,
    ];

    /// What a "Busy" copy of an event from `source` is called.
    fn busy(source: &str) -> String {
        format!("Busy ({source})")
    }

    fn choice(self, source: &str) -> Choice {
        let busy = Self::busy(source);
        let (title, detail, event, more) = match self {
            Preset::Busy => (
                "Busy blocker",
                &*format!(
                    "Only the time, titled \"{busy}\". Nobody sees what the meeting is about."
                ),
                &*busy,
                "",
            ),
            Preset::Title => (
                "Title and time",
                "The meeting's title, nothing else.",
                "Team meeting",
                "",
            ),
            Preset::Everything => (
                "Everything",
                "Title, notes, location and reminders.",
                "Team meeting",
                "Room 3, reminder 10 minutes before",
            ),
            Preset::Custom => {
                return Choice {
                    title: "Custom".to_string(),
                    detail: "Set it up yourself in the editor. The sync starts switched off, \
                             so nothing is copied until you switch it on."
                        .to_string(),
                    enabled: true,
                    ..Choice::default()
                };
            }
        };
        Choice {
            title: title.to_string(),
            detail: detail.to_string(),
            enabled: true,
            preview: Some(EventPreview {
                time: "09:00 - 10:00".to_string(),
                title: event.to_string(),
                detail: more.to_string(),
            }),
            ..Choice::default()
        }
    }

    /// `sync` with this preset; `source` names the source calendar.
    pub fn apply(self, mut sync: capycal::SyncConfig, source: &str) -> capycal::SyncConfig {
        let fields: &[&str] = match self {
            Preset::Busy | Preset::Custom => &[],
            Preset::Title => &["SUMMARY"],
            Preset::Everything => &["SUMMARY", "DESCRIPTION", "LOCATION", "VALARM"],
        };
        sync.fields = fields.iter().map(|f| f.to_string()).collect();
        if self == Preset::Busy {
            let mut busy = capycal::TransformConfig::new(capycal::TransformAction::SetField);
            busy.field = "SUMMARY".to_string();
            busy.value = Some(Self::busy(source));
            sync.transforms = vec![busy];
        }
        sync.enabled = self != Preset::Custom;
        sync
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// Calendar.app or a CalDAV account (first start on macOS).
    Start,
    Account,
    Source,
    Target,
    Copy,
    Name,
    /// The first sync was created.
    Done,
}

/// A new sync, one question per step.
struct Wizard {
    step: Step,
    /// The first sync: Cancel leaves the empty state.
    first: bool,
    account: AccountForm,
    /// The step before the account step, for Back.
    account_from: Option<Step>,
    /// The answer of the start step (0 Calendar.app, 1 CalDAV).
    start: Option<usize>,
    /// The calendar picked in the current calendar step. Kept as the
    /// calendar, not a row: rows move when listings arrive.
    picked: Option<CalendarConfig>,
    source: Option<CalendarConfig>,
    target: Option<CalendarConfig>,
    preset: Preset,
    name: String,
    /// The name last proposed, replaced when the calendars change.
    proposed: String,
    /// A save of this wizard runs.
    waiting: bool,
    /// The address typed after "Add Calendar Feed…" in the source step.
    feed: Option<String>,
}

pub const CALENDAR_ACCESS: &str = "Next, macOS asks whether Capycal may access your \
    calendars. Capycal reads the calendar you copy from and writes only the copies it makes.";

const WELCOME: &str = "Capycal copies the events of one calendar into another, for \
    example your work meetings as \"Busy\" into your family calendar. It runs in the \
    background and keeps the copies up to date.";

/// How often a sync runs, as shown and as written; Custom after them.
const INTERVAL_PRESETS: [(&str, &str); 3] = [("1 min", "1m"), ("5 min", "5m"), ("15 min", "15m")];

/// The field presets of "What the copies show", Custom after them.
const COPY_PRESETS: [(&str, &[&str]); 3] = [
    ("Only the time", &[]),
    ("Title", &["SUMMARY"]),
    (
        "Title, notes, location, reminders",
        &["SUMMARY", "DESCRIPTION", "LOCATION", "VALARM"],
    ),
];

impl Wizard {
    /// Leaving it loses something the user did.
    fn started(&self) -> bool {
        self.step != Step::Start && self.source.is_some()
            || self.step == Step::Account && self.account.is_dirty()
            || self.picked.is_some()
    }
}

/// The step indicator: a dot per step, filled up to the current one.
pub fn steps_text(current: usize, count: usize) -> String {
    (0..count)
        .map(|i| if i <= current { "\u{25cf}" } else { "\u{25cb}" })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Why `name` cannot name a sync (the rules of `Config::validate`).
fn sync_name_error(name: &str) -> Option<&'static str> {
    let name = name.trim();
    if name.is_empty() {
        Some("Name is required")
    } else if name.contains('/') || name.contains('\\') || name.starts_with('.') {
        Some("No slashes and no leading dot")
    } else {
        None
    }
}

/// A new feed calendar with this address.
fn feed_calendar(url: &str) -> CalendarConfig {
    CalendarConfig {
        account: None,
        url: None,
        eventkit_calendar: None,
        ics_url: Some(Credential::Plain(url.to_string())),
        concurrency: capycal::DEFAULT_CONCURRENCY,
    }
}

/// The key a feed not configured yet gets.
fn new_feed_key(calendar: &CalendarConfig) -> Option<String> {
    match &calendar.ics_url {
        Some(Credential::Plain(url)) => Some(feed_key(url)),
        _ => None,
    }
}

/// What an item of a calendar popup stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PopupTarget {
    None,
    Calendar(usize, usize),
    AddAccount,
    AddFeed,
}

/// The next event of a source as iCal (None: nothing ahead), or why it
/// could not be read.
type NextEvent = Result<Option<String>, String>;

pub struct SettingsScreen {
    settings: Settings,
    picker: CalendarPicker,
    transform: Option<usize>,
    advanced: bool,
    /// The field checkboxes show although the fields match a preset.
    custom_fields: bool,
    /// The interval field shows although the interval is a preset.
    custom_interval: bool,
    /// The input being handled was confirmed by the user.
    confirmed: bool,
    saving: bool,
    /// The save removes the synced events of a sync: (sync, done, total).
    removing: Option<(String, usize, usize)>,
    /// What to select once the running save went through.
    after_save: Option<Selection>,
    error: Option<String>,
    note: Option<String>,
    generation: u64,
    moves: u64,
    /// The selection a pending question was asked for; a confirmation
    /// for another selection is ignored.
    asked_for: Option<Selection>,
    /// The config changed while a save ran.
    changed_while_saving: bool,
    wizard: Option<Wizard>,
    /// The field being typed in: its mistakes show once the typing ends,
    /// not after the first key.
    typing: Option<FieldId>,
    /// The app's own settings: starting at login, the config's path.
    start_at_login: bool,
    config_path: Option<std::path::PathBuf>,
    /// The update check, shown only when set (macOS).
    updates: Option<UpdatesInfo>,
    /// How the syncs are doing, as of `now`.
    statuses: Vec<capycal::SyncStatus>,
    now: chrono::DateTime<chrono::Utc>,
    /// The sync form before each change of its steps, and the open one.
    undo: Vec<(SyncForm, Option<usize>)>,
    /// The entry shown before the assistant started, for its Cancel.
    wizard_return: Option<Selection>,
    /// The preview shows the source's next event instead of the sample.
    real_preview: bool,
    /// The next event of a source calendar (by key): None while loading.
    next_event: Option<(String, Option<NextEvent>)>,
    /// Why the config is not (fully) running, shown on General.
    problem: Option<Problem>,
    /// The config file could not be read when the window opened: only
    /// General works until it can.
    broken: bool,
}

const DISCARD: &str = "Discard changes?";
const ADD_ACCOUNT: &str = "Add Account…";
const ADD_FEED: &str = "Add Calendar Feed…";
const FEED_PLACEHOLDER: &str = "https://calendar.google.com/…/basic.ics";

/// A config error in a line: where it is and what is wrong, without the
/// file name and the source lines TOML quotes ("line 3, column 14: unclosed
/// table, expected `]`").
fn config_error(error: &str) -> String {
    let mut lines = error.lines();
    let first = lines.next().unwrap_or_default();
    let first = match first.split_once("TOML parse error at ") {
        Some((_, at)) => at,
        None => first.strip_prefix("configuration error: ").unwrap_or(first),
    };
    let quoted = |l: &str| {
        let l = l.trim();
        l.is_empty()
            || l.starts_with('|')
            || l.starts_with('^')
            || l.split_once(" |")
                .is_some_and(|(n, _)| n.chars().all(|c| c.is_ascii_digit()))
    };
    std::iter::once(first)
        .chain(lines.filter(|l| !quoted(l)))
        .collect::<Vec<_>>()
        .join(": ")
}

/// How a saved sync is doing, in a line under its name, and whether that
/// is a problem; None for a disabled one (its summary says so).
fn sync_status(
    status: &capycal::SyncStatus,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<(String, bool)> {
    if status.disabled {
        return None;
    }
    let ago = |run| crate::menu::relative_time(run, now);
    Some(if status.paused {
        ("Paused".to_string(), false)
    } else if status.running && status.full_sync.is_some() {
        ("Full sync running…".to_string(), false)
    } else if status.running {
        ("Syncing now…".to_string(), false)
    } else {
        match (&status.last_result, status.last_run) {
            (Some(Err(e)), _) => (
                format!(
                    "The last sync failed: {}",
                    e.lines().next().unwrap_or_default()
                ),
                true,
            ),
            (Some(Ok(stats)), Some(run)) if stats.errors > 0 => (
                format!(
                    "Synced {}, {} events failed (see Problems)",
                    ago(run),
                    stats.errors
                ),
                true,
            ),
            (Some(Ok(stats)), Some(run)) => {
                let changes: Vec<String> = [
                    (stats.created, "new"),
                    (stats.updated, "changed"),
                    (stats.deleted, "removed"),
                ]
                .into_iter()
                .filter(|(n, _)| *n > 0)
                .map(|(n, what)| format!("{n} {what}"))
                .collect();
                if changes.is_empty() {
                    (format!("Synced {}, all up to date", ago(run)), false)
                } else {
                    (
                        format!("Synced {}: {}", ago(run), changes.join(", ")),
                        false,
                    )
                }
            }
            _ => ("Waiting for the first sync".to_string(), false),
        }
    })
}

/// The log levels General offers, as written to `[daemon] log_level`.
const LOG_LEVELS: [&str; 5] = ["error", "warn", "info", "debug", "trace"];

/// The dialog of a question: Cancel, the alternative if any, the main
/// button last (the default, unless destructive).
pub fn question_view(confirm: &Confirm, alternative: Option<&'static str>) -> crate::dialog::View {
    crate::dialog::View {
        heading: confirm.heading.clone(),
        message: confirm.message.clone(),
        lines: Vec::new(),
        buttons: question_buttons(confirm.button, alternative),
        links: Vec::new(),
        busy: false,
        destructive: confirm.destructive,
        progress: None,
        choice: None,
        warning: None,
        entry: None,
        last_disabled: false,
        footnote: None,
    }
}

fn question_buttons(
    button: &'static str,
    alternative: Option<&'static str>,
) -> &'static [&'static str] {
    match (alternative, button) {
        (None, "Discard") => &["Cancel", "Discard"],
        (None, "Save") => &["Cancel", "Save"],
        (None, "Remove and Sync Again") => &["Cancel", "Remove and Sync Again"],
        (None, "Remove Account") => &["Cancel", "Remove Account"],
        (None, "Remove Sync") => &["Cancel", "Remove Sync"],
        (Some("Keep Copies"), "Remove Sync") => &["Cancel", "Keep Copies", "Remove Sync"],
        // A new label: leak this small slice once rather than lose the button
        (alternative, button) => {
            let mut buttons = vec!["Cancel"];
            buttons.extend(alternative);
            buttons.push(button);
            Box::leak(buttons.into_boxed_slice())
        }
    }
}

fn discard() -> Confirm {
    Confirm {
        heading: DISCARD.to_string(),
        message: "Your changes to this entry are not saved.".to_string(),
        button: "Discard",
        destructive: true,
    }
}

impl SettingsScreen {
    /// The screen and the discovery commands to send when it opens.
    pub fn new(
        config: &Config,
        editor: &ConfigEditor,
        with_calendar_app: bool,
    ) -> (Self, Vec<Command>) {
        let screen = Self {
            settings: Settings::new(config, editor),
            picker: CalendarPicker::new(config, with_calendar_app),
            transform: None,
            advanced: false,
            custom_fields: false,
            custom_interval: false,
            confirmed: false,
            saving: false,
            removing: None,
            after_save: None,
            error: None,
            note: None,
            generation: 0,
            moves: 0,
            asked_for: None,
            changed_while_saving: false,
            wizard: None,
            typing: None,
            start_at_login: false,
            config_path: None,
            updates: None,
            statuses: Vec::new(),
            now: chrono::DateTime::UNIX_EPOCH,
            undo: Vec::new(),
            wizard_return: None,
            real_preview: false,
            next_event: None,
            problem: None,
            broken: false,
        };
        let mut screen = screen;
        if screen.settings.first_start() {
            screen.start_wizard(true);
        }
        let mut commands: Vec<Command> = screen
            .picker
            .accounts_to_discover()
            .into_iter()
            .map(Command::DiscoverAccount)
            .collect();
        // On the start step, Calendar.app is only read (and its access
        // asked for) once the user picks it
        let asks_first = screen
            .wizard
            .as_ref()
            .is_some_and(|w| w.step == Step::Start);
        if with_calendar_app && !asks_first {
            commands.push(Command::ListCalendarApp);
        }
        (screen, commands)
    }

    /// Same controls of the same kinds: the renderer updates in place.
    pub fn same_shape(a: &SettingsView, b: &SettingsView) -> bool {
        a.generation == b.generation
            && a.syncs.len() == b.syncs.len()
            && a.accounts.len() == b.accounts.len()
            && a.banner.is_some() == b.banner.is_some()
            && a.controls.len() == b.controls.len()
            && a.controls
                .iter()
                .zip(&b.controls)
                .all(|(x, y)| x.shape() == y.shape())
    }

    /// Ask for the next event of the source shown, unless it is loaded or
    /// asked for already.
    fn next_event_request(&mut self) -> Option<Command> {
        if !self.real_preview || self.wizard.is_some() {
            return None;
        }
        let form = self.settings.sync_form_ref()?;
        let key = form.source().to_string();
        if key.is_empty()
            || self.next_event.as_ref().is_some_and(|(k, _)| *k == key)
            || self.settings.feed_problem().is_some()
        {
            return None;
        }
        let calendar = self.settings.calendar(&key)?.clone();
        let mut sync = capycal::SyncConfig::new("preview", &key, form.target());
        if let Ok(built) = form.build(&[]) {
            sync.eventkit_days_past = built.eventkit_days_past;
            sync.eventkit_days_future = built.eventkit_days_future;
            sync.eventkit_all_events = built.eventkit_all_events;
        }
        self.next_event = Some((key, None));
        Some(Command::NextSourceEvent { calendar, sync })
    }

    /// A shortcut does what its button would, only while that is enabled.
    fn shortcut(&mut self, shortcut: Shortcut) -> Vec<Reply> {
        let enabled = |s: &Self, wanted: FieldId| {
            s.current_view().controls.iter().any(|c| match c {
                Control::Footer { left, right, .. } => left
                    .iter()
                    .chain(right)
                    .any(|(id, _, on)| *id == wanted && *on),
                _ => false,
            })
        };
        match shortcut {
            Shortcut::Save if enabled(self, FieldId::Save) => {
                self.input(SettingsInput::Pressed(FieldId::Save))
            }
            Shortcut::Delete if enabled(self, FieldId::Delete) => {
                self.input(SettingsInput::Pressed(FieldId::Delete))
            }
            Shortcut::Close => self.input(SettingsInput::CloseRequested),
            // Quitting drops a draft: ask first
            Shortcut::Quit if self.has_draft() && !self.confirmed => vec![Reply::Ask {
                confirm: Confirm {
                    heading: "Quit and discard your changes?".to_string(),
                    message: "Your changes in Settings are not saved.".to_string(),
                    button: "Quit",
                    destructive: true,
                },
                then: Box::new(SettingsInput::Shortcut(Shortcut::Quit)),
                alternative: None,
            }],
            Shortcut::Quit => vec![Reply::Quit],
            Shortcut::NewSync if self.wizard.is_none() => {
                self.input(SettingsInput::Pressed(FieldId::AddSync))
            }
            Shortcut::MoveUp | Shortcut::MoveDown => match self.transform {
                Some(open) => {
                    let gap = match shortcut {
                        Shortcut::MoveUp if open > 0 => open - 1,
                        Shortcut::MoveDown => open + 2,
                        _ => return Vec::new(),
                    };
                    self.input(SettingsInput::Moved(FieldId::TransformRow(open), gap))
                }
                None => Vec::new(),
            },
            Shortcut::Undo => match (self.undo.pop(), self.settings.sync_form()) {
                (Some((form, open)), Some(current)) => {
                    *current = form;
                    self.transform = open;
                    self.generation += 1;
                    vec![Reply::Show]
                }
                _ => Vec::new(),
            },
            _ => Vec::new(),
        }
    }

    /// What General shows about the app: whether it starts at login and
    /// where its config is.
    pub fn set_app_info(&mut self, start_at_login: bool, config_path: &std::path::Path) {
        self.start_at_login = start_at_login;
        self.config_path = Some(config_path.to_path_buf());
    }

    /// What General shows about the update check; `None` hides it.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub fn set_updates(&mut self, updates: Option<UpdatesInfo>) {
        self.updates = updates;
    }

    /// The entry shown, to show it again when the window opens anew.
    pub fn selection(&self) -> Selection {
        self.settings.selection().clone()
    }

    /// Show the sync or account `selection` again, if it still exists and
    /// the assistant does not run.
    pub fn restore(&mut self, selection: Selection) {
        let exists = match &selection {
            Selection::Sync(name) => self.settings.sync_names().contains(name),
            Selection::Account(name) => self.settings.account_names().contains(name),
            Selection::General => true,
            _ => false,
        };
        if exists && self.wizard.is_none() {
            self.select(selection);
        }
    }

    /// Settings for a config file that cannot be read: General with the
    /// mistake and a way to open the file. It becomes the whole window once
    /// the file can be read (`Update::ConfigChanged`).
    pub fn broken(path: &std::path::Path, error: String, with_calendar_app: bool) -> Self {
        let config: Config = toml::from_str("").expect("an empty config is valid");
        let (mut screen, _) = Self::new(&config, &ConfigEditor::empty(path), with_calendar_app);
        screen.wizard = None;
        screen.select(Selection::General);
        screen.broken = true;
        screen.problem = Some(Problem::Config(error));
        screen
    }

    /// The backend's problem, if any; true when the view changed. A config
    /// that cannot be read stays a problem until the file is read again.
    pub fn set_problem(&mut self, problem: Option<&Problem>) -> bool {
        if self.broken && problem.is_none() {
            return false;
        }
        let problem = problem.filter(|p| **p != Problem::NoConfig).cloned();
        let changed = self.problem != problem;
        self.problem = problem;
        changed
    }

    /// The syncs' status from the backend; true when the view changed.
    pub fn set_statuses(
        &mut self,
        statuses: &[capycal::SyncStatus],
        now: chrono::DateTime<chrono::Utc>,
    ) -> bool {
        let before = (self.view(), self.wizard_view());
        self.statuses = statuses.to_vec();
        self.now = now;
        (self.view(), self.wizard_view()) != before
    }

    /// The saved name of the sync shown in the editor.
    fn saved_sync_name(&self) -> Option<String> {
        Some(self.settings.sync_form_ref()?.original_name()?.to_string())
    }

    /// Changes not saved yet.
    pub fn has_draft(&self) -> bool {
        self.settings.dirty()
    }

    pub fn saving(&self) -> bool {
        self.saving
    }

    /// The Settings window.
    pub fn view(&self) -> SettingsView {
        self.render(false)
    }

    /// The new-sync wizard, a small window over Settings while it runs.
    pub fn wizard_view(&self) -> Option<SettingsView> {
        self.wizard.as_ref().map(|_| self.render(true))
    }

    /// The window that takes input: the wizard while it runs.
    fn current_view(&self) -> SettingsView {
        self.wizard_view().unwrap_or_else(|| self.view())
    }

    fn render(&self, wizard: bool) -> SettingsView {
        let selection = self.settings.selection();
        let syncs = self
            .settings
            .sync_names()
            .into_iter()
            .map(|n| SidebarItem {
                selected: *selection == Selection::Sync(n.clone()),
                active: self.settings.sync_enabled(&n),
                problem: self
                    .statuses
                    .iter()
                    .find(|s| s.name == n)
                    .and_then(|s| sync_status(s, self.now))
                    .is_some_and(|(_, problem)| problem),
                draft: *selection == Selection::Sync(n.clone()) && self.settings.dirty(),
                name: n,
            })
            .collect();
        let accounts = self
            .settings
            .account_names()
            .into_iter()
            .map(|n| SidebarItem {
                selected: *selection == Selection::Account(n.clone()),
                active: true,
                problem: false,
                draft: *selection == Selection::Account(n.clone()) && self.settings.dirty(),
                name: n,
            })
            .collect();
        let mut controls = Vec::new();
        let banner = self.settings.banner().map(str::to_string);
        if banner.is_some() {
            controls.push(Control::Buttons(vec![
                (FieldId::Reload, "Reload".to_string(), true),
                (FieldId::Overwrite, "Overwrite".to_string(), true),
            ]));
        }
        if let Some(w) = self.wizard.as_ref().filter(|_| wizard) {
            self.wizard_controls(w, &mut controls);
        } else if self.settings.first_start() && self.settings.selection() == &Selection::None {
            controls.push(Control::Note("Add your first sync".to_string()));
            controls.push(Control::Buttons(vec![(
                FieldId::AddSync,
                "Add Sync".to_string(),
                true,
            )]));
        } else if let Some(form) = self.settings.sync_form_ref() {
            self.sync_controls(form, &mut controls);
        } else if self.settings.account_form_ref().is_some() {
            self.account_controls(&mut controls);
        } else if *self.settings.selection() == Selection::General {
            self.general_controls(&mut controls);
        } else {
            controls.push(Control::Note("Choose a sync or an account.".to_string()));
        }
        let title = match self.wizard.as_ref().filter(|_| wizard) {
            Some(w) if w.first => "Welcome to Capycal",
            Some(_) => "New Sync",
            None => "Settings",
        };
        // The field being typed in shows no mistake yet
        if let Some(typing) = &self.typing {
            for control in &mut controls {
                match control {
                    Control::Text { id, error, .. } if id == typing => *error = None,
                    Control::Header {
                        name: Some((id, error)),
                        ..
                    } if id == typing => *error = None,
                    _ => {}
                }
            }
        }
        SettingsView {
            title: title.to_string(),
            compact: wizard,
            generation: self.generation,
            moves: self.moves,
            syncs,
            accounts,
            general: SidebarItem {
                name: "General".to_string(),
                selected: *self.settings.selection() == Selection::General,
                active: true,
                problem: self.problem.is_some(),
                draft: false,
            },
            version: format!("Version {}", capycal::VERSION),
            banner,
            controls,
        }
    }

    fn text(
        id: FieldId,
        label: &str,
        value: &str,
        placeholder: &str,
        error: Option<&str>,
    ) -> Control {
        Control::Text {
            id,
            label: label.to_string(),
            value: value.to_string(),
            placeholder: placeholder.to_string(),
            error: error.map(str::to_string),
            secure: false,
            enabled: true,
            suggestions: Vec::new(),
        }
    }

    /// The items of a calendar popup and what each stands for.
    fn calendar_items(
        &self,
        role: Role,
        current: &str,
    ) -> (Vec<PopupItem>, Vec<PopupTarget>, Option<usize>) {
        let mut items = Vec::new();
        let mut targets = Vec::new();
        let mut selected = None;
        let item = |title: &str, enabled: bool, header: bool, hint: Option<&str>| PopupItem {
            title: title.to_string(),
            enabled,
            header,
            hint: hint.map(str::to_string),
        };
        if current.is_empty() {
            // Nothing chosen yet: say so instead of showing the first group
            selected = Some(0);
            items.push(item("Choose a calendar", false, true, None));
            targets.push(PopupTarget::None);
        }
        for (g, group) in self
            .picker
            .groups(role, Some(current))
            .into_iter()
            .enumerate()
        {
            items.push(item(&group.title, false, true, None));
            targets.push(PopupTarget::None);
            if let Some(status) = &group.status {
                items.push(item(status, false, false, None));
                targets.push(PopupTarget::None);
            }
            for (i, entry) in group.items.into_iter().enumerate() {
                if entry.selected {
                    selected = Some(items.len());
                }
                items.push(item(&entry.label, entry.enabled, false, entry.hint));
                targets.push(PopupTarget::Calendar(g, i));
            }
        }
        // A calendar picked in this form, not in the config yet: its row
        if selected.is_none()
            && let Some(calendar) = self.settings.new_calendar(current)
        {
            selected = targets.iter().position(|t| match t {
                PopupTarget::Calendar(g, i) => self.picker.choose(*g, *i).calendar == *calendar,
                _ => false,
            });
        }
        if selected.is_none() && !current.is_empty() {
            // Not listed (any more): show the key
            selected = Some(items.len());
            items.push(item(current, true, false, None));
            targets.push(PopupTarget::None);
        }
        items.push(item(ADD_ACCOUNT, true, false, None));
        targets.push(PopupTarget::AddAccount);
        if role == Role::Source {
            items.push(item(ADD_FEED, true, false, None));
            targets.push(PopupTarget::AddFeed);
        }
        (items, targets, selected)
    }

    fn sync_controls(&self, form: &SyncForm, controls: &mut Vec<Control>) {
        let label = |key: &str| self.calendar_title(key);
        let stage = |label: &str, accent: bool| Control::Stage {
            label: label.to_string(),
            accent,
        };
        controls.push(Control::Header {
            icon: false,
            title: form.text(SyncField::Name).to_string(),
            name: Some((
                FieldId::SyncName,
                form.error(SyncField::Name).map(str::to_string),
            )),
            subtitle: self.sync_summary(form),
            switch: Some((FieldId::Enabled, form.enabled())),
            status: form
                .original_name()
                .and_then(|name| self.statuses.iter().find(|s| s.name == name))
                .and_then(|status| sync_status(status, self.now)),
        });

        controls.push(stage("From", false));
        let (items, _, selected) = self.calendar_items(Role::Source, form.source());
        let refresh = || vec![(FieldId::RefreshCalendars, "Refresh".to_string(), true)];
        controls.push(Control::Popup {
            id: FieldId::Source,
            label: "Calendar".to_string(),
            items,
            selected,
            buttons: refresh(),
        });
        if let Some(feed) = self
            .settings
            .calendar(form.source())
            .and_then(|c| c.ics_url.as_ref())
        {
            controls.push(match feed {
                Credential::Plain(url) => Self::text(
                    FieldId::FeedUrl,
                    "Feed address",
                    url,
                    FEED_PLACEHOLDER,
                    self.settings.feed_error(),
                ),
                // A secret the app does not show or change
                _ => Control::Text {
                    id: FieldId::FeedUrl,
                    label: "Feed address".to_string(),
                    value: String::new(),
                    placeholder: "Set in the config file".to_string(),
                    error: None,
                    secure: false,
                    enabled: false,
                    suggestions: Vec::new(),
                },
            });
        }
        let interval = form.text(SyncField::Interval).trim();
        let preset = (!self.custom_interval)
            .then(|| INTERVAL_PRESETS.iter().position(|(_, t)| *t == interval))
            .flatten();
        let mut items: Vec<String> = INTERVAL_PRESETS
            .iter()
            .map(|(l, _)| l.to_string())
            .collect();
        items.push("Custom".to_string());
        controls.push(Control::Segments {
            id: FieldId::IntervalPreset,
            label: "Check every".to_string(),
            selected: Some(preset.unwrap_or(INTERVAL_PRESETS.len())),
            items,
            centered: false,
        });
        if preset.is_none() {
            controls.push(Self::text(
                FieldId::Interval,
                "Custom interval",
                form.text(SyncField::Interval),
                "10m, 2h, 1d",
                form.error(SyncField::Interval),
            ));
        }
        controls.push(Control::GroupEnd);

        controls.push(Control::Joint {
            id: None,
            items: Vec::new(),
        });
        controls.push(stage("Keep", false));
        let preset = (!self.custom_fields)
            .then(|| {
                COPY_PRESETS.iter().position(|(_, fields)| {
                    form.other_fields().trim().is_empty()
                        && COMMON_FIELDS
                            .iter()
                            .all(|(p, _)| form.is_selected(p) == fields.contains(p))
                })
            })
            .flatten();
        let mut items: Vec<String> = COPY_PRESETS.iter().map(|(t, _)| t.to_string()).collect();
        items.push("Custom".to_string());
        controls.push(Control::Segments {
            id: FieldId::CopyPreset,
            label: String::new(),
            selected: Some(preset.unwrap_or(COPY_PRESETS.len())),
            items,
            centered: false,
        });
        if preset.is_none() {
            controls.push(Control::Note(STRUCTURAL_NOTE.to_string()));
            for (property, hint) in COMMON_FIELDS {
                controls.push(Control::Check {
                    id: FieldId::Field(property),
                    label: format!("{hint} ({property})"),
                    on: form.is_selected(property),
                    hint: (property == "VALARM").then(|| VALARM_HINT.to_string()),
                });
            }
            controls.push(Self::text(
                FieldId::OtherFields,
                "Other fields",
                form.other_fields(),
                "X-CUSTOM, …",
                None,
            ));
        }
        controls.push(Control::GroupEnd);

        // Each transform is a step; "+" between the steps inserts one there
        let mut add = vec![PopupItem {
            title: "+".to_string(),
            enabled: false,
            header: true,
            hint: None,
        }];
        add.extend(ACTIONS.iter().map(|a| PopupItem {
            title: a.title.to_string(),
            enabled: true,
            header: false,
            hint: Some(a.description.to_string()),
        }));
        // Then common steps, filled in
        add.push(PopupItem {
            title: "Common steps".to_string(),
            enabled: false,
            header: true,
            hint: None,
        });
        add.extend(TEMPLATES.iter().map(|t| PopupItem {
            title: t.title.to_string(),
            enabled: true,
            header: false,
            hint: Some(t.description.to_string()),
        }));
        let joint = |at: usize| Control::Joint {
            id: Some(FieldId::InsertTransform(at)),
            items: add.clone(),
        };
        let rows = form.transform_rows();
        let open = self.transform.filter(|i| *i < rows.len());
        let count = rows.len();
        for (index, summary) in rows.into_iter().enumerate() {
            controls.push(joint(index));
            let title = form.transform(index).map_or("Transform", |t| t.title());
            controls.push(stage(title, false));
            controls.push(Control::TransformRow {
                index,
                summary,
                open: open == Some(index),
            });
            if open == Some(index)
                && let Some(t) = form.transform(index)
            {
                self.transform_controls(t, controls);
            }
            controls.push(Control::GroupEnd);
        }
        controls.push(joint(count));

        controls.push(stage("Into", true));
        let (items, _, selected) = self.calendar_items(Role::Target, form.target());
        controls.push(Control::Popup {
            id: FieldId::Target,
            label: "Calendar".to_string(),
            items,
            selected,
            buttons: refresh(),
        });
        controls.push(Control::GroupEnd);

        let built = form.build(&[]).ok();
        // The source's next event, once loaded, or the sample
        let next = self
            .next_event
            .as_ref()
            .filter(|(key, _)| self.real_preview && key == form.source())
            .map(|(_, result)| result);
        let event = match next {
            Some(Some(Ok(Some(ical)))) => Some(ical.as_str()),
            _ => None,
        };
        let copy = crate::preview::copy(built.as_ref(), event);
        let source = label(form.source());
        controls.push(Control::Preview {
            mode: (!form.source().is_empty()).then_some(usize::from(self.real_preview)),
            source_label: match next {
                _ if form.source().is_empty() || self.settings.feed_problem().is_some() => {
                    "A sample event".to_string()
                }
                Some(None) => format!("Reading the next event in {source}…"),
                Some(Some(Ok(None))) => format!("Nothing ahead in {source}, a sample"),
                Some(Some(Err(e))) => format!(
                    "Cannot read {source} ({}), a sample",
                    e.lines().next().unwrap_or_default()
                ),
                _ if self.real_preview => format!("Next in {source}"),
                _ => format!("A sample in {source}"),
            },
            source: crate::preview::source(event),
            copy_label: match &copy {
                crate::preview::Copy::Event(_) => format!("Copy in {}", label(form.target())),
                crate::preview::Copy::Skipped => "Not copied: a transform skips it".to_string(),
                crate::preview::Copy::Unknown => {
                    "The copy shows once the sync is complete".to_string()
                }
            },
            copy: match copy {
                crate::preview::Copy::Event(event) => Some(event),
                _ => None,
            },
        });

        controls.push(Control::Disclosure {
            id: FieldId::Advanced,
            label: "Advanced".to_string(),
            open: self.advanced,
        });
        if self.advanced {
            // Only a Calendar.app source reads a window of days
            if self.settings.is_eventkit_calendar(form.source()) {
                controls.push(Control::Group("Calendar.app events".to_string()));
                controls.push(Control::Note(
                    "Which events the sync reads from Calendar.app, counted from today."
                        .to_string(),
                ));
                controls.push(Control::Row("Read".to_string()));
                for (id, label, field, placeholder) in [
                    (
                        FieldId::DaysPast,
                        "days back",
                        SyncField::EventKitDaysPast,
                        "30",
                    ),
                    (
                        FieldId::DaysFuture,
                        "days ahead",
                        SyncField::EventKitDaysFuture,
                        "365",
                    ),
                ] {
                    // All events ignore the days
                    controls.push(Control::Text {
                        id,
                        label: label.to_string(),
                        value: form.text(field).to_string(),
                        placeholder: placeholder.to_string(),
                        error: form.error(field).map(str::to_string),
                        secure: false,
                        enabled: !form.all_events(),
                        suggestions: Vec::new(),
                    });
                }
                controls.push(Control::RowEnd);
                controls.push(Control::Check {
                    id: FieldId::AllEvents,
                    label: "All events (20 years back, 4 years ahead)".to_string(),
                    on: form.all_events(),
                    hint: None,
                });
                controls.push(Control::GroupEnd);
            }
            // Only a CalDAV target limits its writes
            if self.settings.calendar(form.target()).is_some()
                && !self.settings.is_eventkit_calendar(form.target())
            {
                controls.push(Control::Group("CalDAV target".to_string()));
                controls.push(Control::Note(format!(
                    "How many events Capycal writes to {} at the same time. Every sync \
                     writing into it shares this. Use 1 if the server answers parallel \
                     writes with errors.",
                    self.calendar_title(form.target())
                )));
                controls.push(Self::text(
                    FieldId::Concurrency,
                    "Parallel writes",
                    form.text(SyncField::Concurrency),
                    "2",
                    form.error(SyncField::Concurrency),
                ));
                controls.push(Control::GroupEnd);
            }
            if let Some(name) = form.original_name() {
                self.troubleshooting(name, controls);
            }
        }
        self.status_lines(controls);
        self.save_footer(controls, form.original_name().is_some());
    }

    /// Full sync and removing the copies of the saved sync `name`.
    fn troubleshooting(&self, name: &str, controls: &mut Vec<Control>) {
        let full_sync_running = self
            .statuses
            .iter()
            .any(|s| s.name == name && s.running && s.full_sync.is_some());
        controls.push(Control::Group("Troubleshooting".to_string()));
        controls.push(Control::Note(
            "Between full syncs, Capycal copies only what changed in the source. Once a \
             day, a full sync reads every source event, compares it with its copy and \
             fixes what differs: missing, changed, duplicated or left-over copies. Full \
             Sync Now starts one at once. Removing the synced events deletes the copies in the target and \
             switches the sync off. Removing all events empties the target, also of \
             events Capycal did not create, and switches off every sync writing into it."
                .to_string(),
        ));
        controls.push(Control::Buttons(vec![
            (
                FieldId::FullSyncNow,
                "Full Sync Now".to_string(),
                self.settings.sync_enabled(name) && !full_sync_running && !self.saving,
            ),
            (
                FieldId::RemoveSyncedEvents,
                "Remove Synced Events…".to_string(),
                !self.saving,
            ),
            (
                FieldId::RemoveAllEvents,
                "Remove All Events…".to_string(),
                !self.saving,
            ),
        ]));
        controls.push(Control::GroupEnd);
    }

    /// What the sync does, in a sentence under its name; it also says
    /// what the switch beside it means.
    fn sync_summary(&self, form: &SyncForm) -> String {
        if !form.enabled() {
            return "Turned off: copies nothing until you turn it on.".to_string();
        }
        if form.source().is_empty() || form.target().is_empty() {
            return "Choose where the events come from and where the copies go.".to_string();
        }
        let interval = form.text(SyncField::Interval).trim();
        let every = INTERVAL_PRESETS
            .iter()
            .find(|(_, t)| *t == interval)
            .map_or(interval, |(label, _)| label);
        format!(
            "Copies events from {} into {} every {every}.",
            self.calendar_title(form.source()),
            self.calendar_title(form.target())
        )
    }

    /// The fields of an opened transform.
    fn transform_controls(
        &self,
        t: &crate::transform_form::TransformForm,
        controls: &mut Vec<Control>,
    ) {
        for field in spec(&t.transform.action).fields {
            let mut text = Self::text(
                FieldId::TransformField(field.key),
                field.label,
                &t.text(field.key),
                "",
                t.error(field.key),
            );
            if field.kind == FieldKind::Property
                && let Control::Text { suggestions, .. } = &mut text
            {
                *suggestions = PROPERTY_SUGGESTIONS.iter().map(|p| p.to_string()).collect();
            }
            controls.push(text);
        }
        self.condition_controls(t, controls);
    }

    /// The conditions of an opened transform: per condition a popup with
    /// what it looks at (and Remove), its test and its texts or days.
    fn condition_controls(
        &self,
        t: &crate::transform_form::TransformForm,
        controls: &mut Vec<Control>,
    ) {
        let skip = t.transform.action == capycal::TransformAction::Skip;
        let count = t.conditions().len();
        let drops = t.drops();
        let mark = |controls: &mut Vec<Control>, gap: usize, or_before: bool, or_after: bool| {
            let drop = crate::transform_form::ConditionDrop {
                gap,
                or_before,
                or_after,
            };
            if let Some(slot) = drops.iter().position(|d| *d == drop) {
                let stays = (0..count)
                    .filter(|from| {
                        let mut moved = t.clone();
                        moved.move_condition(*from, drop);
                        moved.transform.when == t.transform.when
                    })
                    .collect();
                controls.push(Control::DropMark { slot, stays });
            }
        };
        for i in 0..count {
            let subject = t.subject(i);
            let or = t.joined_by_or(i);
            // Between two conditions, or above the first
            mark(controls, i, or, or);
            if i > 0 {
                controls.push(Control::Segments {
                    id: FieldId::ConditionJoin(i),
                    label: String::new(),
                    items: vec!["and".to_string(), "or".to_string()],
                    selected: Some(usize::from(or)),
                    centered: true,
                });
            }
            // Conditions joined by "or" share a box
            if !or && t.joined_by_or(i + 1) {
                controls.push(Control::AnyOf("One of these".to_string()));
                mark(controls, i, false, true);
            }
            controls.push(Control::Condition(FieldId::Condition(i)));
            let lead = match (i, skip) {
                (0, true) => "If",
                (0, false) => "Only if",
                _ => "if",
            };
            // A skip keeps its last condition
            let removable = !skip || count > 1;
            let mut buttons = Vec::new();
            if i > 0 {
                buttons.push((FieldId::ConditionMoveUp(i), "Move Up".to_string(), true));
            }
            if i + 1 < count {
                buttons.push((FieldId::ConditionMoveDown(i), "Move Down".to_string(), true));
            }
            if removable {
                buttons.push((FieldId::ConditionRemove(i), "Remove".to_string(), true));
            }
            controls.push(Control::Popup {
                id: FieldId::ConditionSubject(i),
                label: lead.to_string(),
                items: SUBJECTS
                    .iter()
                    .map(|(_, label)| PopupItem {
                        title: label.to_string(),
                        enabled: true,
                        header: false,
                        hint: None,
                    })
                    .collect(),
                selected: SUBJECTS.iter().position(|(s, _)| *s == subject),
                buttons,
            });
            if subject == Subject::OtherField {
                let mut text = Self::text(
                    FieldId::ConditionText(i, "field"),
                    "Field",
                    &t.condition_text(i, "field"),
                    "X-CUSTOM",
                    t.condition_error(i, "field"),
                );
                if let Control::Text { suggestions, .. } = &mut text {
                    *suggestions = PROPERTY_SUGGESTIONS.iter().map(|p| p.to_string()).collect();
                }
                controls.push(text);
            }
            let test = t.test(i);
            controls.push(Control::Popup {
                id: FieldId::ConditionTest(i),
                label: "Test".to_string(),
                items: tests(subject)
                    .iter()
                    .map(|title| PopupItem {
                        title: title.to_string(),
                        enabled: true,
                        header: false,
                        hint: None,
                    })
                    .collect(),
                selected: Some(test),
                buttons: Vec::new(),
            });
            match subject {
                Subject::Start => {
                    for (day, name) in DAYS.iter().zip(DAY_NAMES) {
                        let days = t.conditions()[i].days.as_deref().unwrap_or_default();
                        controls.push(Control::Check {
                            id: FieldId::ConditionDay(i, day),
                            label: name.to_string(),
                            on: days.iter().any(|d| d == day),
                            hint: None,
                        });
                    }
                    for (key, label) in [("start_time", "From"), ("end_time", "Until")] {
                        controls.push(Self::text(
                            FieldId::ConditionText(i, key),
                            label,
                            &t.condition_text(i, key),
                            "HH:MM",
                            t.condition_error(i, key),
                        ));
                    }
                }
                Subject::Event => {}
                _ if test < 4 => controls.push(Self::text(
                    FieldId::ConditionText(i, "value"),
                    if test < 2 { "Text" } else { "Regex" },
                    &t.condition_text(i, "value"),
                    "",
                    t.condition_error(i, "value"),
                )),
                _ => {}
            }
            controls.push(Control::ConditionEnd);
            if or && !t.joined_by_or(i + 1) {
                mark(controls, i + 1, true, false);
                controls.push(Control::AnyOfEnd);
            }
        }
        if count > 0 {
            mark(controls, count, false, false);
        }
        controls.push(Control::Buttons(vec![(
            FieldId::AddCondition,
            "Add Condition".to_string(),
            true,
        )]));
    }

    /// The bar with Revert and Save.
    /// The bar with Delete on the left (for an entry in the config), Revert
    /// and Save on the right.
    fn save_footer(&self, controls: &mut Vec<Control>, deletable: bool) {
        let dirty = self.settings.dirty();
        let problem = self.settings.problem();
        let can_save = dirty && !self.saving && problem.is_none();
        // What keeps Save off (once the typing ends), else what is going on
        let problem = problem.filter(|_| self.typing.is_none());
        // A full sync of the shown sync, for example after a save changed it
        let full_sync = self.saved_sync_name().and_then(|name| {
            self.statuses
                .iter()
                .find(|s| s.name == name && s.running)
                .and_then(|s| s.full_sync)
                .map(|p| (name, p))
        });
        let mut progress = self
            .removing
            .as_ref()
            .filter(|_| self.saving)
            .map(|(_, done, total)| (*done, *total));
        let (text, warning) = match (problem.or_else(|| self.error.clone()), self.saving) {
            (_, true) => match &self.removing {
                Some((sync, done, total)) => (
                    format!("Removing the synced events of \"{sync}\"… {done} of {total}"),
                    false,
                ),
                None => ("Saving…".to_string(), false),
            },
            (Some(problem), false) => (problem, true),
            (None, false) if dirty => ("Unsaved changes".to_string(), false),
            (None, false) => match full_sync {
                Some((
                    name,
                    capycal::FullSyncProgress {
                        done,
                        total: Some(total),
                    },
                )) => {
                    let done = done.min(total);
                    progress = Some((done, total));
                    (
                        format!("Full sync of \"{name}\"… {done} of {total} changes"),
                        false,
                    )
                }
                Some((name, _)) => (
                    format!("Full sync of \"{name}\": reading the events…"),
                    false,
                ),
                None => (String::new(), false),
            },
        };
        controls.push(Control::Footer {
            left: vec![(
                FieldId::Delete,
                "Delete".to_string(),
                deletable && !self.saving,
            )],
            text,
            warning,
            progress,
            right: vec![
                (FieldId::Revert, "Revert".to_string(), dirty && !self.saving),
                (FieldId::Save, "Save".to_string(), can_save),
            ],
        });
    }

    /// How a configured calendar is called in the sync's header.
    fn calendar_title(&self, key: &str) -> String {
        let (items, _, selected) = self.calendar_items(Role::Source, key);
        selected
            .and_then(|i| items.get(i))
            .map(|item| item.title.clone())
            .filter(|t| !key.is_empty() && !t.is_empty())
            .unwrap_or_else(|| {
                if key.is_empty() {
                    "Choose a calendar".to_string()
                } else {
                    key.to_string()
                }
            })
    }

    /// The app itself: how it starts, how much it logs, its config file.
    fn general_controls(&self, controls: &mut Vec<Control>) {
        controls.push(Control::Header {
            icon: false,
            title: "General".to_string(),
            name: None,
            subtitle: "How Capycal starts, how much it logs and where its config lives."
                .to_string(),
            switch: None,
            status: None,
        });
        if let Some(problem) = &self.problem {
            self.problem_controls(problem, controls);
        }
        controls.push(Control::Group("Startup".to_string()));
        controls.push(Control::Check {
            id: FieldId::StartAtLogin,
            label: "Start Capycal at login".to_string(),
            on: self.start_at_login,
            hint: None,
        });
        controls.push(Control::Note(
            "Puts Capycal in the menu bar when you log in, so the syncs run without opening it."
                .to_string(),
        ));
        controls.push(Control::GroupEnd);

        if let Some(updates) = &self.updates {
            controls.push(Control::Group("Updates".to_string()));
            controls.push(Control::Check {
                id: FieldId::CheckUpdates,
                label: "Check for updates automatically".to_string(),
                on: updates.enabled,
                hint: None,
            });
            let note = match (&updates.note, &updates.manager) {
                _ if updates.checking => "Checking for updates\u{2026}".to_string(),
                (Some(note), _) => note.clone(),
                _ if updates.enabled => {
                    "Capycal looks for a new release on GitHub once a day.".to_string()
                }
                (None, Some(manager)) => {
                    format!("Installed with {manager}, which keeps Capycal up to date.")
                }
                (None, None) => {
                    "Capycal looks for a new release only when you press Check Now.".to_string()
                }
            };
            controls.push(Control::Note(note));
            controls.push(Control::Buttons(vec![(
                FieldId::CheckUpdatesNow,
                "Check Now".to_string(),
                !updates.checking,
            )]));
            controls.push(Control::GroupEnd);
        }

        controls.push(Control::Group("Log".to_string()));
        let current = self.settings.log_level();
        let mut items: Vec<PopupItem> = LOG_LEVELS
            .iter()
            .map(|level| PopupItem {
                title: format!("{}{}", level[..1].to_uppercase(), &level[1..]),
                enabled: true,
                header: false,
                hint: None,
            })
            .collect();
        let mut selected = LOG_LEVELS.iter().position(|l| *l == current);
        // A filter written by hand ("info,capycal=debug") stays shown
        if selected.is_none() {
            selected = Some(items.len());
            items.push(PopupItem {
                title: format!("Custom: {current}"),
                enabled: false,
                header: false,
                hint: None,
            });
        }
        controls.push(Control::Popup {
            id: FieldId::LogLevel,
            label: "Log level".to_string(),
            items,
            selected,
            buttons: Vec::new(),
        });
        controls.push(Control::Note(
            "Info notes each sync; Debug and Trace add detail for finding a problem.".to_string(),
        ));
        controls.push(Control::Buttons(vec![(
            FieldId::ShowLog,
            "Show Log".to_string(),
            true,
        )]));
        controls.push(Control::GroupEnd);

        controls.push(Control::Group("Config file".to_string()));
        let path = self
            .config_path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        controls.push(Control::Note(path));
        let reveal = if cfg!(target_os = "macos") {
            "Show in Finder"
        } else {
            "Show in Files"
        };
        controls.push(Control::Buttons(vec![
            (
                FieldId::OpenConfig,
                "Open in Editor\u{2026}".to_string(),
                true,
            ),
            (FieldId::RevealConfig, reveal.to_string(), true),
        ]));
        controls.push(Control::Note(
            "Settings writes this file, and you can edit it by hand: Capycal reloads it when it \
             changes."
                .to_string(),
        ));
        controls.push(Control::GroupEnd);

        controls.push(Control::Group("Support Capycal".to_string()));
        controls.push(Control::Note(
            "Capycal is free and developed in spare time. If it saves you time, a \
             sponsorship helps to keep it going."
                .to_string(),
        ));
        controls.push(Control::Buttons(
            crate::sponsor::OPTIONS
                .iter()
                .enumerate()
                .map(|(i, o)| (FieldId::Sponsor(i), o.title.to_string(), true))
                .collect(),
        ));
        controls.push(Control::GroupEnd);
        controls.push(Control::Error(self.error.clone().unwrap_or_default()));
    }

    /// What is wrong, at the top of General, with the way out.
    fn problem_controls(&self, problem: &Problem, controls: &mut Vec<Control>) {
        let (text, button) = match problem {
            Problem::NoConfig => return,
            Problem::Config(e) => (
                format!(
                    "Capycal cannot load the config file. Fix it and save it, Capycal reloads \
                     it by itself; syncs that ran before keep running as they were.\n\n{}",
                    config_error(e)
                ),
                (FieldId::OpenConfig, "Open in Editor\u{2026}"),
            ),
            Problem::Locked { pid } => (
                format!(
                    "Another Capycal process (pid {pid}) runs this config, so this one does not \
                     sync. Quit the other one, then reload the config."
                ),
                (FieldId::ReloadConfig, "Reload Config"),
            ),
            Problem::Purge(e) => (
                format!("Removing synced events failed: {e}"),
                (FieldId::ShowLog, "Show Log"),
            ),
        };
        controls.push(Control::Group("Needs attention".to_string()));
        controls.push(Control::Error(text));
        controls.push(Control::Buttons(vec![(
            button.0,
            button.1.to_string(),
            true,
        )]));
        controls.push(Control::GroupEnd);
    }

    fn account_controls(&self, controls: &mut Vec<Control>) {
        let Some(form) = self.settings.account_form_ref() else {
            return;
        };
        controls.push(Control::Header {
            icon: true,
            title: if form.name().is_empty() {
                "New Account".to_string()
            } else {
                form.name().to_string()
            },
            name: None,
            subtitle: "CalDAV account".to_string(),
            switch: None,
            status: None,
        });
        controls.push(Control::Group("Connection".to_string()));
        controls.push(Self::text(
            FieldId::AccountName,
            "Name",
            form.name(),
            "",
            None,
        ));
        controls.push(Self::text(
            FieldId::Url,
            "Server URL",
            form.url(),
            "https://cloud.example.org",
            None,
        ));
        controls.push(Self::text(
            FieldId::Username,
            "User name",
            form.username(),
            "",
            None,
        ));
        let placeholder = match (form.original_name(), form.managed_externally()) {
            (None, _) => String::new(),
            (Some(_), Some(source)) => format!("From the {source}, type to replace"),
            (Some(_), None) => "Stored in the keychain".to_string(),
        };
        controls.push(Control::Text {
            id: FieldId::Password,
            label: "Password".to_string(),
            value: form.password_text().to_string(),
            placeholder,
            error: None,
            secure: true,
            enabled: true,
            suggestions: Vec::new(),
        });
        controls.push(Control::Buttons(vec![(
            FieldId::Test,
            "Test Connection".to_string(),
            !self.saving && !form.url().is_empty(),
        )]));
        controls.push(Control::GroupEnd);
        let used = form
            .original_name()
            .map(|n| self.settings.syncs_using(n))
            .unwrap_or_default();
        controls.push(Control::Note(if used.is_empty() {
            "Used by no sync.".to_string()
        } else {
            format!("Used by {}.", used.join(", "))
        }));
        self.status_lines(controls);
        self.save_footer(controls, form.original_name().is_some());
    }

    /// Always the same three lines, empty when there is nothing to say, so
    /// a message that comes or goes keeps the shape (and the focus).
    /// A note such as a connection test's result; problems and saving
    /// show in the footer.
    fn status_lines(&self, controls: &mut Vec<Control>) {
        controls.push(Control::Note(self.note.clone().unwrap_or_default()));
    }

    pub fn input(&mut self, input: SettingsInput) -> Vec<Reply> {
        if matches!(
            input,
            SettingsInput::Pressed(FieldId::ConditionMoveUp(_) | FieldId::ConditionMoveDown(_))
                | SettingsInput::Moved(FieldId::Condition(_), _)
        ) {
            self.moves += 1;
        }
        // Nothing may write the empty config of a file that cannot be read
        if self.broken
            && !matches!(
                input,
                SettingsInput::Pressed(
                    FieldId::OpenConfig
                        | FieldId::RevealConfig
                        | FieldId::ReloadConfig
                        | FieldId::ShowLog
                        | FieldId::SidebarGeneral
                        | FieldId::CheckUpdatesNow
                ) | SettingsInput::Toggled(FieldId::StartAtLogin | FieldId::CheckUpdates, _)
                    | SettingsInput::CloseRequested
            )
        {
            return vec![Reply::Show];
        }
        self.typing = match &input {
            SettingsInput::Text(id, _) => Some(id.clone()),
            SettingsInput::Left(id) => self.typing.take().filter(|t| t != id),
            _ => None,
        };
        if matches!(input, SettingsInput::Left(_)) {
            // A changed feed address is read again once typed
            if input == SettingsInput::Left(FieldId::FeedUrl) {
                self.next_event = None;
                if let Some(command) = self.next_event_request() {
                    return vec![Reply::Send(command), Reply::Show];
                }
            }
            return vec![Reply::Show];
        }
        if let SettingsInput::Shortcut(shortcut) = input {
            let replies = self.shortcut(shortcut);
            if replies.iter().any(|r| matches!(r, Reply::Ask { .. })) {
                self.asked_for = Some(self.settings.selection().clone());
            }
            return replies;
        }
        if let SettingsInput::Selected(FieldId::PreviewMode, mode) = input {
            // Choosing the next event reads it again
            self.real_preview = mode == 1;
            self.next_event = None;
        }
        let mut replies = self.handle(input);
        // The real preview follows the source
        if let Some(command) = self.next_event_request() {
            replies.push(Reply::Send(command));
            if !replies.iter().any(|r| matches!(r, Reply::Show)) {
                replies.push(Reply::Show);
            }
        }
        if replies.iter().any(|r| matches!(r, Reply::Ask { .. })) {
            self.asked_for = Some(self.settings.selection().clone());
        }
        replies
    }

    fn handle(&mut self, input: SettingsInput) -> Vec<Reply> {
        if let SettingsInput::Confirmed(inner) = input {
            // Asked about another entry than the one shown now
            if self.asked_for.take().as_ref() != Some(self.settings.selection()) {
                return vec![Reply::Show];
            }
            self.confirmed = true;
            let replies = match *inner {
                SettingsInput::Shortcut(shortcut) => self.shortcut(shortcut),
                inner => self.handle(inner),
            };
            self.confirmed = false;
            return replies;
        }
        if let Some(replies) = self.wizard_input(&input) {
            return replies;
        }
        let ask_discard = |input: SettingsInput| {
            vec![
                Reply::Ask {
                    confirm: discard(),
                    then: Box::new(input),
                    alternative: None,
                },
                Reply::Show,
            ]
        };
        let selecting = match &input {
            SettingsInput::Pressed(FieldId::SidebarSync(n)) => Some(Selection::Sync(n.clone())),
            SettingsInput::Pressed(FieldId::SidebarAccount(n)) => {
                Some(Selection::Account(n.clone()))
            }
            SettingsInput::Pressed(
                FieldId::AddSync | FieldId::AddEmptySync | FieldId::DuplicateSync(_),
            ) => Some(Selection::NewSync),
            SettingsInput::Pressed(FieldId::AddAccount) => Some(Selection::NewAccount),
            SettingsInput::Pressed(FieldId::SidebarGeneral) => Some(Selection::General),
            _ => None,
        };
        if let Some(selection) = selecting {
            let wizard_started = self.wizard.as_ref().is_some_and(Wizard::started);
            if self.saving && self.wizard.is_some() {
                return vec![Reply::Show];
            }
            if (self.settings.dirty() || wizard_started) && !self.confirmed {
                return ask_discard(input);
            }
            if input == SettingsInput::Pressed(FieldId::AddSync) {
                // Settings stays behind the wizard on the entry shown
                // before, without a discarded draft; Cancel goes back to it
                let back = self.settings.selection().clone();
                self.wizard = None;
                self.select(Selection::None);
                self.restore(back.clone());
                self.wizard_return = Some(back);
                self.start_wizard(false);
                return vec![Reply::Show];
            }
            self.wizard = None;
            self.select(selection);
            if let SettingsInput::Pressed(FieldId::DuplicateSync(name)) = &input {
                self.settings.select_duplicate(name);
            }
            return vec![Reply::Show];
        }
        match input {
            SettingsInput::CloseRequested => {
                if self.settings.dirty() && !self.confirmed && !self.saving {
                    return ask_discard(SettingsInput::CloseRequested);
                }
                vec![Reply::Close]
            }
            SettingsInput::Pressed(FieldId::Save) => self.save(),
            SettingsInput::Toggled(FieldId::StartAtLogin, on) => {
                self.start_at_login = on;
                vec![Reply::SetStartAtLogin(on), Reply::Show]
            }
            SettingsInput::Toggled(FieldId::CheckUpdates, on) => {
                if let Some(updates) = &mut self.updates {
                    updates.enabled = on;
                }
                vec![Reply::SetCheckUpdates(on), Reply::Show]
            }
            SettingsInput::Pressed(FieldId::CheckUpdatesNow) => vec![Reply::CheckUpdatesNow],
            SettingsInput::Chose(FieldId::LogLevel, index, _) => {
                match LOG_LEVELS
                    .get(index)
                    .filter(|l| **l != self.settings.log_level())
                {
                    Some(level) if !self.saving => {
                        self.after_save = Some(Selection::General);
                        self.begin_save(SettingsChange {
                            edits: vec![Edit::SetLogLevel(level.to_string())],
                            ..SettingsChange::default()
                        })
                    }
                    _ => vec![Reply::Show],
                }
            }
            SettingsInput::Pressed(FieldId::OpenConfig) => vec![Reply::Send(Command::EditConfig)],
            SettingsInput::Pressed(FieldId::RevealConfig) => vec![Reply::RevealConfig],
            SettingsInput::Pressed(FieldId::ReloadConfig) => vec![Reply::Send(Command::Reload)],
            SettingsInput::Pressed(FieldId::ShowLog) => vec![Reply::ShowLog],
            SettingsInput::Pressed(FieldId::Sponsor(i)) => crate::sponsor::OPTIONS
                .get(i)
                .map(|o| vec![Reply::OpenUrl(o.url)])
                .unwrap_or_default(),
            SettingsInput::Pressed(FieldId::FullSyncNow) => match self.saved_sync_name() {
                Some(name) => vec![Reply::Send(Command::FullSync(Some(name))), Reply::Show],
                None => vec![Reply::Show],
            },
            SettingsInput::Pressed(FieldId::RemoveSyncedEvents) => match self.saved_sync_name() {
                Some(name) => vec![Reply::Purge(name)],
                None => vec![Reply::Show],
            },
            SettingsInput::Pressed(FieldId::RemoveAllEvents) => {
                match self
                    .saved_sync_name()
                    .and_then(|n| self.settings.target_of(&n))
                {
                    Some((calendar, syncs)) => vec![Reply::PurgeCalendar {
                        name: self.calendar_title(&calendar),
                        calendar,
                        syncs,
                    }],
                    None => vec![Reply::Show],
                }
            }
            SettingsInput::Pressed(FieldId::Delete) => self.delete(false),
            SettingsInput::Pressed(FieldId::DeleteKeepCopies) => self.delete(true),
            SettingsInput::Pressed(FieldId::Revert | FieldId::Reload) => {
                self.generation += 1;
                self.settings.revert();
                self.undo.clear();
                self.error = None;
                vec![Reply::Show]
            }
            SettingsInput::Pressed(FieldId::Overwrite) => {
                self.settings.overwrite();
                vec![Reply::Show]
            }
            SettingsInput::Pressed(FieldId::Test) => self.test(),
            SettingsInput::Pressed(FieldId::RefreshCalendars) => {
                let mut replies: Vec<Reply> = self
                    .picker
                    .refresh()
                    .into_iter()
                    .map(|a| Reply::Send(Command::DiscoverAccount(a)))
                    .collect();
                // A wizard for CalDAV does not read (and ask for) Calendar.app
                if self.picker.has_calendar_app() && self.wizard_shows(CALENDAR_APP) {
                    replies.push(Reply::Send(Command::ListCalendarApp));
                }
                replies.push(Reply::Show);
                replies
            }
            SettingsInput::Chose(FieldId::Source | FieldId::Target, index, _) => {
                let SettingsInput::Chose(id, _, _) = &input else {
                    unreachable!()
                };
                self.choose_calendar(id.clone(), index, &input)
            }
            SettingsInput::Pressed(FieldId::Advanced)
            | SettingsInput::Toggled(FieldId::Advanced, _) => {
                self.advanced = !self.advanced;
                vec![Reply::Show]
            }
            other => {
                self.edit(other);
                vec![Reply::Show]
            }
        }
    }

    fn select(&mut self, selection: Selection) {
        self.generation += 1;
        self.undo.clear();
        self.next_event = None;
        self.settings.select(selection);
        self.transform = None;
        self.custom_fields = false;
        self.custom_interval = false;
        self.error = None;
        self.note = None;
    }

    /// A change of the selected form.
    fn edit(&mut self, input: SettingsInput) {
        self.error = None;
        if let Some(form) = self.settings.account_form() {
            match input {
                SettingsInput::Text(FieldId::AccountName, t) => form.set_name(&t),
                SettingsInput::Text(FieldId::Url, t) => form.set_url(&t),
                SettingsInput::Text(FieldId::Username, t) => form.set_username(&t),
                SettingsInput::Text(FieldId::Password, t) => form.set_password(&t),
                _ => {}
            }
            return;
        }
        if let SettingsInput::Text(FieldId::FeedUrl, t) = &input {
            self.settings.set_feed_url(t);
            return;
        }
        let transform = self.transform;
        // For a common step that names the source, as "Busy (Work)"
        let source_name = self
            .settings
            .sync_form_ref()
            .and_then(|f| self.settings.calendar(f.source()))
            .map(|c| self.calendar_own_name(c))
            .unwrap_or_default();
        let Some(form) = self.settings.sync_form() else {
            return;
        };
        // Adding, moving and removing steps can be undone
        if matches!(
            input,
            SettingsInput::Chose(FieldId::InsertTransform(_), _, _)
                | SettingsInput::Moved(FieldId::TransformRow(_), _)
                | SettingsInput::Pressed(FieldId::TransformRowRemove(_))
        ) {
            self.undo.push((form.clone(), transform));
        }
        match input {
            SettingsInput::Text(FieldId::SyncName, t) => form.set_name(&t),
            SettingsInput::Text(FieldId::Interval, t) => {
                // Typing keeps the field, also when it reads like a preset
                self.custom_interval = true;
                form.set_interval(&t);
            }
            SettingsInput::Text(FieldId::OtherFields, t) => {
                self.custom_fields = true;
                form.set_other_fields(&t);
            }
            SettingsInput::Text(FieldId::Concurrency, t) => form.set_concurrency(&t),
            SettingsInput::Text(FieldId::DaysPast, t) => {
                let future = form.text(SyncField::EventKitDaysFuture).to_string();
                form.set_eventkit_days(&t, &future);
            }
            SettingsInput::Text(FieldId::DaysFuture, t) => {
                let past = form.text(SyncField::EventKitDaysPast).to_string();
                form.set_eventkit_days(&past, &t);
            }
            SettingsInput::Text(FieldId::TransformField(key), t) => {
                if let Some(i) = transform.filter(|i| form.transform(*i).is_some()) {
                    form.transform_mut(i).set(key, &t);
                }
            }
            SettingsInput::Chose(FieldId::ConditionSubject(c), index, _) => {
                if let (Some(i), Some((subject, _))) = (
                    transform.filter(|i| form.transform(*i).is_some()),
                    SUBJECTS.get(index),
                ) {
                    form.transform_mut(i).set_subject(c, *subject);
                }
            }
            SettingsInput::Chose(FieldId::ConditionTest(c), index, _) => {
                if let Some(i) = transform.filter(|i| form.transform(*i).is_some()) {
                    form.transform_mut(i).set_test(c, index);
                }
            }
            SettingsInput::Text(FieldId::ConditionText(c, key), t) => {
                if let Some(i) = transform.filter(|i| form.transform(*i).is_some()) {
                    form.transform_mut(i).set_condition(c, key, &t);
                }
            }
            SettingsInput::Toggled(FieldId::ConditionDay(c, day), on) => {
                if let Some(i) = transform.filter(|i| form.transform(*i).is_some()) {
                    form.transform_mut(i).toggle_day(c, day, on);
                }
            }
            SettingsInput::Pressed(FieldId::ConditionRemove(c)) => {
                if let Some(i) = transform.filter(|i| form.transform(*i).is_some()) {
                    form.transform_mut(i).remove_condition(c);
                }
            }
            SettingsInput::Pressed(FieldId::ConditionMoveUp(c)) => {
                if let Some(i) = transform.filter(|i| form.transform(*i).is_some()) {
                    form.transform_mut(i).move_condition_up(c);
                }
            }
            SettingsInput::Pressed(FieldId::ConditionMoveDown(c)) => {
                if let Some(i) = transform.filter(|i| form.transform(*i).is_some()) {
                    form.transform_mut(i).move_condition_up(c + 1);
                }
            }
            SettingsInput::Moved(FieldId::Condition(c), slot) => {
                if let Some(i) = transform.filter(|i| form.transform(*i).is_some()) {
                    let t = form.transform_mut(i);
                    if let Some(drop) = t.drops().get(slot).copied() {
                        t.move_condition(c, drop);
                    }
                }
            }
            SettingsInput::Selected(FieldId::ConditionJoin(c), index) => {
                if let Some(i) = transform.filter(|i| form.transform(*i).is_some()) {
                    form.transform_mut(i).set_or(c, index == 1);
                }
            }
            SettingsInput::Pressed(FieldId::AddCondition) => {
                if let Some(i) = transform.filter(|i| form.transform(*i).is_some()) {
                    form.transform_mut(i).add_condition();
                }
            }
            SettingsInput::Toggled(FieldId::Enabled, on) => form.set_enabled(on),
            SettingsInput::Toggled(FieldId::AllEvents, on) => form.set_eventkit_all_events(on),
            SettingsInput::Toggled(FieldId::Field(property), _) => {
                self.custom_fields = true;
                form.toggle_field(property);
            }
            SettingsInput::Selected(FieldId::IntervalPreset, index) => {
                match INTERVAL_PRESETS.get(index) {
                    Some((_, text)) => {
                        form.set_interval(text);
                        self.custom_interval = false;
                    }
                    None => self.custom_interval = true,
                }
            }
            SettingsInput::Chose(FieldId::InsertTransform(at), index, _) => {
                if let Some(action) = index.checked_sub(1).and_then(|i| ACTIONS.get(i)) {
                    self.transform = Some(form.insert_transform(at, action.action.clone()));
                } else if let Some(template) = index
                    .checked_sub(ACTIONS.len() + 2)
                    .and_then(|i| TEMPLATES.get(i))
                {
                    // A filled in step needs no editing: it stays closed
                    let at = form.insert_transform(at, template.action.clone());
                    form.transform_mut(at).set_when((template.when)());
                    for (key, text) in template.texts {
                        form.transform_mut(at)
                            .set(key, &text.replace("{source}", &source_name));
                    }
                    self.transform = None;
                }
            }
            SettingsInput::Selected(FieldId::CopyPreset, index) => match COPY_PRESETS.get(index) {
                Some((_, fields)) => {
                    form.set_fields(fields);
                    self.custom_fields = false;
                }
                None => self.custom_fields = true,
            },
            SettingsInput::Pressed(FieldId::TransformRow(i)) => {
                self.transform = (transform != Some(i)).then_some(i);
            }
            SettingsInput::Moved(FieldId::TransformRow(from), gap) => {
                if let Some(to) = form.move_transform_to(from, gap) {
                    // The open editor stays with its transform
                    self.transform = transform.map(|t| match t {
                        t if t == from => to,
                        t if from < t && t <= to => t - 1,
                        t if to <= t && t < from => t + 1,
                        t => t,
                    });
                }
            }
            SettingsInput::Pressed(FieldId::TransformRowRemove(i)) => {
                form.remove_transform(i);
                self.transform = transform.and_then(|t| match t {
                    t if t == i => None,
                    t if t > i => Some(t - 1),
                    t => Some(t),
                });
            }
            _ => {}
        }
    }

    fn choose_calendar(&mut self, id: FieldId, index: usize, input: &SettingsInput) -> Vec<Reply> {
        let Some(form) = self.settings.sync_form_ref() else {
            return Vec::new();
        };
        let (role, current) = match id {
            FieldId::Source => (Role::Source, form.source().to_string()),
            _ => (Role::Target, form.target().to_string()),
        };
        let (items, targets, _) = self.calendar_items(role, &current);
        // GTK's dropdown cannot disable items; a header, a status line or a
        // read-only target chosen there changes nothing. Neither does an
        // item that is no longer the one shown when it was chosen.
        let shown = match input {
            SettingsInput::Chose(_, _, title) => title.as_deref(),
            _ => None,
        };
        let stale = |i: &PopupItem| shown.is_some_and(|t| t != i.title);
        if items
            .get(index)
            .is_none_or(|i| !i.enabled || i.header || stale(i))
        {
            return vec![Reply::Show];
        }
        match targets.get(index) {
            Some(PopupTarget::Calendar(g, i)) => {
                let choice = self.picker.choose(*g, *i);
                self.settings.pick(role, choice);
                vec![Reply::Show]
            }
            Some(PopupTarget::AddFeed) => {
                self.settings.pick(
                    Role::Source,
                    CalendarChoice {
                        key: "feed".to_string(),
                        calendar: feed_calendar(""),
                        new_entry: true,
                    },
                );
                vec![Reply::Show]
            }
            Some(PopupTarget::AddAccount) => {
                if self.settings.dirty() && !self.confirmed {
                    return vec![
                        Reply::Ask {
                            confirm: discard(),
                            then: Box::new(input.clone()),
                            alternative: None,
                        },
                        Reply::Show,
                    ];
                }
                self.select(Selection::NewAccount);
                vec![Reply::Show]
            }
            _ => vec![Reply::Show],
        }
    }

    fn save(&mut self) -> Vec<Reply> {
        let is_sync = self.settings.sync_form_ref().is_some();
        let result = if is_sync {
            self.settings.save_sync()
        } else {
            self.settings.save_account().map(|change| (change, None))
        };
        let (change, confirm) = match result {
            Ok(saved) => saved,
            Err(e) => {
                self.error = Some(e);
                return vec![Reply::Show];
            }
        };
        if let Some(confirm) = confirm
            && !self.confirmed
        {
            return vec![Reply::Ask {
                confirm,
                then: Box::new(SettingsInput::Pressed(FieldId::Save)),
                alternative: None,
            }];
        }
        self.after_save = Some(self.saved_selection(&change));
        self.begin_save(change)
    }

    /// What to show after a save: the saved entry under its new name.
    fn saved_selection(&self, change: &crate::settings::SettingsChange) -> Selection {
        use crate::settings::Edit;
        change
            .edits
            .iter()
            .rev()
            .find_map(|e| match e {
                Edit::SetSync(s) => Some(Selection::Sync(s.name.clone())),
                Edit::SetAccount(n, _) => Some(Selection::Account(n.clone())),
                _ => None,
            })
            .unwrap_or_else(|| {
                let first = self
                    .settings
                    .sync_names()
                    .into_iter()
                    .find(|n| !change.edits.contains(&Edit::RemoveSync(n.clone())));
                first.map_or(Selection::None, Selection::Sync)
            })
    }

    fn begin_save(&mut self, change: crate::settings::SettingsChange) -> Vec<Reply> {
        self.saving = true;
        self.error = None;
        self.note = None;
        vec![Reply::Send(Command::SaveSettings(change)), Reply::Show]
    }

    fn delete(&mut self, keep_copies: bool) -> Vec<Reply> {
        match self.settings.selection().clone() {
            Selection::Sync(_) => {
                let (change, confirm) = self.settings.delete_sync(keep_copies);
                if !self.confirmed {
                    let keep = (!keep_copies).then_some((
                        "Keep Copies",
                        SettingsInput::Pressed(FieldId::DeleteKeepCopies),
                    ));
                    return vec![Reply::Ask {
                        confirm,
                        then: Box::new(SettingsInput::Pressed(if keep_copies {
                            FieldId::DeleteKeepCopies
                        } else {
                            FieldId::Delete
                        })),
                        alternative: keep,
                    }];
                }
                self.after_save = Some(self.saved_selection(&change));
                self.begin_save(change)
            }
            Selection::Account(name) => match self.settings.delete_account() {
                Err(e) => {
                    self.error = Some(e);
                    vec![Reply::Show]
                }
                Ok(change) if !self.confirmed => {
                    let _ = change;
                    vec![Reply::Ask {
                        confirm: Confirm {
                            heading: format!("Remove \"{name}\"?"),
                            message: "The account and its calendars are removed from the config."
                                .to_string(),
                            button: "Remove Account",
                            destructive: true,
                        },
                        then: Box::new(SettingsInput::Pressed(FieldId::Delete)),
                        alternative: None,
                    }]
                }
                Ok(change) => {
                    self.after_save = Some(Selection::None);
                    self.begin_save(change)
                }
            },
            _ => Vec::new(),
        }
    }

    fn test(&mut self) -> Vec<Reply> {
        let Some(form) = self.settings.account_form_ref() else {
            return Vec::new();
        };
        let (url, username, password) = form.test_credentials();
        let account = form.original_name().map(str::to_string);
        self.note = Some("Testing…".to_string());
        vec![
            Reply::Send(Command::TestAccount {
                account,
                url,
                username,
                password,
            }),
            Reply::Show,
        ]
    }

    /// An update of the backend; `config` is the config file as it is now,
    /// read by the app for `ConfigChanged` and `SettingsSaved`.
    pub fn update(
        &mut self,
        update: &Update,
        config: Option<(&Config, &ConfigEditor)>,
    ) -> Vec<Reply> {
        match update {
            Update::AccountCalendars { account, result } => {
                let listing = match result {
                    Ok(found) => Listing::Loaded(found.clone()),
                    Err(e) => Listing::Failed(e.clone()),
                };
                self.picker.set_account_listing(account, listing);
                vec![Reply::Show]
            }
            Update::NextSourceEvent { source, result } => {
                if self
                    .next_event
                    .as_ref()
                    .is_some_and(|(key, _)| key == source)
                {
                    self.next_event = Some((source.clone(), Some(result.clone())));
                }
                vec![Reply::Show]
            }
            Update::CalendarAppCalendars(result) => {
                self.picker.set_calendar_app_listing(match result {
                    Ok(found) => Listing::Loaded(found.clone()),
                    Err(e) => Listing::Failed(e.clone()),
                });
                vec![Reply::Show]
            }
            Update::AccountTested(result) => {
                self.note = Some(match result {
                    Ok(1) => "Found 1 calendar".to_string(),
                    Ok(n) => format!("Found {n} calendars"),
                    Err(e) => format!("Test failed: {e}"),
                });
                vec![Reply::Show]
            }
            Update::PurgeProgress { sync, done, total } if self.saving => {
                self.removing = total
                    .filter(|t| *t > 0)
                    .map(|t| (sync.clone(), (*done).min(t), t));
                vec![Reply::Show]
            }
            Update::SettingsSaved(result) => {
                self.saving = false;
                self.removing = None;
                match result {
                    Ok(()) => {
                        if let Some(wizard) = &mut self.wizard
                            && wizard.waiting
                        {
                            wizard.waiting = false;
                            if wizard.step == Step::Account {
                                // The account is saved: on to its calendars,
                                // back where it was added from
                                let next = match wizard.account_from {
                                    Some(Step::Target) => Step::Target,
                                    _ => Step::Source,
                                };
                                if matches!(wizard.account_from, Some(Step::Source | Step::Target))
                                {
                                    // Calendar.app and CalDAV side by side now
                                    wizard.start = None;
                                }
                                self.wizard_go(next);
                            } else if wizard.first && wizard.preset != Preset::Custom {
                                // The first sync: say it is done, in the small window
                                self.wizard_go(Step::Done);
                            } else {
                                self.wizard = None;
                            }
                        }
                        let selection = self.after_save.take().unwrap_or(Selection::None);
                        self.changed_while_saving = false;
                        if let Some((config, editor)) = config {
                            self.generation += 1;
                            self.settings.saved(config, editor, selection);
                            self.transform = None;
                            return self.rebase_picker(config);
                        }
                    }
                    Err(e) => {
                        if let Some(wizard) = &mut self.wizard {
                            wizard.waiting = false;
                        }
                        self.error = Some(e.clone());
                        if std::mem::take(&mut self.changed_while_saving)
                            && let Some((config, editor)) = config
                        {
                            self.settings.file_changed(config, editor);
                            return self.rebase_picker(config);
                        }
                    }
                }
                vec![Reply::Show]
            }
            // The app's own save reloads too; SettingsSaved rebases then,
            // or replays the change if the save failed
            Update::ConfigChanged if self.saving => {
                self.changed_while_saving = true;
                Vec::new()
            }
            Update::ConfigChanged => match config {
                Some((config, editor)) if self.broken => self.recover(config, editor),
                Some((config, editor)) => {
                    let dirty = self.settings.dirty();
                    self.settings.file_changed(config, editor);
                    if !dirty && self.wizard.is_none() {
                        // The form was replaced by the file's version
                        self.generation += 1;
                    }
                    self.rebase_picker(config)
                }
                None => Vec::new(),
            },
            _ => Vec::new(),
        }
    }

    /// The config file can be read again: the whole window from now on.
    fn recover(&mut self, config: &Config, editor: &ConfigEditor) -> Vec<Reply> {
        let (screen, commands) = Self::new(config, editor, self.picker.has_calendar_app());
        let old = std::mem::replace(self, screen);
        self.start_at_login = old.start_at_login;
        self.config_path = old.config_path;
        self.statuses = old.statuses;
        self.now = old.now;
        self.generation = old.generation + 1;
        if self.wizard.is_none() {
            self.select(Selection::General);
        }
        let mut replies: Vec<Reply> = commands.into_iter().map(Reply::Send).collect();
        replies.push(Reply::Show);
        replies
    }

    fn start_wizard(&mut self, first: bool) {
        let has_accounts = !self.settings.account_names().is_empty();
        let step = match (has_accounts, self.picker.has_calendar_app()) {
            (false, _) if first => Step::Start,
            (false, false) => Step::Account,
            _ => Step::Source,
        };
        self.generation += 1;
        self.error = None;
        self.note = None;
        self.wizard = Some(Wizard {
            step,
            first,
            account: AccountForm::new_account(),
            account_from: None,
            start: None,
            picked: None,
            source: None,
            target: None,
            preset: Preset::Busy,
            name: String::new(),
            proposed: String::new(),
            waiting: false,
            feed: None,
        });
    }

    /// The calendars of a calendar step under a header per account, the
    /// calendar of each item (None for a header or a read-only one) and
    /// the status of a listing that is loading or failed.
    fn wizard_calendars(
        &self,
        role: Role,
    ) -> (Vec<Choice>, Vec<Option<CalendarChoice>>, Vec<String>) {
        let mut items = Vec::new();
        let mut choices = Vec::new();
        let mut notes = Vec::new();
        let source = self.wizard.as_ref().and_then(|w| w.source.as_ref());
        for (g, group) in self.picker.groups(role, None).into_iter().enumerate() {
            if !self.wizard_shows(&group.title) {
                continue;
            }
            if let Some(status) = &group.status {
                notes.push(format!("{}: {status}", group.title));
            }
            // Calendar.app's calendars under their account, as there
            let calendar_app = group.title == CALENDAR_APP;
            let mut section = None;
            for (i, item) in group.items.into_iter().enumerate() {
                let choice = self.picker.choose(g, i);
                // The source stays in its place, greyed out
                let is_source = role == Role::Target && source == Some(&choice.calendar);
                let header = if calendar_app {
                    item.section
                        .clone()
                        .unwrap_or_else(|| "On My Mac".to_string())
                } else {
                    group.title.clone()
                };
                if section.as_ref() != Some(&header) {
                    items.push(Choice {
                        title: header.clone(),
                        header: true,
                        ..Choice::default()
                    });
                    choices.push(None);
                    section = Some(header);
                }
                let enabled = item.enabled && !is_source;
                let hint = if is_source {
                    Some("the source".to_string())
                } else {
                    (!item.enabled).then(|| "read-only".to_string())
                };
                items.push(Choice {
                    title: if calendar_app { item.title } else { item.label },
                    enabled,
                    hint,
                    ..Choice::default()
                });
                choices.push(enabled.then_some(choice));
            }
        }
        (items, choices, notes)
    }

    /// Whether the wizard lists this group: the start step's answer limits
    /// it to Calendar.app or to the CalDAV accounts.
    fn wizard_shows(&self, group: &str) -> bool {
        let calendar_app = group == CALENDAR_APP;
        match self.wizard.as_ref().and_then(|w| w.start) {
            _ if group == FEEDS => true,
            Some(0) => calendar_app,
            Some(_) => !calendar_app,
            None => true,
        }
    }

    /// How a calendar of the wizard is called: its title and where it is.
    /// A calendar's own name, without its account.
    fn calendar_name(&self, calendar: &CalendarConfig) -> String {
        for (g, group) in self
            .picker
            .groups(Role::Source, None)
            .into_iter()
            .enumerate()
        {
            for (i, item) in group.items.iter().enumerate() {
                if self.picker.choose(g, i).calendar == *calendar {
                    return item.label.clone();
                }
            }
        }
        self.calendar_label(calendar)
    }

    /// The calendar's own name without its account, for the copies'
    /// titles ("Busy (Personal)").
    fn calendar_own_name(&self, calendar: &CalendarConfig) -> String {
        for (g, group) in self
            .picker
            .groups(Role::Source, None)
            .into_iter()
            .enumerate()
        {
            for (i, item) in group.items.iter().enumerate() {
                if self.picker.choose(g, i).calendar == *calendar {
                    return item.title.clone();
                }
            }
        }
        self.calendar_name(calendar)
    }

    fn calendar_label(&self, calendar: &CalendarConfig) -> String {
        for (g, group) in self
            .picker
            .groups(Role::Source, None)
            .into_iter()
            .enumerate()
        {
            for (i, item) in group.items.iter().enumerate() {
                if self.picker.choose(g, i).calendar == *calendar {
                    return if group.title == CALENDAR_APP {
                        item.label.clone()
                    } else {
                        format!("{} ({})", item.label, group.title)
                    };
                }
            }
        }
        self.settings
            .calendar_key_for(calendar)
            .or_else(|| new_feed_key(calendar))
            .unwrap_or_else(|| "calendar".to_string())
    }

    /// The step Back goes to, if any.
    fn wizard_back(&self, wizard: &Wizard) -> Option<Step> {
        let first_of_all = wizard.first && self.settings.account_names().is_empty();
        match wizard.step {
            Step::Start => None,
            Step::Account => wizard.account_from.or(first_of_all.then_some(Step::Start)),
            Step::Done => None,
            Step::Source => (first_of_all && self.picker.has_calendar_app()).then_some(Step::Start),
            Step::Target => Some(Step::Source),
            Step::Copy => Some(Step::Target),
            Step::Name => Some(Step::Copy),
        }
    }

    /// The keys source and target get, and the new `[calendars]` entries:
    /// a configured calendar keeps its key, a new one gets a free one.
    fn wizard_keys(
        &self,
        source: &CalendarConfig,
        target: &CalendarConfig,
    ) -> (String, String, Vec<Edit>) {
        let mut taken = self.settings.calendar_keys();
        let mut edits = Vec::new();
        let mut key_of = |calendar: &CalendarConfig, proposal: String| {
            if let Some(key) = self.settings.calendar_key_for(calendar) {
                return key;
            }
            let key = (1..)
                .map(|n| match n {
                    1 => proposal.clone(),
                    n => format!("{proposal}_{n}"),
                })
                .find(|k| !taken.contains(k))
                .unwrap_or(proposal);
            taken.push(key.clone());
            edits.push(Edit::SetCalendar(key.clone(), calendar.clone()));
            key
        };
        let source_key = key_of(source, self.proposed_key(source));
        let target_key = key_of(target, self.proposed_key(target));
        (source_key, target_key, edits)
    }

    /// The key the picker proposes for a calendar not configured yet.
    fn proposed_key(&self, calendar: &CalendarConfig) -> String {
        for role in [Role::Source, Role::Target] {
            for (g, group) in self.picker.groups(role, None).into_iter().enumerate() {
                for i in 0..group.items.len() {
                    let choice = self.picker.choose(g, i);
                    if choice.calendar == *calendar {
                        return choice.key;
                    }
                }
            }
        }
        new_feed_key(calendar).unwrap_or_else(|| "calendar".to_string())
    }

    fn wizard_controls(&self, wizard: &Wizard, controls: &mut Vec<Control>) {
        let mac = self.picker.has_calendar_app();
        let hero = |image, title: &str, text: &str| Control::Hero {
            image,
            title: title.to_string(),
            text: text.to_string(),
        };
        let current = match wizard.step {
            Step::Source => Some(0),
            Step::Target => Some(1),
            Step::Copy => Some(2),
            Step::Name => Some(3),
            _ => None,
        };
        if let Some(current) = current {
            controls.push(Control::Steps { current, count: 4 });
        }
        let source = wizard.source.as_ref().map(|c| self.calendar_label(c));
        let can_next = match wizard.step {
            Step::Done => {
                let target = wizard
                    .target
                    .as_ref()
                    .map(|t| self.calendar_label(t))
                    .unwrap_or_default();
                controls.push(hero(
                    Some(HeroImage::Done),
                    "You're all set",
                    &format!(
                        "\"{}\" now copies the events of {} into {target} every 5 minutes. \
                         Change it, or add more syncs, any time in Settings.",
                        wizard.name.trim(),
                        source.unwrap_or_default()
                    ),
                ));
                controls.push(Control::Footer {
                    left: vec![(
                        FieldId::WizardOpenSettings,
                        "Open Settings".to_string(),
                        true,
                    )],
                    text: String::new(),
                    warning: false,
                    progress: None,
                    right: vec![(FieldId::WizardDone, "Done".to_string(), true)],
                });
                return;
            }
            Step::Start if !mac => {
                controls.push(hero(
                    Some(HeroImage::App),
                    "Welcome to Capycal",
                    &format!("{WELCOME}\n\nFirst, add the CalDAV account of your calendars."),
                ));
                true
            }
            Step::Start => {
                controls.push(hero(Some(HeroImage::App), "Welcome to Capycal", WELCOME));
                controls.push(Control::Choices {
                    id: FieldId::WizardList,
                    style: ChoiceStyle::Cards,
                    items: vec![
                        Choice {
                            title: "Calendars in Calendar.app".to_string(),
                            detail: "iCloud, Exchange, Google and every account on this Mac"
                                .to_string(),
                            icon: Some(ChoiceIcon::CalendarApp),
                            enabled: true,
                            ..Choice::default()
                        },
                        Choice {
                            title: "A CalDAV account".to_string(),
                            detail: "Nextcloud, Stalwart, mailbox.org, Fastmail, …".to_string(),
                            icon: Some(ChoiceIcon::CalDav),
                            enabled: true,
                            ..Choice::default()
                        },
                    ],
                    selected: wizard.start,
                });
                // Before macOS asks: why, and what capycal does with it
                controls.push(Control::Note(
                    if wizard.start == Some(0) {
                        CALENDAR_ACCESS
                    } else {
                        ""
                    }
                    .to_string(),
                ));
                wizard.start.is_some()
            }
            Step::Account => {
                let another = matches!(wizard.account_from, Some(Step::Source | Step::Target))
                    && !self.settings.account_names().is_empty();
                controls.push(hero(
                    None,
                    if another {
                        "Add another account"
                    } else {
                        "Add your account"
                    },
                    "The CalDAV server and how to sign in.",
                ));
                let form = &wizard.account;
                controls.push(Self::text(
                    FieldId::AccountName,
                    "Name",
                    form.name(),
                    "nextcloud",
                    None,
                ));
                controls.push(Self::text(
                    FieldId::Url,
                    "Server URL",
                    form.url(),
                    "https://cloud.example.org",
                    None,
                ));
                controls.push(Self::text(
                    FieldId::Username,
                    "User name",
                    form.username(),
                    "",
                    None,
                ));
                let store = if cfg!(target_os = "macos") {
                    "Keychain"
                } else {
                    "keyring"
                };
                controls.push(Control::Text {
                    id: FieldId::Password,
                    label: "Password".to_string(),
                    value: form.password_text().to_string(),
                    placeholder: format!("Stored in the {store}"),
                    error: None,
                    secure: true,
                    enabled: true,
                    suggestions: Vec::new(),
                });
                controls.push(Control::Buttons(vec![(
                    FieldId::Test,
                    "Test".to_string(),
                    !form.url().is_empty() && !self.saving,
                )]));
                form.build(&self.settings.account_names()).is_ok()
            }
            Step::Source | Step::Target => {
                let (role, title, text) = if wizard.step == Step::Source {
                    (
                        Role::Source,
                        "Copy events from",
                        "Choose the calendar whose events Capycal copies.".to_string(),
                    )
                } else {
                    (
                        Role::Target,
                        "Into",
                        format!(
                            "Choose where the copies of {} go.",
                            source.as_deref().unwrap_or("the events")
                        ),
                    )
                };
                controls.push(hero(None, title, &text));
                let (items, choices, notes) = self.wizard_calendars(role);
                for note in notes {
                    controls.push(Control::Note(note));
                }
                let selected = wizard.picked.as_ref().and_then(|picked| {
                    choices
                        .iter()
                        .position(|c| c.as_ref().is_some_and(|c| c.calendar == *picked))
                });
                controls.push(Control::Choices {
                    id: FieldId::WizardList,
                    style: ChoiceStyle::Rows,
                    items,
                    selected,
                });
                let feed = wizard.feed.as_ref().filter(|_| role == Role::Source);
                if let Some(url) = feed {
                    let error =
                        (!url.trim().is_empty() && !feed_url_valid(url)).then_some(FEED_INVALID);
                    controls.push(Self::text(
                        FieldId::FeedUrl,
                        "Feed address",
                        url,
                        FEED_PLACEHOLDER,
                        error,
                    ));
                }
                let add = if self.settings.account_names().is_empty() {
                    "Add CalDAV Account…"
                } else {
                    "Add Another Account…"
                };
                let mut buttons = vec![
                    (
                        FieldId::RefreshCalendars,
                        "Refresh Calendars".to_string(),
                        true,
                    ),
                    (FieldId::WizardAddAccount, add.to_string(), !self.saving),
                ];
                if role == Role::Source && feed.is_none() {
                    buttons.push((FieldId::WizardAddFeed, ADD_FEED.to_string(), !self.saving));
                }
                controls.push(Control::Buttons(buttons));
                selected.is_some() || wizard.picked.as_ref().is_some_and(|p| p.is_ics())
            }
            Step::Copy => {
                let target = wizard.target.as_ref().map(|c| self.calendar_label(c));
                controls.push(hero(
                    None,
                    "What should the copies show?",
                    &format!(
                        "{} \u{2192} {}. Pick a starting point: everything can be changed \
                         later in Settings.",
                        source.unwrap_or_default(),
                        target.unwrap_or_default()
                    ),
                ));
                let name = wizard
                    .source
                    .as_ref()
                    .map(|c| self.calendar_own_name(c))
                    .unwrap_or_default();
                controls.push(Control::Choices {
                    id: FieldId::WizardList,
                    style: ChoiceStyle::Cards,
                    items: Preset::ALL.iter().map(|p| p.choice(&name)).collect(),
                    selected: Preset::ALL.iter().position(|p| *p == wizard.preset),
                });
                true
            }
            Step::Name => {
                let target = wizard.target.as_ref().map(|c| self.calendar_label(c));
                let text = if wizard.preset == Preset::Custom {
                    "It starts switched off: set it up in the editor, then switch it on."
                        .to_string()
                } else {
                    format!(
                        "It copies the events of {} into {} every 5 minutes.",
                        source.unwrap_or_default(),
                        target.unwrap_or_default()
                    )
                };
                controls.push(hero(None, "Name the sync", &text));
                let error = sync_name_error(&wizard.name);
                controls.push(Self::text(
                    FieldId::WizardName,
                    "Name",
                    &wizard.name,
                    "",
                    error,
                ));
                error.is_none()
            }
        };
        controls.push(Control::Error(self.error.clone().unwrap_or_default()));
        let note = match (self.saving, &self.note) {
            (true, _) => "Saving…".to_string(),
            (false, Some(note)) => note.clone(),
            (false, None) => String::new(),
        };
        controls.push(Control::Note(note));
        let next = if wizard.step == Step::Name {
            "Create"
        } else {
            "Continue"
        };
        let mut left = vec![(FieldId::WizardCancel, "Cancel".to_string(), !self.saving)];
        // The way out for those who rather fill in the form
        if matches!(
            wizard.step,
            Step::Source | Step::Target | Step::Copy | Step::Name
        ) {
            left.push((
                FieldId::AddEmptySync,
                "Set Up in Editor".to_string(),
                !self.saving,
            ));
        }
        controls.push(Control::Footer {
            left,
            text: String::new(),
            warning: false,
            progress: None,
            right: vec![
                (
                    FieldId::WizardBack,
                    "Back".to_string(),
                    self.wizard_back(wizard).is_some() && !self.saving,
                ),
                (
                    FieldId::WizardNext,
                    next.to_string(),
                    can_next && !self.saving,
                ),
            ],
        });
    }

    /// Move the wizard to `step`, showing what was picked there before.
    fn wizard_go(&mut self, step: Step) {
        let Some(wizard) = self.wizard.as_mut() else {
            return;
        };
        wizard.step = step;
        wizard.picked = match step {
            Step::Source => wizard.source.clone(),
            Step::Target => wizard.target.clone(),
            _ => None,
        };
        self.generation += 1;
        self.error = None;
        self.note = None;
    }

    /// The input of a running wizard; None for input it does not handle.
    fn wizard_input(&mut self, input: &SettingsInput) -> Option<Vec<Reply>> {
        let step = self.wizard.as_ref()?.step;
        let show = || Some(vec![Reply::Show]);
        let wizard_button = matches!(
            input,
            SettingsInput::Pressed(
                FieldId::WizardNext | FieldId::WizardBack | FieldId::WizardCancel
            )
        );
        if self.saving && wizard_button {
            return show();
        }
        match input {
            SettingsInput::Selected(FieldId::WizardList, i) => {
                let picked = match step {
                    Step::Source | Step::Target => {
                        let role = if step == Step::Source {
                            Role::Source
                        } else {
                            Role::Target
                        };
                        self.wizard_calendars(role).1.get(*i).cloned().flatten()
                    }
                    _ => None,
                };
                let wizard = self.wizard.as_mut()?;
                match step {
                    Step::Start => wizard.start = Some(*i),
                    Step::Copy => {
                        if let Some(preset) = Preset::ALL.get(*i) {
                            wizard.preset = *preset;
                        }
                    }
                    Step::Source | Step::Target => {
                        // A read-only row keeps the previous pick
                        if let Some(choice) = picked {
                            wizard.picked = Some(choice.calendar);
                        }
                    }
                    Step::Account | Step::Name | Step::Done => {}
                }
                show()
            }
            SettingsInput::Text(id, text) if step == Step::Account => {
                let form = &mut self.wizard.as_mut()?.account;
                match id {
                    FieldId::AccountName => form.set_name(text),
                    FieldId::Url => form.set_url(text),
                    FieldId::Username => form.set_username(text),
                    FieldId::Password => form.set_password(text),
                    _ => {}
                }
                show()
            }
            SettingsInput::Text(FieldId::WizardName, text) => {
                self.wizard.as_mut()?.name = text.clone();
                show()
            }
            SettingsInput::Pressed(FieldId::WizardAddFeed) if step == Step::Source => {
                self.wizard.as_mut()?.feed = Some(String::new());
                show()
            }
            SettingsInput::Text(FieldId::FeedUrl, text) if step == Step::Source => {
                let wizard = self.wizard.as_mut()?;
                wizard.feed = Some(text.clone());
                if feed_url_valid(text) {
                    wizard.picked = Some(feed_calendar(text.trim()));
                } else if wizard.picked.as_ref().is_some_and(|p| p.is_ics()) {
                    wizard.picked = None;
                }
                show()
            }
            SettingsInput::Pressed(FieldId::Test) if step == Step::Account => {
                let (url, username, password) = self.wizard.as_ref()?.account.test_credentials();
                self.note = Some("Testing…".to_string());
                Some(vec![
                    Reply::Send(Command::TestAccount {
                        account: None,
                        url,
                        username,
                        password,
                    }),
                    Reply::Show,
                ])
            }
            SettingsInput::Pressed(FieldId::WizardCancel) => {
                self.wizard = None;
                self.generation += 1;
                self.error = None;
                self.note = None;
                let back = self.wizard_return.take().unwrap_or(Selection::None);
                if self.settings.first_start() {
                    self.select(Selection::None);
                } else if back != Selection::None {
                    self.restore(back);
                }
                show()
            }
            SettingsInput::Pressed(FieldId::WizardBack) => {
                let back = self.wizard.as_ref().and_then(|w| self.wizard_back(w));
                if let Some(back) = back {
                    self.wizard_go(back);
                }
                show()
            }
            SettingsInput::Pressed(FieldId::WizardNext) => Some(self.wizard_next()),
            SettingsInput::Pressed(FieldId::WizardAddAccount)
                if matches!(step, Step::Source | Step::Target) =>
            {
                let wizard = self.wizard.as_mut()?;
                wizard.account = AccountForm::new_account();
                wizard.account_from = Some(step);
                self.wizard_go(Step::Account);
                show()
            }
            SettingsInput::Pressed(FieldId::AddEmptySync) => {
                self.wizard_to_editor();
                show()
            }
            SettingsInput::Pressed(FieldId::WizardDone) => {
                self.wizard = None;
                self.generation += 1;
                Some(vec![Reply::Close])
            }
            SettingsInput::Pressed(FieldId::WizardOpenSettings) => {
                self.wizard = None;
                self.generation += 1;
                show()
            }
            // Sidebar and window input end the wizard as usual
            _ => None,
        }
    }

    /// A new sync in the editor with what the wizard has so far.
    fn wizard_to_editor(&mut self) {
        let Some(wizard) = self.wizard.take() else {
            return;
        };
        self.select(Selection::NewSync);
        // The calendar shown in the current step counts as picked
        let (source, target) = match wizard.step {
            Step::Source => (wizard.picked.clone(), wizard.target.clone()),
            Step::Target => (wizard.source.clone(), wizard.picked.clone()),
            _ => (wizard.source.clone(), wizard.target.clone()),
        };
        for (role, calendar) in [(Role::Source, source), (Role::Target, target)] {
            let Some(calendar) = calendar else {
                continue;
            };
            let (key, new_entry) = match self.settings.calendar_key_for(&calendar) {
                Some(key) => (key, false),
                None => (self.proposed_key(&calendar), true),
            };
            self.settings.pick(
                role,
                CalendarChoice {
                    key,
                    calendar,
                    new_entry,
                },
            );
        }
        let source_name = wizard
            .source
            .as_ref()
            .map(|c| self.calendar_own_name(c))
            .unwrap_or_default();
        let preset = wizard
            .preset
            .apply(capycal::SyncConfig::new("", "", ""), &source_name);
        if let Some(form) = self.settings.sync_form() {
            if !wizard.name.trim().is_empty() {
                form.set_name(wizard.name.trim());
            }
            form.set_enabled(preset.enabled);
            let fields: Vec<&str> = preset.fields.iter().map(String::as_str).collect();
            form.set_fields(&fields);
            for (i, transform) in preset.transforms.iter().enumerate() {
                let at = form.insert_transform(i, transform.action.clone());
                form.transform_mut(at).set("field", &transform.field);
                if let Some(value) = &transform.value {
                    form.transform_mut(at).set("value", value);
                }
            }
        }
    }

    fn wizard_next(&mut self) -> Vec<Reply> {
        let Some(step) = self.wizard.as_ref().map(|w| w.step) else {
            return Vec::new();
        };
        match step {
            Step::Done => Vec::new(),
            Step::Start => {
                let caldav = !self.picker.has_calendar_app()
                    || self.wizard.as_ref().is_some_and(|w| w.start == Some(1));
                if caldav {
                    if let Some(w) = self.wizard.as_mut() {
                        w.account_from = Some(Step::Start);
                    }
                    self.wizard_go(Step::Account);
                    return vec![Reply::Show];
                }
                self.wizard_go(Step::Source);
                // Now Calendar.app is read, which may ask for access
                vec![Reply::Send(Command::ListCalendarApp), Reply::Show]
            }
            Step::Account => {
                let others = self.settings.account_names();
                let Some(wizard) = self.wizard.as_mut() else {
                    return Vec::new();
                };
                match wizard.account.build(&others) {
                    Err(e) => {
                        self.error = Some(e);
                        vec![Reply::Show]
                    }
                    Ok((name, account, keychain, _)) => {
                        wizard.waiting = true;
                        self.after_save = Some(Selection::None);
                        self.begin_save(SettingsChange {
                            keychain,
                            edits: vec![Edit::SetAccount(name, account)],
                            ..SettingsChange::default()
                        })
                    }
                }
            }
            Step::Source | Step::Target => {
                let role = if step == Step::Source {
                    Role::Source
                } else {
                    Role::Target
                };
                let choices = self.wizard_calendars(role).1;
                let Some(wizard) = self.wizard.as_mut() else {
                    return Vec::new();
                };
                // Still listed? Rows may have changed since it was picked.
                // A typed feed has no row
                let listed = wizard.picked.as_ref().is_some_and(|picked| {
                    (step == Step::Source && picked.is_ics())
                        || choices.iter().flatten().any(|c| c.calendar == *picked)
                });
                if !listed {
                    wizard.picked = None;
                    return vec![Reply::Show];
                }
                if step == Step::Source {
                    if wizard.source != wizard.picked {
                        wizard.target = None;
                    }
                    wizard.source = wizard.picked.clone();
                    self.wizard_go(Step::Target);
                } else {
                    wizard.target = wizard.picked.clone();
                    self.wizard_go(Step::Copy);
                    self.propose_name();
                }
                vec![Reply::Show]
            }
            Step::Copy => {
                self.wizard_go(Step::Name);
                vec![Reply::Show]
            }
            Step::Name => self.wizard_create(),
        }
    }

    /// Propose `<source>-to-<target>`, unless the user named it.
    fn propose_name(&mut self) {
        let Some((source, target)) = self
            .wizard
            .as_ref()
            .and_then(|w| Some((w.source.clone()?, w.target.clone()?)))
        else {
            return;
        };
        let (source_key, target_key, _) = self.wizard_keys(&source, &target);
        let names = self.settings.sync_names();
        let base = format!("{source_key}-to-{target_key}");
        let proposal = (1..)
            .map(|n| match n {
                1 => base.clone(),
                n => format!("{base}_{n}"),
            })
            .find(|n| !names.contains(n))
            .unwrap_or(base);
        if let Some(wizard) = self.wizard.as_mut()
            && (wizard.name.is_empty() || wizard.name == wizard.proposed)
        {
            wizard.name = proposal.clone();
            wizard.proposed = proposal;
        }
    }

    fn wizard_create(&mut self) -> Vec<Reply> {
        let Some((source, target, name, preset)) = self.wizard.as_ref().and_then(|w| {
            Some((
                w.source.clone()?,
                w.target.clone()?,
                w.name.trim().to_string(),
                w.preset,
            ))
        }) else {
            return vec![Reply::Show];
        };
        if let Some(e) = sync_name_error(&name) {
            self.error = Some(e.to_string());
            return vec![Reply::Show];
        }
        if self.settings.sync_names().contains(&name) {
            self.error = Some(format!("A sync named \"{name}\" exists already"));
            return vec![Reply::Show];
        }
        let (source_key, target_key, mut edits) = self.wizard_keys(&source, &target);
        let source_name = self.calendar_own_name(&source);
        edits.push(Edit::SetSync(preset.apply(
            capycal::SyncConfig::new(&name, &source_key, &target_key),
            &source_name,
        )));
        if let Some(wizard) = self.wizard.as_mut() {
            wizard.waiting = true;
        }
        self.after_save = Some(Selection::Sync(name));
        self.begin_save(SettingsChange {
            edits,
            ..SettingsChange::default()
        })
    }

    fn rebase_picker(&mut self, config: &Config) -> Vec<Reply> {
        self.picker.rebase(config);
        let mut replies: Vec<Reply> = self
            .picker
            .accounts_to_discover()
            .into_iter()
            .map(|a| Reply::Send(Command::DiscoverAccount(a)))
            .collect();
        replies.push(Reply::Show);
        replies
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use capycal::{Config, ConfigEditor, DiscoveredCalendar};
    use std::path::{Path, PathBuf};

    const CONFIG: &str = r#"[accounts.work]
url = "https://x/"
username = "u"
password = { keychain = "work" }

[accounts.spare]
url = "https://y/"
username = "u"
password = { keychain = "spare" }

[calendars.a]
account = "work"
url = "https://x/a/"

[calendars.b]
account = "work"
url = "https://x/b/"

[calendars.c]
account = "work"
url = "https://x/c/"

[syncs.a-to-b]
source = "a"
target = "b"
interval = "5m"
fields = ["SUMMARY"]

[syncs.c-to-b]
source = "c"
target = "b"
interval = "5m"
"#;

    fn load(path: &Path) -> (Config, ConfigEditor) {
        let editor = ConfigEditor::load(path).unwrap();
        (editor.config().unwrap(), editor)
    }

    fn screen(
        name: &str,
        text: &str,
        calendar_app: bool,
    ) -> (SettingsScreen, Vec<Command>, PathBuf) {
        let dir = std::env::temp_dir().join(format!("capycal-{}-view-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("capycal.toml");
        std::fs::write(&path, text).unwrap();
        let (config, editor) = load(&path);
        let (screen, commands) = SettingsScreen::new(&config, &editor, calendar_app);
        (screen, commands, path)
    }

    /// The footer's text and whether it is a warning.
    fn footer(view: &SettingsView) -> (String, bool) {
        view.controls
            .iter()
            .find_map(|c| match c {
                Control::Footer { text, warning, .. } => Some((text.clone(), *warning)),
                _ => None,
            })
            .expect("a footer")
    }

    fn find<'a>(view: &'a SettingsView, wanted: &FieldId) -> &'a Control {
        view.controls
            .iter()
            .find(|c| c.id() == Some(wanted))
            .unwrap_or_else(|| panic!("no {wanted:?} in {:#?}", view.controls))
    }

    /// Index and whether it is open, of every transform row.
    fn transform_rows(view: &SettingsView) -> Vec<(usize, bool)> {
        view.controls
            .iter()
            .filter_map(|c| match c {
                Control::TransformRow { index, open, .. } => Some((*index, *open)),
                _ => None,
            })
            .collect()
    }

    fn transform_summary(view: &SettingsView, wanted: usize) -> String {
        view.controls
            .iter()
            .find_map(|c| match c {
                Control::TransformRow { index, summary, .. } if *index == wanted => {
                    Some(summary.clone())
                }
                _ => None,
            })
            .unwrap_or_default()
    }

    fn side(items: &[SidebarItem]) -> Vec<(String, bool)> {
        items.iter().map(|i| (i.name.clone(), i.selected)).collect()
    }

    fn button(view: &SettingsView, wanted: FieldId) -> bool {
        view.controls
            .iter()
            .find_map(|c| match c {
                Control::Buttons(buttons) => buttons
                    .iter()
                    .find(|(id, _, _)| *id == wanted)
                    .map(|(_, _, on)| *on),
                Control::Footer { left, right, .. } => left
                    .iter()
                    .chain(right)
                    .find(|(id, _, _)| *id == wanted)
                    .map(|(_, _, on)| *on),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no button {wanted:?}"))
    }

    fn popup_titles(view: &SettingsView, id: FieldId) -> Vec<String> {
        match find(view, &id) {
            Control::Popup { items, .. } => items.iter().map(|i| i.title.clone()).collect(),
            other => panic!("{other:?}"),
        }
    }

    fn found(href: &str, writable: bool) -> DiscoveredCalendar {
        DiscoveredCalendar {
            href: href.into(),
            url: format!("https://x{href}"),
            display_name: Some(href.trim_matches('/').to_string()),
            color: None,
            writable,
        }
    }

    fn shown(replies: &[Reply]) -> bool {
        replies.iter().any(|r| matches!(r, Reply::Show))
    }

    #[test]
    fn test_sidebar_and_sync_form_rows() {
        let (s, _, _) = screen("rows", CONFIG, false);
        let view = s.current_view();
        assert_eq!(
            side(&view.syncs),
            [("a-to-b".to_string(), true), ("c-to-b".to_string(), false)]
        );
        assert_eq!(
            side(&view.accounts),
            [("spare".to_string(), false), ("work".to_string(), false)]
        );
        let ids: Vec<&FieldId> = view
            .controls
            .iter()
            .filter_map(Control::id)
            .take(6)
            .collect();
        assert_eq!(
            ids,
            [
                &FieldId::SyncName,
                &FieldId::Source,
                &FieldId::IntervalPreset,
                &FieldId::CopyPreset,
                &FieldId::Target,
                &FieldId::Advanced
            ]
        );
        // Only SUMMARY: the "Title" preset, no checkboxes
        assert!(matches!(
            find(&view, &FieldId::CopyPreset),
            Control::Segments {
                selected: Some(1),
                ..
            }
        ));
        assert!(
            !view
                .controls
                .iter()
                .any(|c| c.id() == Some(&FieldId::Field("SUMMARY")))
        );
        assert!(!button(&view, FieldId::Save));
        assert!(!button(&view, FieldId::Revert));
        assert!(button(&view, FieldId::Delete));
    }

    #[test]
    fn test_transform_rows_move_with_their_editor() {
        let (mut s, _, _) = screen("rows-move", CONFIG, false);
        s.input(SettingsInput::Chose(
            FieldId::InsertTransform(usize::MAX),
            4,
            None,
        )); // Remove field
        s.input(SettingsInput::Chose(
            FieldId::InsertTransform(usize::MAX),
            7,
            None,
        )); // Skip events
        assert_eq!(transform_rows(&s.current_view()), [(0, false), (1, true)]);
        let second = transform_summary(&s.current_view(), 1);
        // Dragged before the first
        s.input(SettingsInput::Moved(FieldId::TransformRow(1), 0));
        assert_eq!(transform_rows(&s.current_view()), [(0, true), (1, false)]);
        assert_eq!(transform_summary(&s.current_view(), 0), second);
        // Dropped next to itself: nothing moves
        s.input(SettingsInput::Moved(FieldId::TransformRow(0), 1));
        assert_eq!(transform_summary(&s.current_view(), 0), second);
        // Dragged to the end
        s.input(SettingsInput::Moved(FieldId::TransformRow(0), 2));
        assert_eq!(transform_rows(&s.current_view()), [(0, false), (1, true)]);
        assert_eq!(transform_summary(&s.current_view(), 1), second);
        s.input(SettingsInput::Pressed(FieldId::TransformRow(1)));
        assert_eq!(transform_rows(&s.current_view()), [(0, false), (1, false)]);
        s.input(SettingsInput::Pressed(FieldId::TransformRow(1)));
        s.input(SettingsInput::Pressed(FieldId::TransformRowRemove(0)));
        assert_eq!(transform_rows(&s.current_view()), [(0, true)]);
    }

    #[test]
    fn test_header_and_sidebar_show_how_a_sync_is_doing() {
        let (mut s, _, _) = screen("status", CONFIG, false);
        let now = chrono::DateTime::UNIX_EPOCH + chrono::TimeDelta::hours(1);
        let status = |s: &SettingsScreen| match find(&s.current_view(), &FieldId::SyncName) {
            Control::Header { status, .. } => status.clone(),
            _ => unreachable!(),
        };
        assert_eq!(status(&s), None, "no status known yet");
        let mut a = capycal::SyncStatus::new("a-to-b");
        a.last_run = Some(now - chrono::TimeDelta::minutes(2));
        a.last_result = Some(Ok(capycal::CycleStats {
            created: 3,
            deleted: 1,
            ..Default::default()
        }));
        assert!(s.set_statuses(std::slice::from_ref(&a), now));
        assert_eq!(
            status(&s),
            Some(("Synced 2 min ago: 3 new, 1 removed".into(), false))
        );
        assert!(!s.set_statuses(std::slice::from_ref(&a), now), "unchanged");
        a.last_result = Some(Err("server unreachable\ndetails".into()));
        s.set_statuses(&[a], now);
        assert_eq!(
            status(&s),
            Some(("The last sync failed: server unreachable".into(), true))
        );
        assert!(
            s.current_view()
                .syncs
                .iter()
                .any(|i| i.name == "a-to-b" && i.problem)
        );
        // A sync not saved yet has no status
        press(&mut s, FieldId::DuplicateSync("a-to-b".into()));
        assert_eq!(status(&s), None);
    }

    #[test]
    fn test_the_footer_follows_a_full_sync_of_the_shown_sync() {
        let (mut s, _, _) = screen("footer-full-sync", CONFIG, false);
        let now = chrono::DateTime::UNIX_EPOCH;
        let progress = |s: &SettingsScreen| {
            s.current_view().controls.iter().find_map(|c| match c {
                Control::Footer { progress, .. } => Some(*progress),
                _ => None,
            })
        };
        let mut a = capycal::SyncStatus::new("a-to-b");
        a.running = true;
        a.full_sync = Some(capycal::FullSyncProgress::default());
        s.set_statuses(std::slice::from_ref(&a), now);
        assert_eq!(
            footer(&s.current_view()),
            ("Full sync of \"a-to-b\": reading the events…".into(), false)
        );
        assert_eq!(progress(&s), Some(None));
        a.full_sync = Some(capycal::FullSyncProgress {
            done: 9,
            total: Some(4),
        });
        s.set_statuses(std::slice::from_ref(&a), now);
        assert_eq!(
            footer(&s.current_view()).0,
            "Full sync of \"a-to-b\"… 4 of 4 changes"
        );
        assert_eq!(progress(&s), Some(Some((4, 4))));
        // Unsaved changes come first; another sync's full sync is not shown
        s.input(SettingsInput::Text(FieldId::Interval, "1d".into()));
        assert_eq!(footer(&s.current_view()).0, "Unsaved changes");
        assert_eq!(progress(&s), Some(None));
        press(&mut s, FieldId::Revert);
        press(&mut s, FieldId::SidebarSync("c-to-b".into()));
        assert_eq!(footer(&s.current_view()).0, "");
        a.running = false;
        a.full_sync = None;
        s.set_statuses(&[a], now);
        press(&mut s, FieldId::SidebarSync("a-to-b".into()));
        assert_eq!(footer(&s.current_view()).0, "");
    }

    #[test]
    fn test_shortcuts_undo_and_the_draft_mark() {
        let (mut s, _, _) = screen("shortcuts", CONFIG, false);
        let draft = |s: &SettingsScreen| s.current_view().syncs.iter().any(|i| i.draft);
        // Save is off: Cmd+S does nothing
        assert!(s.input(SettingsInput::Shortcut(Shortcut::Save)).is_empty());
        s.input(SettingsInput::Chose(
            FieldId::InsertTransform(usize::MAX),
            6,
            None,
        ));
        assert_eq!(transform_rows(&s.current_view()).len(), 1);
        assert!(draft(&s), "a change marks the entry");
        s.input(SettingsInput::Pressed(FieldId::TransformRowRemove(0)));
        assert!(transform_rows(&s.current_view()).is_empty());
        // Undo brings the step back, then takes the insert back
        s.input(SettingsInput::Shortcut(Shortcut::Undo));
        assert_eq!(transform_rows(&s.current_view()).len(), 1);
        s.input(SettingsInput::Shortcut(Shortcut::Undo));
        assert!(transform_rows(&s.current_view()).is_empty());
        assert!(!draft(&s));
        assert!(s.input(SettingsInput::Shortcut(Shortcut::Undo)).is_empty());
        // Cmd+Delete asks like Delete
        assert!(matches!(
            s.input(SettingsInput::Shortcut(Shortcut::Delete))[..],
            [Reply::Ask { .. }, ..]
        ));
        s.input(SettingsInput::Pressed(FieldId::Revert));
        // Cmd+Option+Down moves the open step
        s.input(SettingsInput::Chose(
            FieldId::InsertTransform(usize::MAX),
            6,
            None,
        ));
        s.input(SettingsInput::Chose(
            FieldId::InsertTransform(usize::MAX),
            7,
            None,
        ));
        s.input(SettingsInput::Pressed(FieldId::TransformRow(0)));
        let first = transform_summary(&s.current_view(), 0);
        s.input(SettingsInput::Shortcut(Shortcut::MoveDown));
        assert_eq!(transform_summary(&s.current_view(), 1), first);
        assert_eq!(transform_rows(&s.current_view()), [(0, false), (1, true)]);
        s.input(SettingsInput::Shortcut(Shortcut::MoveUp));
        assert_eq!(transform_summary(&s.current_view(), 0), first);
        s.input(SettingsInput::Pressed(FieldId::Revert));
        // Cmd+N starts the assistant
        s.input(SettingsInput::Shortcut(Shortcut::NewSync));
        assert!(s.current_view().compact);
    }

    #[test]
    fn test_the_preview_can_show_the_next_real_event() {
        let (mut s, _, _) = screen("real-preview", CONFIG, false);
        let preview = |s: &SettingsScreen| {
            s.current_view()
                .controls
                .into_iter()
                .find_map(|c| match c {
                    Control::Preview {
                        mode,
                        source_label,
                        source,
                        ..
                    } => Some((mode, source_label, source.title)),
                    _ => None,
                })
                .unwrap()
        };
        assert_eq!(preview(&s).0, Some(0), "the sample first");
        let replies = s.input(SettingsInput::Selected(FieldId::PreviewMode, 1));
        assert!(replies.iter().any(|r| matches!(
            r,
            Reply::Send(Command::NextSourceEvent { sync, .. }) if sync.source == "a"
        )));
        assert!(preview(&s).1.starts_with("Reading the next event"));
        let ical = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:x\r\nDTSTART:20261020T140000\r\n\
            DTEND:20261020T150000\r\nSUMMARY:Dentist\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        s.update(
            &Update::NextSourceEvent {
                source: "a".into(),
                result: Ok(Some(ical.into())),
            },
            None,
        );
        let (mode, label, title) = preview(&s);
        assert_eq!((mode, title.as_str()), (Some(1), "Dentist"));
        assert!(label.starts_with("Next in"), "{label}");
        // Asked once per source, again when chosen again
        assert!(
            !s.input(SettingsInput::Text(FieldId::Interval, "10m".into()))
                .iter()
                .any(|r| matches!(r, Reply::Send(_)))
        );
        assert!(
            s.input(SettingsInput::Selected(FieldId::PreviewMode, 1))
                .iter()
                .any(|r| matches!(r, Reply::Send(_)))
        );
        s.input(SettingsInput::Selected(FieldId::PreviewMode, 0));
        assert_eq!(preview(&s).2, "Design review");
    }

    #[test]
    fn test_common_steps_come_filled_in() {
        let (mut s, _, _) = screen("templates", CONFIG, false);
        let Control::Joint { items, .. } = s
            .current_view()
            .controls
            .into_iter()
            .find(|c| matches!(c, Control::Joint { id: Some(_), .. }))
            .unwrap()
        else {
            unreachable!()
        };
        let travel = items
            .iter()
            .position(|i| i.title.starts_with("Travel time"))
            .unwrap();
        assert!(items[travel - 2].header, "under a header");
        // Each one says what it does
        assert!(items.iter().filter(|i| !i.header).all(|i| i.hint.is_some()));
        assert_eq!(
            items[travel].hint.as_deref(),
            Some("Blocks 30 minutes before and after each event")
        );
        s.input(SettingsInput::Chose(
            FieldId::InsertTransform(usize::MAX),
            travel,
            None,
        ));
        let view = s.current_view();
        assert_eq!(transform_rows(&view), [(0, false)]);
        assert_eq!(
            transform_summary(&view, 0),
            "Add buffer: 30 min before, 30 min after"
        );
        assert!(button(&view, FieldId::Save));
        // "Busy" names the source calendar
        s.input(SettingsInput::Chose(
            FieldId::InsertTransform(usize::MAX),
            travel - 1,
            None,
        ));
        assert_eq!(
            transform_summary(&s.current_view(), 1),
            "Set SUMMARY to \"Busy (a)\""
        );
        let hours = items
            .iter()
            .position(|i| i.title.starts_with("Skip outside working hours"))
            .unwrap();
        s.input(SettingsInput::Chose(
            FieldId::InsertTransform(usize::MAX),
            hours,
            None,
        ));
        assert_eq!(
            transform_summary(&s.current_view(), 2),
            "Skip events if it starts Mon-Fri until 07:59 and from 18:00"
        );
    }

    /// The text of the problem on General, if any.
    fn problem_text(view: &SettingsView) -> Option<String> {
        let at = view
            .controls
            .iter()
            .position(|c| *c == Control::Group("Needs attention".to_string()))?;
        match &view.controls[at + 1] {
            Control::Error(text) => Some(text.clone()),
            _ => None,
        }
    }

    #[test]
    fn test_config_errors_in_one_line() {
        assert_eq!(
            config_error(
                "configuration error: failed to parse /x/c.toml: TOML parse error at line 3, \
                 column 14\n  |\n3 | [syncs.broken\n  |             ^\nunclosed table, expected `]`\n"
            ),
            "line 3, column 14: unclosed table, expected `]`"
        );
        assert_eq!(
            config_error("configuration error: sync 'a': unknown calendar 'x'"),
            "sync 'a': unknown calendar 'x'"
        );
    }

    #[test]
    fn test_a_broken_config_opens_general_until_it_is_fixed() {
        let (_, _, path) = screen("broken", CONFIG, false);
        let mut s = SettingsScreen::broken(&path, "line 3: expected `=`".to_string(), false);
        let view = s.current_view();
        assert!(view.general.selected && view.general.problem);
        assert!(view.syncs.is_empty());
        assert!(
            problem_text(&view)
                .unwrap()
                .contains("line 3: expected `=`")
        );
        assert!(button(&view, FieldId::OpenConfig));
        // Nothing writes the empty config over the file
        assert!(matches!(
            &s.input(SettingsInput::Chose(FieldId::LogLevel, 1, None))[..],
            [Reply::Show]
        ));
        s.input(SettingsInput::Pressed(FieldId::AddSync));
        assert!(s.wizard.is_none());
        assert!(!s.set_problem(None), "stays until the file is read");
        // Fixed on disk: the whole window
        let (config, editor) = load(&path);
        s.update(&Update::ConfigChanged, Some((&config, &editor)));
        let view = s.current_view();
        assert!(!view.syncs.is_empty());
        assert!(!view.general.problem);
        assert_eq!(problem_text(&view), None);
    }

    #[test]
    fn test_a_problem_shows_on_general_with_its_way_out() {
        let (mut s, _, _) = screen("problem", CONFIG, false);
        press(&mut s, FieldId::SidebarGeneral);
        assert!(s.set_problem(Some(&Problem::Locked { pid: "42".into() })));
        let view = s.current_view();
        assert!(view.general.problem);
        assert!(problem_text(&view).unwrap().contains("pid 42"));
        assert!(matches!(
            &s.input(SettingsInput::Pressed(FieldId::ReloadConfig))[..],
            [Reply::Send(Command::Reload)]
        ));
        // A missing config is a state, not a problem
        assert!(s.set_problem(Some(&Problem::NoConfig)));
        assert_eq!(problem_text(&s.current_view()), None);
        assert!(matches!(
            &s.input(SettingsInput::Pressed(FieldId::ShowLog))[..],
            [Reply::ShowLog]
        ));
    }

    #[test]
    fn test_reopening_shows_the_last_entry() {
        let (mut s, _, _) = screen("restore", CONFIG, false);
        press(&mut s, FieldId::SidebarSync("c-to-b".into()));
        let last = s.selection();
        let (mut again, _, _) = screen("restore", CONFIG, false);
        again.restore(last);
        assert_eq!(again.selection(), Selection::Sync("c-to-b".into()));
        again.restore(Selection::Sync("gone".into()));
        assert_eq!(again.selection(), Selection::Sync("c-to-b".into()));
    }

    #[test]
    fn test_the_start_step_explains_the_access_question() {
        let (mut s, _, _) = screen("access", "", true);
        assert!(
            s.current_view()
                .controls
                .contains(&Control::Note(String::new()))
        );
        s.input(SettingsInput::Selected(FieldId::WizardList, 0));
        assert!(
            s.current_view()
                .controls
                .contains(&Control::Note(CALENDAR_ACCESS.to_string()))
        );
    }

    #[test]
    fn test_quit_asks_only_with_a_draft() {
        let (mut s, _, _) = screen("quit", CONFIG, false);
        assert!(matches!(
            s.input(SettingsInput::Shortcut(Shortcut::Quit))[..],
            [Reply::Quit]
        ));
        s.input(SettingsInput::Text(FieldId::Interval, "10m".into()));
        let replies = s.input(SettingsInput::Shortcut(Shortcut::Quit));
        let [Reply::Ask { then, .. }] = &replies[..] else {
            panic!("{replies:?}")
        };
        assert!(matches!(
            s.input(SettingsInput::Confirmed(then.clone()))[..],
            [Reply::Quit]
        ));
        assert!(matches!(
            s.input(SettingsInput::Shortcut(Shortcut::Close))[..],
            [Reply::Ask { .. }, ..]
        ));
    }

    #[test]
    fn test_general_sets_login_log_level_and_shows_the_config() {
        let (mut s, _, path) = screen("general", CONFIG, false);
        s.set_app_info(false, &path);
        assert!(!s.current_view().general.selected);
        press(&mut s, FieldId::SidebarGeneral);
        let view = s.current_view();
        assert!(view.general.selected);
        assert!(
            view.controls
                .contains(&Control::Note(path.display().to_string()))
        );
        assert!(matches!(
            s.input(SettingsInput::Toggled(FieldId::StartAtLogin, true))[..],
            [Reply::SetStartAtLogin(true), ..]
        ));
        assert!(matches!(
            find(&s.current_view(), &FieldId::StartAtLogin),
            Control::Check { on: true, .. }
        ));
        assert!(matches!(
            s.input(SettingsInput::Pressed(FieldId::RevealConfig))[..],
            [Reply::RevealConfig]
        ));
        // The ways to sponsor, quietly at the end
        assert!(s.current_view().controls.iter().any(|c| matches!(
            c,
            Control::Buttons(b) if b.iter().map(|(id, ..)| id.clone()).eq(
                (0..crate::sponsor::OPTIONS.len()).map(FieldId::Sponsor)
            )
        )));
        assert!(matches!(
            s.input(SettingsInput::Pressed(FieldId::Sponsor(1)))[..],
            [Reply::OpenUrl("https://ko-fi.com/thomaswiebe")]
        ));
        // Info is the default; Debug is written, then shown
        let level = |s: &SettingsScreen| match find(&s.current_view(), &FieldId::LogLevel) {
            Control::Popup { selected, .. } => *selected,
            _ => unreachable!(),
        };
        assert_eq!(level(&s), Some(2));
        let replies = s.input(SettingsInput::Chose(FieldId::LogLevel, 3, None));
        let [Reply::Send(Command::SaveSettings(change)), ..] = &replies[..] else {
            panic!("{replies:?}")
        };
        assert_eq!(change.edits, [Edit::SetLogLevel("debug".into())]);
        let mut editor = ConfigEditor::load(&path).unwrap();
        editor.set_log_level("debug");
        editor.save().unwrap();
        let (config, editor) = load(&path);
        s.update(&Update::ConfigChanged, Some((&config, &editor)));
        s.update(&Update::SettingsSaved(Ok(())), Some((&config, &editor)));
        assert!(s.current_view().general.selected, "General stays shown");
        assert_eq!(level(&s), Some(3));
    }

    #[test]
    fn test_general_shows_updates_only_when_set() {
        let (mut s, _, path) = screen("updates", CONFIG, false);
        s.set_app_info(false, &path);
        press(&mut s, FieldId::SidebarGeneral);
        let has = |s: &SettingsScreen, id: FieldId| {
            s.current_view().controls.iter().any(|c| match c {
                Control::Check { id: i, .. } => *i == id,
                Control::Buttons(b) => b.iter().any(|(i, ..)| *i == id),
                _ => false,
            })
        };
        assert!(
            !has(&s, FieldId::CheckUpdates),
            "the GTK app has no update check"
        );
        s.set_updates(Some(UpdatesInfo {
            enabled: true,
            checking: false,
            note: None,
            manager: None,
        }));
        assert!(matches!(
            find(&s.current_view(), &FieldId::CheckUpdates),
            Control::Check { on: true, .. }
        ));
        assert!(matches!(
            s.input(SettingsInput::Toggled(FieldId::CheckUpdates, false))[..],
            [Reply::SetCheckUpdates(false), Reply::Show]
        ));
        assert!(matches!(
            find(&s.current_view(), &FieldId::CheckUpdates),
            Control::Check { on: false, .. }
        ));
        assert!(matches!(
            s.input(SettingsInput::Pressed(FieldId::CheckUpdatesNow))[..],
            [Reply::CheckUpdatesNow]
        ));
        let note = |s: &SettingsScreen| {
            let controls = s.current_view().controls;
            let at = controls.iter().position(
                |c| matches!(c, Control::Buttons(b) if b[0].0 == FieldId::CheckUpdatesNow),
            );
            let Some(Control::Buttons(b)) = at.map(|i| &controls[i]) else {
                panic!("no Check Now")
            };
            let enabled = b[0].2;
            let Some(Control::Note(note)) = at.map(|i| controls[i - 1].clone()) else {
                panic!("no note")
            };
            (enabled, note)
        };
        // Switched off above
        assert_eq!(
            note(&s),
            (
                true,
                "Capycal looks for a new release only when you press Check Now.".to_string()
            )
        );
        let off = |manager: Option<&str>| UpdatesInfo {
            enabled: false,
            checking: false,
            note: None,
            manager: manager.map(String::from),
        };
        s.set_updates(Some(off(None)));
        assert_eq!(
            note(&s).1,
            "Capycal looks for a new release only when you press Check Now."
        );
        s.set_updates(Some(off(Some("Homebrew"))));
        assert_eq!(
            note(&s).1,
            "Installed with Homebrew, which keeps Capycal up to date."
        );
        s.set_updates(Some(UpdatesInfo {
            checking: true,
            ..off(None)
        }));
        assert_eq!(
            note(&s),
            (false, "Checking for updates\u{2026}".to_string())
        );
        s.set_updates(Some(UpdatesInfo {
            note: Some("Capycal 0.2.0 is the latest version.".into()),
            ..off(Some("Homebrew"))
        }));
        assert_eq!(
            note(&s),
            (true, "Capycal 0.2.0 is the latest version.".to_string())
        );
    }

    #[test]
    fn test_cancelling_the_assistant_shows_the_entry_from_before() {
        let (mut s, _, _) = screen("wizard-back", CONFIG, false);
        press(&mut s, FieldId::SidebarSync("c-to-b".into()));
        press(&mut s, FieldId::AddSync);
        assert!(s.wizard_view().is_some_and(|w| w.compact));
        // Settings stays as it was behind the wizard
        let behind = s.view();
        assert!(!behind.compact);
        assert_eq!(behind.title, "Settings");
        assert!(find(&behind, &FieldId::SyncName).id().is_some());
        assert_eq!(s.selection(), Selection::Sync("c-to-b".into()));
        press(&mut s, FieldId::WizardCancel);
        assert!(s.wizard_view().is_none());
        assert_eq!(s.selection(), Selection::Sync("c-to-b".into()));
    }

    #[test]
    fn test_duplicate_copies_a_sync_as_a_new_draft() {
        let (mut s, _, _) = screen("duplicate", CONFIG, false);
        press(&mut s, FieldId::DuplicateSync("a-to-b".into()));
        let view = s.current_view();
        assert!(matches!(
            find(&view, &FieldId::SyncName),
            Control::Header { title, .. } if title == "a-to-b-copy"
        ));
        assert!(!button(&view, FieldId::Delete), "not saved yet");
        assert!(button(&view, FieldId::Save), "ready to save");
    }

    #[test]
    fn test_the_wizard_hands_its_picks_to_the_editor() {
        let (mut s, _, _) = screen("to-editor", CONFIG, false);
        press(&mut s, FieldId::AddSync);
        s.update(
            &Update::AccountCalendars {
                account: "work".into(),
                result: Ok(vec![found("/a/", true), found("/b/", true)]),
            },
            None,
        );
        let view = s.current_view();
        assert!(button(&view, FieldId::AddEmptySync), "the way out");
        let a = row(&view, "a");
        s.input(SettingsInput::Selected(FieldId::WizardList, a));
        press(&mut s, FieldId::AddEmptySync);
        let view = s.current_view();
        assert!(
            !view
                .controls
                .iter()
                .any(|c| c.id() == Some(&FieldId::WizardList))
        );
        assert!(!button(&view, FieldId::Delete), "a new sync");
        let Control::Popup {
            items, selected, ..
        } = find(&view, &FieldId::Source)
        else {
            unreachable!()
        };
        assert_eq!(items[selected.unwrap()].title, "a");
        // Named after its calendars once both are picked, until typed
        let titles = popup_titles(&s.current_view(), FieldId::Target);
        let c = titles.iter().position(|t| t == "c").expect("listed");
        s.input(SettingsInput::Chose(FieldId::Target, c, Some("c".into())));
        assert!(matches!(
            find(&s.current_view(), &FieldId::SyncName),
            Control::Header { title, .. } if title == "a-to-c"
        ));
        // The wizard's default: only the time, as "Busy"
        assert_eq!(transform_rows(&view).len(), 1);
    }

    #[test]
    fn test_a_new_calendar_shows_its_name_once_picked() {
        let (mut s, _, _) = screen("new-calendar", CONFIG, false);
        s.update(
            &Update::AccountCalendars {
                account: "work".into(),
                result: Ok(vec![found("/fresh/", true)]),
            },
            None,
        );
        let titles = popup_titles(&s.current_view(), FieldId::Source);
        let i = titles.iter().position(|t| t == "fresh").expect("listed");
        s.input(SettingsInput::Chose(
            FieldId::Source,
            i,
            Some("fresh".into()),
        ));
        let view = s.current_view();
        let Control::Popup {
            items, selected, ..
        } = find(&view, &FieldId::Source)
        else {
            unreachable!()
        };
        assert_eq!(items[selected.unwrap()].title, "fresh");
        assert_eq!(
            items.iter().filter(|i| i.title.contains("fresh")).count(),
            1
        );
    }

    #[test]
    fn test_header_sums_up_the_sync_and_delete_sits_in_the_footer() {
        let (mut s, _, _) = screen("summary", CONFIG, false);
        let subtitle = |s: &SettingsScreen| match find(&s.current_view(), &FieldId::SyncName) {
            Control::Header { subtitle, .. } => subtitle.clone(),
            _ => String::new(),
        };
        assert!(
            subtitle(&s).starts_with("Copies events from "),
            "{}",
            subtitle(&s)
        );
        assert!(subtitle(&s).ends_with(" every 5 min."), "{}", subtitle(&s));
        s.input(SettingsInput::Toggled(FieldId::Enabled, false));
        assert!(subtitle(&s).starts_with("Turned off"));
        let view = s.current_view();
        assert!(matches!(
            view.controls.last(),
            Some(Control::Footer { left, .. }) if left[0].0 == FieldId::Delete && left[0].1 == "Delete"
        ));
    }

    #[test]
    fn test_pipeline_steps_and_inserting_between_them() {
        let (mut s, _, _) = screen("pipeline", CONFIG, false);
        let steps = |s: &SettingsScreen| -> Vec<String> {
            s.current_view()
                .controls
                .iter()
                .filter_map(|c| match c {
                    Control::Stage { label, .. } => Some(label.clone()),
                    Control::Joint { id: Some(_), .. } => Some("+".to_string()),
                    _ => None,
                })
                .collect()
        };
        assert_eq!(steps(&s), ["From", "Keep", "+", "Into"]);
        // Remove field, then Set field before it
        s.input(SettingsInput::Chose(FieldId::InsertTransform(0), 4, None));
        s.input(SettingsInput::Chose(FieldId::InsertTransform(0), 1, None));
        assert_eq!(
            steps(&s),
            [
                "From",
                "Keep",
                "+",
                "Set field",
                "+",
                "Remove field",
                "+",
                "Into"
            ]
        );
        // The inserted one is open
        assert_eq!(transform_rows(&s.current_view()), [(0, true), (1, false)]);
        assert!(matches!(
            s.current_view().controls.last(),
            Some(Control::Footer { .. })
        ));
    }

    #[test]
    fn test_parallel_writes_of_a_caldav_target_under_advanced() {
        let (mut s, _, _) = screen("concurrency", CONFIG, false);
        let value = |s: &SettingsScreen| {
            s.current_view().controls.iter().find_map(|c| match c {
                Control::Text { id, value, .. } if *id == FieldId::Concurrency => {
                    Some(value.clone())
                }
                _ => None,
            })
        };
        assert_eq!(value(&s), None, "only under Advanced");
        s.input(SettingsInput::Pressed(FieldId::Advanced));
        assert_eq!(value(&s).as_deref(), Some("2"));
        s.input(SettingsInput::Text(FieldId::Concurrency, "4".into()));
        assert!(button(&s.current_view(), FieldId::Save));
    }

    #[test]
    fn test_interval_presets_and_custom_interval() {
        let (mut s, _, _) = screen("interval", CONFIG, false);
        let interval = |s: &SettingsScreen| match find(&s.current_view(), &FieldId::IntervalPreset)
        {
            Control::Segments { selected, .. } => *selected,
            _ => None,
        };
        let field = |s: &SettingsScreen| {
            s.current_view()
                .controls
                .iter()
                .any(|c| c.id() == Some(&FieldId::Interval))
        };
        assert_eq!(interval(&s), Some(1), "5m is the 5 min preset");
        assert!(!field(&s));
        s.input(SettingsInput::Selected(FieldId::IntervalPreset, 2));
        assert!(button(&s.current_view(), FieldId::Save));
        assert_eq!(interval(&s), Some(2));
        // Custom: typing a preset keeps the field
        s.input(SettingsInput::Selected(FieldId::IntervalPreset, 3));
        assert!(field(&s));
        s.input(SettingsInput::Text(FieldId::Interval, "5m".into()));
        assert!(field(&s));
        assert_eq!(interval(&s), Some(3));
    }

    #[test]
    fn test_troubleshooting_of_a_saved_sync() {
        let (mut s, _, _) = screen("troubleshooting", CONFIG, false);
        s.input(SettingsInput::Pressed(FieldId::Advanced));
        let buttons = |s: &SettingsScreen| {
            s.current_view().controls.into_iter().find_map(|c| match c {
                Control::Buttons(b) if b.iter().any(|(id, _, _)| *id == FieldId::FullSyncNow) => {
                    Some(b)
                }
                _ => None,
            })
        };
        let shown = buttons(&s).unwrap();
        assert_eq!(
            shown
                .iter()
                .map(|(_, t, on)| (t.as_str(), *on))
                .collect::<Vec<_>>(),
            [
                ("Full Sync Now", true),
                ("Remove Synced Events…", true),
                ("Remove All Events…", true)
            ]
        );
        let name = s.saved_sync_name().unwrap();
        assert!(matches!(
            &s.input(SettingsInput::Pressed(FieldId::FullSyncNow))[..],
            [Reply::Send(Command::FullSync(Some(n))), Reply::Show] if *n == name
        ));
        assert!(matches!(
            &s.input(SettingsInput::Pressed(FieldId::RemoveSyncedEvents))[..],
            [Reply::Purge(n)] if *n == name
        ));
        let target = s.settings.target_of(&name).unwrap().0;
        assert!(matches!(
            &s.input(SettingsInput::Pressed(FieldId::RemoveAllEvents))[..],
            [Reply::PurgeCalendar { calendar, name: shown, syncs }]
                if *calendar == target && *shown == s.calendar_title(&target)
                    && syncs.contains(&name)
        ));
        // A new sync has nothing to fix yet
        s.input(SettingsInput::Pressed(FieldId::AddEmptySync));
        s.input(SettingsInput::Pressed(FieldId::Advanced));
        assert!(s.saved_sync_name().is_none());
        assert!(buttons(&s).is_none());
    }

    #[test]
    fn test_calendar_app_range_only_for_a_calendar_app_source() {
        let (mut s, _, _) = screen("all-events", CONFIG, true);
        s.input(SettingsInput::Pressed(FieldId::Advanced));
        assert!(
            s.current_view()
                .controls
                .iter()
                .any(|c| matches!(c, Control::Note(t) if t.contains("Once a day, a full sync")))
        );
        let has_days = |s: &SettingsScreen| {
            s.current_view()
                .controls
                .iter()
                .any(|c| c.id() == Some(&FieldId::DaysPast))
        };
        // A CalDAV source has no window of days
        assert!(!has_days(&s));
        s.update(
            &Update::CalendarAppCalendars(Ok(vec![capycal::CalendarInfo {
                title: "Home".into(),
                source: Some("iCloud".into()),
                identifier: "home-id".into(),
                writable: true,
            }])),
            None,
        );
        let titles = popup_titles(&s.current_view(), FieldId::Source);
        let i = titles
            .iter()
            .position(|t| t == "Home (iCloud)")
            .expect("listed");
        s.input(SettingsInput::Chose(
            FieldId::Source,
            i,
            Some(titles[i].clone()),
        ));
        assert!(has_days(&s));
        // "Busy (Home)", not "Busy (Home (iCloud))"
        let source = s.settings.sync_form_ref().unwrap().source().to_string();
        let calendar = s.settings.calendar(&source).unwrap().clone();
        assert_eq!(s.calendar_name(&calendar), "Home (iCloud)");
        assert_eq!(s.calendar_own_name(&calendar), "Home");
        let view = s.current_view();
        // Both day fields share one row
        let row = view
            .controls
            .iter()
            .position(|c| *c == Control::Row("Read".into()))
            .expect("row");
        assert_eq!(view.controls[row + 1].id(), Some(&FieldId::DaysPast));
        assert_eq!(view.controls[row + 2].id(), Some(&FieldId::DaysFuture));
        assert_eq!(view.controls[row + 3], Control::RowEnd);
        // All events grey out the days
        let enabled = |s: &SettingsScreen| match find(&s.current_view(), &FieldId::DaysPast) {
            Control::Text { enabled, .. } => *enabled,
            _ => panic!("no days"),
        };
        assert!(enabled(&s));
        s.input(SettingsInput::Toggled(FieldId::AllEvents, true));
        assert!(!enabled(&s));
    }

    #[test]
    fn test_copy_presets_and_custom_fields() {
        let (mut s, _, _) = screen("presets", CONFIG, false);
        // Custom shows the checkboxes of the same fields
        s.input(SettingsInput::Selected(FieldId::CopyPreset, 3));
        let view = s.current_view();
        assert!(matches!(
            find(&view, &FieldId::Field("VALARM")),
            Control::Check { hint: Some(h), on: false, .. } if h == crate::sync_form::VALARM_HINT
        ));
        assert!(matches!(
            find(&view, &FieldId::Field("SUMMARY")),
            Control::Check { on: true, .. }
        ));
        assert!(!button(&view, FieldId::Save));
        // A preset replaces the fields; the preview follows
        s.input(SettingsInput::Selected(FieldId::CopyPreset, 0));
        let view = s.current_view();
        assert!(button(&view, FieldId::Save));
        assert!(view.controls.iter().any(|c| matches!(
            c,
            Control::Preview { copy: Some(copy), .. } if copy.title == "(no title)"
        )));
        s.input(SettingsInput::Selected(FieldId::CopyPreset, 2));
        assert!(s.current_view().controls.iter().any(|c| matches!(
            c,
            Control::Preview { copy: Some(copy), .. } if copy.title == "Design review" && copy.detail.contains("Room 3")
        )));
    }

    #[test]
    fn test_popups_follow_discovery() {
        let (mut s, commands, _) = screen("popups", CONFIG, true);
        let commands: Vec<String> = commands.iter().map(|c| format!("{c:?}")).collect();
        assert_eq!(
            commands,
            [
                "DiscoverAccount(\"spare\")",
                "DiscoverAccount(\"work\")",
                "ListCalendarApp"
            ]
        );
        let view = s.current_view();
        assert_eq!(
            popup_titles(&view, FieldId::Source),
            [
                "spare",
                "Loading…",
                "work",
                "Loading…",
                "a",
                "b",
                "c",
                "Calendar.app",
                "Loading…",
                "Add Account…",
                "Add Calendar Feed…"
            ]
        );
        assert!(!popup_titles(&view, FieldId::Target).contains(&ADD_FEED.to_string()));
        let replies = s.update(
            &Update::AccountCalendars {
                account: "work".into(),
                result: Err("offline".into()),
            },
            None,
        );
        assert!(shown(&replies));
        let titles = popup_titles(&s.current_view(), FieldId::Source);
        assert_eq!(titles[2..7], ["work", "offline", "a", "b", "c"]);
        s.update(
            &Update::AccountCalendars {
                account: "work".into(),
                result: Ok(vec![
                    found("/a/", true),
                    found("/b/", true),
                    found("/ro/", false),
                ]),
            },
            None,
        );
        let view = s.current_view();
        let Control::Popup {
            items, selected, ..
        } = find(&view, &FieldId::Target)
        else {
            unreachable!()
        };
        let ro = items.iter().find(|i| i.title == "ro").unwrap();
        assert!(!ro.enabled);
        assert_eq!(ro.hint.as_deref(), Some("Read-only"));
        assert_eq!(items[selected.unwrap()].title, "b");
    }

    #[test]
    fn test_typing_keeps_the_shape_and_adding_a_transform_changes_it() {
        let (mut s, _, _) = screen("shape", CONFIG, false);
        s.input(SettingsInput::Selected(FieldId::IntervalPreset, 3));
        let before = s.current_view();
        assert!(shown(
            &s.input(SettingsInput::Text(FieldId::Interval, "1h".into()))
        ));
        let after = s.current_view();
        assert!(SettingsScreen::same_shape(&before, &after));
        assert!(button(&after, FieldId::Save));
        s.input(SettingsInput::Chose(
            FieldId::InsertTransform(usize::MAX),
            1,
            None,
        ));
        let added = s.current_view();
        assert!(!SettingsScreen::same_shape(&after, &added));
        assert_eq!(transform_rows(&added), [(0, true)]);
        assert!(matches!(
            find(&added, &FieldId::TransformField("value")),
            Control::Text { .. }
        ));
    }

    #[test]
    fn test_field_names_are_suggested_and_upper_case() {
        let (mut s, _, _) = screen("field-name", CONFIG, false);
        s.input(SettingsInput::Chose(
            FieldId::InsertTransform(usize::MAX),
            1,
            None,
        ));
        let view = s.current_view();
        let Control::Text {
            label, suggestions, ..
        } = find(&view, &FieldId::TransformField("field"))
        else {
            unreachable!()
        };
        assert_eq!(label, "Field");
        assert!(suggestions.iter().any(|f| f == "LOCATION"));
        s.input(SettingsInput::Text(
            FieldId::TransformField("field"),
            "x-Note".into(),
        ));
        assert!(matches!(
            find(&s.current_view(), &FieldId::TransformField("field")),
            Control::Text { value, .. } if value == "X-NOTE"
        ));
        // The value is no field name
        assert!(matches!(
            find(&s.current_view(), &FieldId::TransformField("value")),
            Control::Text { suggestions, .. } if suggestions.is_empty()
        ));
    }

    #[test]
    fn test_save_asks_once_then_sends_and_rebases() {
        let (mut s, _, path) = screen("save", CONFIG, false);
        s.input(SettingsInput::Text(FieldId::SyncName, "renamed".into()));
        let replies = s.input(SettingsInput::Pressed(FieldId::Save));
        let [Reply::Ask { confirm, then, .. }, ..] = &replies[..] else {
            panic!("{replies:?}")
        };
        assert_eq!(confirm.button, "Remove and Sync Again");
        let replies = s.input(SettingsInput::Confirmed(then.clone()));
        assert!(replies.iter().any(
            |r| matches!(r, Reply::Send(Command::SaveSettings(c)) if c.purge_first == ["a-to-b"])
        ));
        let view = s.current_view();
        assert!(!button(&view, FieldId::Save));
        assert_eq!(footer(&view), ("Saving…".into(), false));

        // Removing the copies shows how far it is
        let progress = |s: &SettingsScreen| {
            s.current_view().controls.iter().find_map(|c| match c {
                Control::Footer { progress, .. } => Some(*progress),
                _ => None,
            })
        };
        s.update(
            &Update::PurgeProgress {
                sync: "a-to-b".into(),
                done: 3,
                total: Some(4),
            },
            None,
        );
        assert_eq!(
            footer(&s.current_view()),
            (
                "Removing the synced events of \"a-to-b\"… 3 of 4".into(),
                false
            )
        );
        assert_eq!(progress(&s), Some(Some((3, 4))));
        s.update(
            &Update::PurgeProgress {
                sync: "a-to-b".into(),
                done: 9,
                total: Some(4),
            },
            None,
        );
        assert_eq!(progress(&s), Some(Some((4, 4))));

        // The backend writes and reloads; its own change raises no banner
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, text.replace("[syncs.a-to-b]", "[syncs.renamed]")).unwrap();
        let (config, editor) = load(&path);
        s.update(&Update::ConfigChanged, Some((&config, &editor)));
        s.update(&Update::SettingsSaved(Ok(())), Some((&config, &editor)));
        let view = s.current_view();
        assert_eq!(view.banner, None);
        assert_eq!(side(&view.syncs)[0], ("renamed".to_string(), true));
        assert!(!button(&view, FieldId::Save));
        assert_eq!(progress(&s), Some(None));
    }

    #[test]
    fn test_sidebar_shows_the_version() {
        let (s, _, _) = screen("version", CONFIG, false);
        assert_eq!(
            s.current_view().version,
            format!("Version {}", capycal::VERSION)
        );
    }

    #[test]
    fn test_failed_save_keeps_the_draft() {
        let (mut s, _, _) = screen("save-fails", CONFIG, false);
        s.input(SettingsInput::Text(FieldId::Interval, "1h".into()));
        s.input(SettingsInput::Pressed(FieldId::Save));
        s.update(&Update::SettingsSaved(Err("disk full".into())), None);
        let view = s.current_view();
        assert_eq!(footer(&view), ("disk full".into(), true));
        assert!(
            matches!(find(&view, &FieldId::Interval), Control::Text { value, .. } if value == "1h")
        );
        assert!(button(&view, FieldId::Save));
    }

    #[test]
    fn test_delete_offers_to_keep_the_copies() {
        let (mut s, _, _) = screen("delete", CONFIG, false);
        s.input(SettingsInput::Pressed(FieldId::SidebarSync(
            "c-to-b".into(),
        )));
        let replies = s.input(SettingsInput::Pressed(FieldId::Delete));
        let [
            Reply::Ask {
                alternative: Some((label, keep)),
                ..
            },
        ] = &replies[..]
        else {
            panic!("{replies:?}")
        };
        assert_eq!(*label, "Keep Copies");
        let replies = s.input(SettingsInput::Confirmed(Box::new(keep.clone())));
        assert!(replies.iter().any(|r| matches!(r, Reply::Send(Command::SaveSettings(c)) if c.forget_state == ["c-to-b"] && c.purge_first.is_empty())));
    }

    #[test]
    fn test_switching_away_from_a_draft_asks() {
        let (mut s, _, _) = screen("switch", CONFIG, false);
        s.input(SettingsInput::Text(FieldId::Interval, "1h".into()));
        let replies = s.input(SettingsInput::Pressed(FieldId::SidebarSync(
            "c-to-b".into(),
        )));
        let [Reply::Ask { confirm, then, .. }, ..] = &replies[..] else {
            panic!("{replies:?}")
        };
        assert_eq!(confirm.heading, "Discard changes?");
        s.input(SettingsInput::Confirmed(then.clone()));
        assert_eq!(
            side(&s.current_view().syncs)[1],
            ("c-to-b".to_string(), true)
        );
    }

    #[test]
    fn test_external_change_shows_the_banner() {
        let (mut s, _, path) = screen("banner", CONFIG, false);
        s.input(SettingsInput::Text(FieldId::Interval, "2h".into()));
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(
            &path,
            text.replacen("interval = \"5m\"", "interval = \"3h\"", 1),
        )
        .unwrap();
        let (config, editor) = load(&path);
        s.update(&Update::ConfigChanged, Some((&config, &editor)));
        let view = s.current_view();
        assert_eq!(view.banner.as_deref(), Some("Changed on disk"));
        assert!(
            matches!(&view.controls[0], Control::Buttons(b) if b.iter().map(|(id, _, _)| id.clone()).collect::<Vec<_>>() == [FieldId::Reload, FieldId::Overwrite])
        );
        s.input(SettingsInput::Pressed(FieldId::Reload));
        let view = s.current_view();
        assert_eq!(view.banner, None);
        assert!(
            matches!(find(&view, &FieldId::Interval), Control::Text { value, .. } if value == "3h")
        );
    }

    #[test]
    fn test_closing_asks_only_with_a_draft() {
        let (mut s, _, _) = screen("close", CONFIG, false);
        assert!(matches!(
            s.input(SettingsInput::CloseRequested)[..],
            [Reply::Close]
        ));
        s.input(SettingsInput::Text(FieldId::Interval, "1h".into()));
        assert!(matches!(
            s.input(SettingsInput::CloseRequested)[..],
            [Reply::Ask { .. }, ..]
        ));
    }

    #[test]
    fn test_account_form_test_and_delete() {
        let (mut s, _, _) = screen("account", CONFIG, false);
        s.input(SettingsInput::Pressed(FieldId::SidebarAccount(
            "work".into(),
        )));
        let view = s.current_view();
        assert!(matches!(
            find(&view, &FieldId::Password),
            Control::Text { secure: true, placeholder, .. } if placeholder == "Stored in the keychain"
        ));
        let replies = s.input(SettingsInput::Pressed(FieldId::Test));
        assert!(replies.iter().any(|r| matches!(r, Reply::Send(Command::TestAccount { account: Some(a), password: None, .. }) if a == "work")));
        s.update(&Update::AccountTested(Ok(3)), None);
        assert!(
            s.current_view()
                .controls
                .contains(&Control::Note("Found 3 calendars".into()))
        );

        s.input(SettingsInput::Pressed(FieldId::Delete));
        assert_eq!(
            footer(&s.current_view()),
            (
                "Used by the syncs a-to-b, c-to-b. Remove them first.".into(),
                true
            )
        );
    }

    #[test]
    fn test_sidebar_buttons_toggles_and_list_rows() {
        let (mut s, _, _) = screen("buttons", CONFIG, false);
        s.input(SettingsInput::Toggled(FieldId::Field("LOCATION"), true));
        s.input(SettingsInput::Chose(
            FieldId::InsertTransform(usize::MAX),
            4,
            None,
        )); // Remove field
        s.input(SettingsInput::Chose(
            FieldId::InsertTransform(usize::MAX),
            7,
            None,
        )); // Skip events
        s.input(SettingsInput::Pressed(FieldId::TransformRow(0)));
        let view = s.current_view();
        assert!(matches!(
            find(&view, &FieldId::Field("LOCATION")),
            Control::Check { on: true, .. }
        ));
        assert_eq!(transform_rows(&view), [(0, true), (1, false)]);
        assert!(view.controls.iter().any(|c| matches!(
            c,
            Control::TransformRow {
                index: 0,
                open: true,
                ..
            }
        )));

        // A draft blocks switching until confirmed
        assert!(matches!(
            s.input(SettingsInput::Pressed(FieldId::AddAccount))[..],
            [Reply::Ask { .. }, ..]
        ));
        s.input(SettingsInput::Confirmed(Box::new(SettingsInput::Pressed(
            FieldId::AddAccount,
        ))));
        assert!(
            matches!(find(&s.current_view(), &FieldId::AccountName), Control::Text { value, .. } if value.is_empty())
        );
        s.input(SettingsInput::Pressed(FieldId::AddSync));
        assert!(matches!(
            find(&s.current_view(), &FieldId::WizardList),
            Control::Choices { .. }
        ));
        s.input(SettingsInput::Pressed(FieldId::WizardCancel));

        // Delete asks
        s.input(SettingsInput::Pressed(FieldId::SidebarSync(
            "c-to-b".into(),
        )));
        assert!(matches!(
            s.input(SettingsInput::Pressed(FieldId::Delete))[..],
            [Reply::Ask {
                alternative: Some(_),
                ..
            }]
        ));
        assert!(!s.saving());
        s.input(SettingsInput::Confirmed(Box::new(SettingsInput::Pressed(
            FieldId::Delete,
        ))));
        assert!(s.saving());
    }

    #[test]
    fn test_invalid_input_and_discovery_keep_the_shape() {
        let (mut s, _, _) = screen("shape-errors", CONFIG, false);
        s.input(SettingsInput::Selected(FieldId::IntervalPreset, 3));
        let before = s.current_view();
        s.input(SettingsInput::Text(FieldId::Interval, "1".into()));
        // Typing "15m" passes "1": no mistake shows until the typing ends
        let typing = s.current_view();
        assert!(SettingsScreen::same_shape(&before, &typing));
        assert!(matches!(
            find(&typing, &FieldId::Interval),
            Control::Text { error: None, .. }
        ));
        assert!(!footer(&typing).1);
        assert!(!button(&typing, FieldId::Save));
        s.input(SettingsInput::Left(FieldId::Interval));
        let invalid = s.current_view();
        assert!(SettingsScreen::same_shape(&before, &invalid));
        assert!(matches!(
            find(&invalid, &FieldId::Interval),
            Control::Text { error: Some(_), .. }
        ));
        assert!(footer(&invalid).1);
        s.update(
            &Update::AccountCalendars {
                account: "work".into(),
                result: Ok(vec![found("/a/", true), found("/new/", true)]),
            },
            None,
        );
        assert!(SettingsScreen::same_shape(&invalid, &s.current_view()));
    }

    #[test]
    fn test_content_switches_are_never_in_place() {
        let (mut s, _, _) = screen("generation", CONFIG, false);
        let a = s.current_view();
        s.input(SettingsInput::Pressed(FieldId::SidebarSync(
            "c-to-b".into(),
        )));
        assert!(!SettingsScreen::same_shape(&a, &s.current_view()));
        s.input(SettingsInput::Text(FieldId::Interval, "1h".into()));
        let edited = s.current_view();
        s.input(SettingsInput::Pressed(FieldId::Revert));
        assert!(!SettingsScreen::same_shape(&edited, &s.current_view()));
    }

    #[test]
    fn test_a_confirmation_for_another_entry_is_ignored() {
        let (mut s, _, _) = screen("stale-confirm", CONFIG, false);
        let replies = s.input(SettingsInput::Pressed(FieldId::Delete));
        let Some(Reply::Ask { then, .. }) =
            replies.into_iter().find(|r| matches!(r, Reply::Ask { .. }))
        else {
            panic!("no question")
        };
        s.input(SettingsInput::Pressed(FieldId::SidebarSync(
            "c-to-b".into(),
        )));
        let replies = s.input(SettingsInput::Confirmed(then));
        assert!(
            !replies.iter().any(|r| matches!(r, Reply::Send(_))),
            "{replies:?}"
        );
        assert!(!s.saving());
    }

    #[test]
    fn test_external_change_during_a_failed_save_shows_the_banner() {
        let (mut s, _, path) = screen("failed-save-banner", CONFIG, false);
        s.input(SettingsInput::Text(FieldId::Interval, "2h".into()));
        s.input(SettingsInput::Pressed(FieldId::Save));
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(
            &path,
            text.replacen("interval = \"5m\"", "interval = \"3h\"", 1),
        )
        .unwrap();
        let (config, editor) = load(&path);
        s.update(&Update::ConfigChanged, Some((&config, &editor)));
        s.update(
            &Update::SettingsSaved(Err("no".into())),
            Some((&config, &editor)),
        );
        assert_eq!(s.current_view().banner.as_deref(), Some("Changed on disk"));
    }

    #[test]
    fn test_test_needs_a_url() {
        let (mut s, _, _) = screen("test-url", CONFIG, false);
        s.input(SettingsInput::Pressed(FieldId::AddAccount));
        assert!(!button(&s.current_view(), FieldId::Test));
        s.input(SettingsInput::Text(FieldId::Url, "https://x/".into()));
        assert!(button(&s.current_view(), FieldId::Test));
    }

    #[test]
    fn test_question_views() {
        let discard = question_view(&discard(), None);
        assert_eq!(discard.heading, "Discard changes?");
        assert_eq!(discard.buttons, ["Cancel", "Discard"]);
        assert!(discard.destructive);
        let (mut s, _, _) = screen("question", CONFIG, false);
        s.input(SettingsInput::Pressed(FieldId::SidebarSync(
            "c-to-b".into(),
        )));
        let replies = s.input(SettingsInput::Pressed(FieldId::Delete));
        let [
            Reply::Ask {
                confirm,
                alternative: Some((label, _)),
                ..
            },
            ..,
        ] = &replies[..]
        else {
            panic!("{replies:?}")
        };
        let view = question_view(confirm, Some(label));
        assert_eq!(view.buttons, ["Cancel", "Keep Copies", "Remove Sync"]);
    }

    #[test]
    fn test_disabled_popup_items_cannot_be_chosen() {
        let (mut s, _, _) = screen("disabled-items", CONFIG, false);
        s.update(
            &Update::AccountCalendars {
                account: "work".into(),
                result: Ok(vec![
                    found("/a/", true),
                    found("/b/", true),
                    found("/ro/", false),
                ]),
            },
            None,
        );
        let view = s.current_view();
        let Control::Popup { items, .. } = find(&view, &FieldId::Target) else {
            unreachable!()
        };
        let ro = items.iter().position(|i| i.title == "ro").unwrap();
        let header = items.iter().position(|i| i.header).unwrap();
        s.input(SettingsInput::Chose(FieldId::Target, ro, None));
        s.input(SettingsInput::Chose(FieldId::Target, header, None));
        let view = s.current_view();
        let Control::Popup {
            items, selected, ..
        } = find(&view, &FieldId::Target)
        else {
            unreachable!()
        };
        assert_eq!(items[selected.unwrap()].title, "b");
        assert!(!button(&view, FieldId::Save));
    }

    #[test]
    fn test_a_choice_of_an_item_that_changed_is_ignored() {
        let (mut s, _, _) = screen("stale-choice", CONFIG, false);
        let view = s.current_view();
        let Control::Popup { items, .. } = find(&view, &FieldId::Target) else {
            unreachable!()
        };
        let c = items.iter().position(|i| i.title == "c").unwrap();
        // The menu showed another item at this index when it was chosen
        s.input(SettingsInput::Chose(FieldId::Target, c, Some("b".into())));
        assert!(!button(&s.current_view(), FieldId::Save));
        s.input(SettingsInput::Chose(FieldId::Target, c, Some("c".into())));
        assert!(button(&s.current_view(), FieldId::Save));
    }

    #[test]
    fn test_skip_starts_with_a_condition() {
        let (mut s, _, _) = screen("skip", CONFIG, false);
        let skip = ACTIONS
            .iter()
            .position(|a| a.action == capycal::TransformAction::Skip)
            .unwrap();
        s.input(SettingsInput::Chose(
            FieldId::InsertTransform(usize::MAX),
            skip + 1,
            None,
        ));
        let view = s.current_view();
        assert!(matches!(
            find(&view, &FieldId::ConditionSubject(0)),
            Control::Popup {
                selected: Some(0),
                ..
            }
        ));
        assert!(matches!(
            find(&view, &FieldId::ConditionText(0, "value")),
            Control::Text { .. }
        ));
        assert!(button(&view, FieldId::AddCondition));
        // The only condition of a skip cannot be removed
        assert!(matches!(
            find(&view, &FieldId::ConditionSubject(0)),
            Control::Popup { buttons, .. } if buttons.is_empty()
        ));
        s.input(SettingsInput::Text(
            FieldId::ConditionText(0, "value"),
            "Lunch".into(),
        ));
        assert_eq!(
            transform_summary(&s.current_view(), 0),
            "Skip events if the title contains \"Lunch\""
        );
    }

    #[test]
    fn test_conditions_on_days_are_toggles() {
        let (mut s, _, _) = screen("weekdays", CONFIG, false);
        let skip = ACTIONS
            .iter()
            .position(|a| a.action == capycal::TransformAction::Skip)
            .unwrap();
        s.input(SettingsInput::Chose(
            FieldId::InsertTransform(usize::MAX),
            skip + 1,
            None,
        ));
        s.input(SettingsInput::Chose(FieldId::ConditionSubject(0), 4, None));
        let view = s.current_view();
        assert!(matches!(
            find(&view, &FieldId::ConditionDay(0, "mon")),
            Control::Check { on: false, .. }
        ));
        assert!(
            view.controls
                .iter()
                .all(|c| c.id() != Some(&FieldId::ConditionText(0, "value")))
        );
        s.input(SettingsInput::Toggled(
            FieldId::ConditionDay(0, "fri"),
            true,
        ));
        s.input(SettingsInput::Toggled(
            FieldId::ConditionDay(0, "mon"),
            true,
        ));
        let view = s.current_view();
        assert!(matches!(
            find(&view, &FieldId::ConditionDay(0, "fri")),
            Control::Check { on: true, .. }
        ));
        assert_eq!(
            transform_summary(&view, 0),
            "Skip events if it starts Mon, Fri"
        );
        s.input(SettingsInput::Chose(FieldId::ConditionTest(0), 1, None));
        s.input(SettingsInput::Text(
            FieldId::ConditionText(0, "start_time"),
            "18:00".into(),
        ));
        assert_eq!(
            transform_summary(&s.current_view(), 0),
            "Skip events if it does not start Mon, Fri from 18:00"
        );
    }

    #[test]
    fn test_conditions_are_added_and_removed() {
        let (mut s, _, _) = screen("conditions", CONFIG, false);
        let buffer = ACTIONS
            .iter()
            .position(|a| a.action == capycal::TransformAction::AddBuffer)
            .unwrap();
        s.input(SettingsInput::Chose(
            FieldId::InsertTransform(usize::MAX),
            buffer + 1,
            None,
        ));
        let view = s.current_view();
        assert!(
            view.controls
                .iter()
                .all(|c| c.id() != Some(&FieldId::ConditionSubject(0)))
        );
        s.input(SettingsInput::Pressed(FieldId::AddCondition));
        s.input(SettingsInput::Pressed(FieldId::AddCondition));
        s.input(SettingsInput::Chose(FieldId::ConditionSubject(1), 1, None));
        s.input(SettingsInput::Chose(FieldId::ConditionTest(1), 5, None));
        let view = s.current_view();
        let Control::Popup { label, buttons, .. } = find(&view, &FieldId::ConditionSubject(1))
        else {
            unreachable!()
        };
        assert_eq!(label, "if");
        assert!(matches!(
            &buttons[..],
            [
                (FieldId::ConditionMoveUp(1), _, true),
                (FieldId::ConditionRemove(1), _, true)
            ]
        ));
        let Control::Popup { buttons, .. } = find(&view, &FieldId::ConditionSubject(0)) else {
            unreachable!()
        };
        assert!(matches!(
            &buttons[..],
            [
                (FieldId::ConditionMoveDown(0), _, true),
                (FieldId::ConditionRemove(0), _, true)
            ]
        ));
        assert!(matches!(
            find(&view, &FieldId::ConditionJoin(1)),
            Control::Segments {
                selected: Some(0),
                ..
            }
        ));
        assert!(
            view.controls
                .iter()
                .all(|c| c.id() != Some(&FieldId::ConditionText(1, "value")))
        );
        s.input(SettingsInput::Pressed(FieldId::ConditionRemove(0)));
        s.input(SettingsInput::Text(
            FieldId::TransformField("pre_event"),
            "30m".into(),
        ));
        let view = s.current_view();
        let Control::Popup { label, .. } = find(&view, &FieldId::ConditionSubject(0)) else {
            unreachable!()
        };
        assert_eq!(label, "Only if");
        assert_eq!(
            transform_summary(&view, 0),
            "Add buffer: 30 min before, only if the location is not empty"
        );
    }

    #[test]
    fn test_conditions_joined_by_or_share_a_box() {
        let (mut s, _, _) = screen("or", CONFIG, false);
        let skip = ACTIONS
            .iter()
            .position(|a| a.action == capycal::TransformAction::Skip)
            .unwrap();
        s.input(SettingsInput::Chose(
            FieldId::InsertTransform(usize::MAX),
            skip + 1,
            None,
        ));
        s.input(SettingsInput::Pressed(FieldId::AddCondition));
        s.input(SettingsInput::Pressed(FieldId::AddCondition));
        for (i, text) in ["Lunch", "Dinner", "Team"].into_iter().enumerate() {
            s.input(SettingsInput::Text(
                FieldId::ConditionText(i, "value"),
                text.into(),
            ));
        }
        s.input(SettingsInput::Selected(FieldId::ConditionJoin(1), 1));
        let view = s.current_view();
        assert_eq!(
            transform_summary(&view, 0),
            "Skip events if (the title contains \"Lunch\" or the title contains \"Dinner\") \
             and the title contains \"Team\""
        );
        // The box goes around the first two conditions and the "or" between them
        let at = |id: FieldId| view.controls.iter().position(|c| c.id() == Some(&id));
        let start = view
            .controls
            .iter()
            .position(|c| matches!(c, Control::AnyOf(_)))
            .unwrap();
        let end = view
            .controls
            .iter()
            .position(|c| *c == Control::AnyOfEnd)
            .unwrap();
        assert!(start < at(FieldId::ConditionSubject(0)).unwrap());
        assert!(at(FieldId::ConditionJoin(1)).unwrap() < end);
        assert!(end < at(FieldId::ConditionJoin(2)).unwrap());
        // Moved up, the third condition takes the second's place in the box
        press(&mut s, FieldId::ConditionMoveUp(2));
        assert_eq!(
            transform_summary(&s.current_view(), 0),
            "Skip events if (the title contains \"Lunch\" or the title contains \"Team\") \
             and the title contains \"Dinner\""
        );
        // And back down
        press(&mut s, FieldId::ConditionMoveDown(1));
        press(&mut s, FieldId::ConditionMoveUp(2));
        s.input(SettingsInput::Selected(FieldId::ConditionJoin(2), 1));
        assert_eq!(
            transform_summary(&s.current_view(), 0),
            "Skip events if the title contains \"Lunch\", the title contains \"Team\" \
             or the title contains \"Dinner\""
        );
        // Removing the middle one keeps the others in one group
        press(&mut s, FieldId::ConditionRemove(1));
        assert_eq!(
            transform_summary(&s.current_view(), 0),
            "Skip events if the title contains \"Lunch\" or the title contains \"Dinner\""
        );
    }

    #[test]
    fn test_conditions_are_cards_to_drag() {
        let (mut s, _, _) = screen("drag", CONFIG, false);
        let skip = ACTIONS
            .iter()
            .position(|a| a.action == capycal::TransformAction::Skip)
            .unwrap();
        s.input(SettingsInput::Chose(
            FieldId::InsertTransform(usize::MAX),
            skip + 1,
            None,
        ));
        s.input(SettingsInput::Pressed(FieldId::AddCondition));
        s.input(SettingsInput::Pressed(FieldId::AddCondition));
        for (i, text) in ["Lunch", "Dinner", "Team"].into_iter().enumerate() {
            s.input(SettingsInput::Text(
                FieldId::ConditionText(i, "value"),
                text.into(),
            ));
        }
        s.input(SettingsInput::Selected(FieldId::ConditionJoin(1), 1));
        // Each condition is a card from its subject to its last field, the
        // joins between the cards
        let view = s.current_view();
        let at = |c: &Control| view.controls.iter().position(|x| x == c).unwrap();
        let id = |id: FieldId| {
            view.controls
                .iter()
                .position(|c| c.id() == Some(&id))
                .unwrap()
        };
        let cards: Vec<usize> = view
            .controls
            .iter()
            .enumerate()
            .filter(|(_, c)| matches!(c, Control::Condition(_)))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(cards.len(), 3);
        assert!(id(FieldId::ConditionJoin(1)) < cards[1]);
        assert!(cards[1] < id(FieldId::ConditionSubject(1)));
        assert!(at(&Control::AnyOfEnd) > cards[1]);
        // A drop mark above the box and one inside it, and below its end
        // one inside and one outside
        let marks: Vec<(usize, usize)> = view
            .controls
            .iter()
            .enumerate()
            .filter_map(|(i, c)| match c {
                Control::DropMark { slot, .. } => Some((i, *slot)),
                _ => None,
            })
            .collect();
        assert_eq!(
            marks.iter().map(|(_, slot)| *slot).collect::<Vec<_>>(),
            [0, 1, 2, 3, 4, 5]
        );
        let any = |c: &Control| matches!(c, Control::AnyOf(_));
        let start = view.controls.iter().position(any).unwrap();
        let end = at(&Control::AnyOfEnd);
        let inside: Vec<bool> = marks.iter().map(|(i, _)| start < *i && *i < end).collect();
        assert_eq!(inside, [false, true, true, true, false, false]);
        // The first stays where it is above or at the start of the box
        assert_eq!(
            view.controls[marks[1].0],
            Control::DropMark {
                slot: 1,
                stays: vec![0]
            }
        );
        // Dragged between the two joined by "or", the third joins them
        s.input(SettingsInput::Moved(FieldId::Condition(2), 2));
        assert_eq!(
            transform_summary(&s.current_view(), 0),
            "Skip events if the title contains \"Lunch\", the title contains \"Team\" \
             or the title contains \"Dinner\""
        );
        // A move rewrites fields in place: the renderers then overwrite
        // the one being typed in too
        let moves = s.current_view().moves;
        s.input(SettingsInput::Moved(FieldId::Condition(0), 5));
        assert_eq!(s.current_view().moves, moves + 1);
        s.input(SettingsInput::Pressed(FieldId::ConditionMoveUp(1)));
        s.input(SettingsInput::Pressed(FieldId::ConditionMoveDown(0)));
        assert_eq!(s.current_view().moves, moves + 3);
        s.input(SettingsInput::Text(
            FieldId::ConditionText(0, "value"),
            "Team".into(),
        ));
        assert_eq!(s.current_view().moves, moves + 3);
        assert_eq!(
            transform_summary(&s.current_view(), 0),
            "Skip events if (the title contains \"Team\" or the title contains \"Dinner\") \
             and the title contains \"Lunch\""
        );
    }

    #[test]
    fn test_template_brings_its_conditions() {
        let (mut s, _, _) = screen("template", CONFIG, false);
        let at_place = TEMPLATES
            .iter()
            .position(|t| t.title == "Travel time for events at a place")
            .unwrap();
        s.input(SettingsInput::Chose(
            FieldId::InsertTransform(usize::MAX),
            ACTIONS.len() + 2 + at_place,
            None,
        ));
        assert_eq!(
            transform_summary(&s.current_view(), 0),
            "Add buffer: 30 min before, 30 min after, only if the location is not empty"
        );
    }

    #[test]
    fn test_refresh_lists_the_calendars_again() {
        let (mut s, _, _) = screen("refresh", CONFIG, true);
        s.update(
            &Update::AccountCalendars {
                account: "work".into(),
                result: Err("offline".into()),
            },
            None,
        );
        let replies = s.input(SettingsInput::Pressed(FieldId::RefreshCalendars));
        let sent: Vec<String> = replies
            .iter()
            .filter_map(|r| match r {
                Reply::Send(c) => Some(format!("{c:?}")),
                _ => None,
            })
            .collect();
        assert_eq!(
            sent,
            [
                "DiscoverAccount(\"spare\")",
                "DiscoverAccount(\"work\")",
                "ListCalendarApp"
            ]
        );
        assert!(popup_titles(&s.current_view(), FieldId::Source).contains(&"Loading…".to_string()));
        assert!(!popup_titles(&s.current_view(), FieldId::Source).contains(&"offline".to_string()));
    }

    fn wizard_items(view: &SettingsView) -> Vec<Choice> {
        match find(view, &FieldId::WizardList) {
            Control::Choices { items, .. } => items.clone(),
            other => panic!("{other:?}"),
        }
    }

    fn wizard_rows(view: &SettingsView) -> Vec<String> {
        wizard_items(view).into_iter().map(|c| c.title).collect()
    }

    fn row(view: &SettingsView, wanted: &str) -> usize {
        wizard_rows(view)
            .iter()
            .position(|r| r == wanted.trim_end() || r.starts_with(wanted))
            .unwrap_or_else(|| panic!("no {wanted} in {:?}", wizard_rows(view)))
    }

    fn press(s: &mut SettingsScreen, id: FieldId) -> Vec<Reply> {
        s.input(SettingsInput::Pressed(id))
    }

    const ACCOUNTS_ONLY: &str = r#"[accounts.work]
url = "https://x/"
username = "u"
password = { keychain = "work" }

[calendars.a]
account = "work"
url = "https://x/a/"

[calendars.b]
account = "work"
url = "https://x/b/"
"#;

    #[test]
    fn test_first_start_on_macos_asks_where_the_calendars_are() {
        let (mut s, _, _) = screen("wizard-mac", "", true);
        let view = s.current_view();
        assert!(view.syncs.is_empty());
        assert!(wizard_rows(&view)[0].starts_with("Calendars in Calendar.app"));
        assert!(!button(&view, FieldId::WizardNext));
        s.input(SettingsInput::Selected(FieldId::WizardList, 1));
        press(&mut s, FieldId::WizardNext);
        assert!(matches!(
            find(&s.current_view(), &FieldId::Url),
            Control::Text { .. }
        ));
        press(&mut s, FieldId::WizardBack);
        s.input(SettingsInput::Selected(FieldId::WizardList, 0));
        press(&mut s, FieldId::WizardNext);
        assert!(s.current_view().controls.contains(&Control::Steps {
            current: 0,
            count: 4
        }));
    }

    #[test]
    fn test_first_start_on_linux_adds_an_account_first() {
        let (mut s, _, path) = screen("wizard-linux", "", false);
        // A welcome first, then the account
        let view = s.current_view();
        assert!(view.compact);
        assert_eq!(view.title, "Welcome to Capycal");
        assert!(button(&view, FieldId::WizardNext));
        press(&mut s, FieldId::WizardNext);
        let view = s.current_view();
        assert!(matches!(find(&view, &FieldId::Url), Control::Text { .. }));
        assert!(button(&view, FieldId::WizardBack));
        assert!(!button(&view, FieldId::WizardNext));
        for (id, text) in [
            (FieldId::AccountName, "work"),
            (FieldId::Url, "https://x/"),
            (FieldId::Username, "u"),
            (FieldId::Password, "secret"),
        ] {
            s.input(SettingsInput::Text(id, text.into()));
        }
        let replies = press(&mut s, FieldId::WizardNext);
        assert!(
            replies.iter().any(
                |r| matches!(r, Reply::Send(Command::SaveSettings(c)) if c.keychain.len() == 1)
            )
        );
        // The backend writes the account
        std::fs::write(&path, "[accounts.work]\nurl = \"https://x/\"\nusername = \"u\"\npassword = { keychain = \"work\" }\n").unwrap();
        let (config, editor) = load(&path);
        let replies = s.update(&Update::SettingsSaved(Ok(())), Some((&config, &editor)));
        assert!(
            replies
                .iter()
                .any(|r| matches!(r, Reply::Send(Command::DiscoverAccount(a)) if a == "work"))
        );
        assert!(s.current_view().controls.contains(&Control::Steps {
            current: 0,
            count: 4
        }));
    }

    #[test]
    fn test_wizard_adds_a_second_account_for_the_target() {
        let (mut s, _, path) = screen("wizard-second-account", "", false);
        let hero = |view: &SettingsView| {
            view.controls
                .iter()
                .find_map(|c| match c {
                    Control::Hero { title, .. } => Some(title.clone()),
                    _ => None,
                })
                .unwrap()
        };
        let account = |s: &mut SettingsScreen, name: &str| {
            for (id, text) in [
                (FieldId::AccountName, name),
                (FieldId::Url, "https://x/"),
                (FieldId::Username, "u"),
                (FieldId::Password, "p"),
            ] {
                s.input(SettingsInput::Text(id, text.into()));
            }
            press(s, FieldId::WizardNext);
        };
        let work = "[accounts.work]\nurl = \"https://x/\"\nusername = \"u\"\npassword = { keychain = \"work\" }\n";
        press(&mut s, FieldId::WizardNext);
        account(&mut s, "work");
        std::fs::write(&path, work).unwrap();
        let (config, editor) = load(&path);
        s.update(&Update::SettingsSaved(Ok(())), Some((&config, &editor)));
        s.update(
            &Update::AccountCalendars {
                account: "work".into(),
                result: Ok(vec![found("/a/", true)]),
            },
            None,
        );
        let a = row(&s.current_view(), "a");
        s.input(SettingsInput::Selected(FieldId::WizardList, a));
        press(&mut s, FieldId::WizardNext);

        // The target is in another account
        let view = s.current_view();
        assert_eq!(hero(&view), "Into");
        assert!(button(&view, FieldId::WizardAddAccount));
        press(&mut s, FieldId::WizardAddAccount);
        let view = s.current_view();
        assert_eq!(hero(&view), "Add another account");
        assert_eq!(
            view.controls.iter().find_map(|c| match c {
                Control::Text {
                    id: FieldId::AccountName,
                    value,
                    ..
                } => Some(value.clone()),
                _ => None,
            }),
            Some(String::new()),
            "an empty form"
        );
        // Back returns to the target, the source kept
        press(&mut s, FieldId::WizardBack);
        assert_eq!(hero(&s.current_view()), "Into");
        press(&mut s, FieldId::WizardAddAccount);
        account(&mut s, "home");
        std::fs::write(
            &path,
            format!("{work}\n[accounts.home]\nurl = \"https://x/\"\nusername = \"u\"\npassword = {{ keychain = \"home\" }}\n"),
        )
        .unwrap();
        let (config, editor) = load(&path);
        let replies = s.update(&Update::SettingsSaved(Ok(())), Some((&config, &editor)));
        assert!(
            replies
                .iter()
                .any(|r| matches!(r, Reply::Send(Command::DiscoverAccount(a)) if a == "home"))
        );
        s.update(
            &Update::AccountCalendars {
                account: "home".into(),
                result: Ok(vec![found("/family/", true)]),
            },
            None,
        );
        let view = s.current_view();
        assert_eq!(hero(&view), "Into", "back at the target");
        assert!(view.controls.contains(&Control::Steps {
            current: 1,
            count: 4
        }));
        let family = row(&view, "family");
        s.input(SettingsInput::Selected(FieldId::WizardList, family));
        press(&mut s, FieldId::WizardNext);
        assert_eq!(hero(&s.current_view()), "What should the copies show?");
    }

    #[test]
    fn test_wizard_creates_a_sync_with_a_preset() {
        let (mut s, _, _) = screen("wizard-create", ACCOUNTS_ONLY, false);
        s.update(
            &Update::AccountCalendars {
                account: "work".into(),
                result: Ok(vec![
                    found("/a/", true),
                    found("/b/", true),
                    found("/ro/", false),
                ]),
            },
            None,
        );
        // Accounts exist: straight to the source
        let view = s.current_view();
        assert!(view.controls.contains(&Control::Steps {
            current: 0,
            count: 4
        }));
        let a = row(&view, "a ");
        s.input(SettingsInput::Selected(FieldId::WizardList, a));
        press(&mut s, FieldId::WizardNext);

        let view = s.current_view();
        // The source stays listed, greyed out
        let source = wizard_items(&view)
            .into_iter()
            .find(|c| c.title == "a" || c.title.starts_with("a "))
            .unwrap_or_else(|| panic!("the source is listed: {:?}", wizard_rows(&view)));
        assert!(!source.enabled, "source offered as target");
        assert_eq!(source.hint.as_deref(), Some("the source"));
        let ro = row(&view, "ro ");
        assert_eq!(wizard_items(&view)[ro].hint.as_deref(), Some("read-only"));
        assert!(!wizard_items(&view)[ro].enabled);
        s.input(SettingsInput::Selected(FieldId::WizardList, ro));
        assert!(!button(&s.current_view(), FieldId::WizardNext));
        let b = row(&s.current_view(), "b ");
        s.input(SettingsInput::Selected(FieldId::WizardList, b));
        press(&mut s, FieldId::WizardNext);

        // What to copy, the name on a page of its own
        let view = s.current_view();
        assert!(
            !view
                .controls
                .iter()
                .any(|c| c.id() == Some(&FieldId::WizardName))
        );
        s.input(SettingsInput::Selected(
            FieldId::WizardList,
            row(&view, "Title and time"),
        ));
        press(&mut s, FieldId::WizardNext);
        let view = s.current_view();
        assert!(view.controls.contains(&Control::Steps {
            current: 3,
            count: 4
        }));
        assert!(
            matches!(find(&view, &FieldId::WizardName), Control::Text { value, .. } if value == "a-to-b")
        );
        let replies = press(&mut s, FieldId::WizardNext);
        let change = replies
            .iter()
            .find_map(|r| match r {
                Reply::Send(Command::SaveSettings(c)) => Some(c.clone()),
                _ => None,
            })
            .expect("a save");
        let sync = change
            .edits
            .iter()
            .find_map(|e| match e {
                crate::settings::Edit::SetSync(s) => Some(s.clone()),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            (
                sync.name.as_str(),
                sync.source.as_str(),
                sync.target.as_str()
            ),
            ("a-to-b", "a", "b")
        );
        assert_eq!(sync.fields, ["SUMMARY"]);
        assert!(sync.transforms.is_empty());
    }

    /// The edits of the save `replies` send.
    fn saved_edits(replies: &[Reply]) -> Vec<Edit> {
        replies
            .iter()
            .find_map(|r| match r {
                Reply::Send(Command::SaveSettings(c)) => Some(c.edits.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no save in {replies:?}"))
    }

    fn feed_field(view: &SettingsView) -> Option<(String, Option<String>, bool)> {
        view.controls.iter().find_map(|c| match c {
            Control::Text {
                id: FieldId::FeedUrl,
                value,
                error,
                enabled,
                ..
            } => Some((value.clone(), error.clone(), *enabled)),
            _ => None,
        })
    }

    const GOOGLE: &str = "https://calendar.google.com/calendar/ical/x/basic.ics";

    #[test]
    fn test_feed_source_is_added_with_its_address() {
        let (mut s, _, _) = screen("feed-add", CONFIG, false);
        let view = s.current_view();
        assert!(feed_field(&view).is_none());
        let add = popup_titles(&view, FieldId::Source)
            .iter()
            .position(|t| t == ADD_FEED)
            .unwrap();
        s.input(SettingsInput::Chose(FieldId::Source, add, None));
        let view = s.current_view();
        // Empty: no mistake under the field yet, but no Save either
        assert_eq!(feed_field(&view), Some((String::new(), None, true)));
        assert_eq!(
            footer(&view),
            ("Paste the feed's address".to_string(), true)
        );
        s.input(SettingsInput::Text(
            FieldId::FeedUrl,
            "calendar.google.com".into(),
        ));
        s.input(SettingsInput::Left(FieldId::FeedUrl));
        assert_eq!(
            feed_field(&s.current_view()).and_then(|f| f.1).as_deref(),
            Some(FEED_INVALID)
        );
        s.input(SettingsInput::Text(FieldId::FeedUrl, GOOGLE.into()));
        let view = s.current_view();
        let Control::Popup {
            items, selected, ..
        } = find(&view, &FieldId::Source)
        else {
            unreachable!()
        };
        assert_eq!(
            items[selected.unwrap()].title,
            "google",
            "keyed by its site"
        );
        // "a" is no longer used
        assert!(matches!(
            press(&mut s, FieldId::Save)[..],
            [Reply::Ask { .. }, ..]
        ));
        let edits = saved_edits(&s.input(SettingsInput::Confirmed(Box::new(
            SettingsInput::Pressed(FieldId::Save),
        ))));
        assert!(edits.iter().any(|e| matches!(e,
            Edit::SetCalendar(key, c) if key == "google" && c.ics_url == Some(Credential::Plain(GOOGLE.into())))));
        assert!(
            edits
                .iter()
                .any(|e| matches!(e, Edit::SetSync(sync) if sync.source == "google"))
        );
    }

    #[test]
    fn test_next_event_waits_for_a_feed_address() {
        let (mut s, _, _) = screen("feed-next", CONFIG, false);
        let reads = |replies: &[Reply]| {
            replies
                .iter()
                .filter(|r| matches!(r, Reply::Send(Command::NextSourceEvent { .. })))
                .count()
        };
        assert_eq!(
            reads(&s.input(SettingsInput::Selected(FieldId::PreviewMode, 1))),
            1
        );
        let add = popup_titles(&s.current_view(), FieldId::Source)
            .iter()
            .position(|t| t == ADD_FEED)
            .unwrap();
        assert_eq!(
            reads(&s.input(SettingsInput::Chose(FieldId::Source, add, None))),
            0
        );
        let view = s.current_view();
        assert!(view.controls.iter().any(|c| matches!(c,
            Control::Preview { source_label, .. } if source_label == "A sample event")));
        let replies = s.input(SettingsInput::Text(FieldId::FeedUrl, GOOGLE.into()));
        assert!(replies.iter().any(|r| matches!(r,
            Reply::Send(Command::NextSourceEvent { calendar, .. }) if calendar.is_ics())));
        // Leaving the field reads the address typed
        assert_eq!(reads(&s.input(SettingsInput::Left(FieldId::FeedUrl))), 1);
    }

    #[test]
    fn test_a_configured_feed_address_picks_that_feed() {
        let text = format!("{CONFIG}\n[calendars.google]\nics_url = \"{GOOGLE}\"\n");
        let (mut s, _, _) = screen("feed-again", &text, false);
        let add = popup_titles(&s.current_view(), FieldId::Source)
            .iter()
            .position(|t| t == ADD_FEED)
            .unwrap();
        s.input(SettingsInput::Chose(FieldId::Source, add, None));
        s.input(SettingsInput::Text(FieldId::FeedUrl, GOOGLE.into()));
        // "a" is no longer used: asked first
        let replies = press(&mut s, FieldId::Save);
        let replies = match replies[..] {
            [Reply::Ask { .. }, ..] => s.input(SettingsInput::Confirmed(Box::new(
                SettingsInput::Pressed(FieldId::Save),
            ))),
            _ => replies,
        };
        let edits = saved_edits(&replies);
        assert!(
            !edits.iter().any(|e| matches!(e, Edit::SetCalendar(..))),
            "no second entry: {edits:?}"
        );
        assert!(
            edits
                .iter()
                .any(|e| matches!(e, Edit::SetSync(sync) if sync.source == "google"))
        );
    }

    #[test]
    fn test_feed_address_of_a_saved_sync_can_be_changed() {
        let text = format!(
            "{ACCOUNTS_ONLY}\n[calendars.google]\nics_url = \"{GOOGLE}\"\n\n\
             [syncs.google-to-b]\nsource = \"google\"\ntarget = \"b\"\ninterval = \"5m\"\n"
        );
        let (mut s, _, _) = screen("feed-edit", &text, false);
        let view = s.current_view();
        assert_eq!(feed_field(&view), Some((GOOGLE.to_string(), None, true)));
        // A feed is no target
        let Control::Popup { items, .. } = find(&view, &FieldId::Target) else {
            unreachable!()
        };
        let feed = items.iter().find(|i| i.title == "google").unwrap();
        assert!(!feed.enabled);
        let moved = GOOGLE.replace("/x/", "/y/");
        s.input(SettingsInput::Text(FieldId::FeedUrl, moved.clone()));
        let edits = saved_edits(&press(&mut s, FieldId::Save));
        assert!(edits.iter().any(|e| matches!(e,
            Edit::SetCalendar(key, c) if key == "google" && c.ics_url == Some(Credential::Plain(moved.clone())))));

        // A secret address stays in the config file
        let hidden = text.replace(&format!("\"{GOOGLE}\""), "{ keychain = \"google\" }");
        let (s, _, _) = screen("feed-hidden", &hidden, false);
        assert_eq!(
            feed_field(&s.current_view()),
            Some((String::new(), None, false))
        );
    }

    #[test]
    fn test_wizard_takes_a_feed_as_source() {
        let (mut s, _, _) = screen("wizard-feed", ACCOUNTS_ONLY, false);
        assert!(button(&s.current_view(), FieldId::WizardAddFeed));
        press(&mut s, FieldId::WizardAddFeed);
        let view = s.current_view();
        assert!(feed_field(&view).is_some());
        assert!(!button(&view, FieldId::WizardNext));
        s.input(SettingsInput::Text(FieldId::FeedUrl, GOOGLE.into()));
        assert!(button(&s.current_view(), FieldId::WizardNext));
        press(&mut s, FieldId::WizardNext);
        let view = s.current_view();
        let feed_button = |c: &Control| matches!(c, Control::Buttons(b) if b.iter().any(|(id, _, _)| *id == FieldId::WizardAddFeed));
        assert!(
            !view.controls.iter().any(feed_button),
            "only for the source"
        );
        let b = row(&view, "b ");
        s.input(SettingsInput::Selected(FieldId::WizardList, b));
        press(&mut s, FieldId::WizardNext);
        press(&mut s, FieldId::WizardNext);
        assert!(
            matches!(find(&s.current_view(), &FieldId::WizardName), Control::Text { value, .. } if value == "google-to-b")
        );
        let edits = saved_edits(&press(&mut s, FieldId::WizardNext));
        assert!(edits.iter().any(|e| matches!(e,
            Edit::SetCalendar(key, c) if key == "google" && c.ics_url == Some(Credential::Plain(GOOGLE.into())))));
    }

    #[test]
    fn test_presets() {
        let busy = Preset::Busy.apply(capycal::SyncConfig::new("s", "a", "b"), "Work");
        assert!(busy.fields.is_empty());
        assert_eq!(busy.transforms[0].value.as_deref(), Some("Busy (Work)"));
        assert!(busy.enabled);
        let all = Preset::Everything.apply(capycal::SyncConfig::new("s", "a", "b"), "Work");
        assert_eq!(all.fields, ["SUMMARY", "DESCRIPTION", "LOCATION", "VALARM"]);
        // Custom: nothing filled in, and off until it is set up
        let custom = Preset::Custom.apply(capycal::SyncConfig::new("s", "a", "b"), "Work");
        assert!(custom.fields.is_empty() && custom.transforms.is_empty());
        assert!(!custom.enabled);
    }

    #[test]
    fn test_cancelling_the_first_wizard_leaves_an_add_button() {
        let (mut s, _, _) = screen("wizard-cancel", ACCOUNTS_ONLY, false);
        press(&mut s, FieldId::WizardCancel);
        let view = s.current_view();
        assert_eq!(
            view.controls[0],
            Control::Note("Add your first sync".into())
        );
        assert!(button(&view, FieldId::AddSync));
        press(&mut s, FieldId::AddSync);
        assert!(s.current_view().controls.contains(&Control::Steps {
            current: 0,
            count: 4
        }));
    }

    fn wizard_screen(name: &str) -> SettingsScreen {
        let (mut s, _, _) = screen(name, ACCOUNTS_ONLY, false);
        s.update(
            &Update::AccountCalendars {
                account: "work".into(),
                result: Ok(vec![
                    found("/a/", true),
                    found("/b/", true),
                    found("/c/", true),
                ]),
            },
            None,
        );
        s
    }

    fn selected_row(view: &SettingsView) -> Option<String> {
        match find(view, &FieldId::WizardList) {
            Control::Choices {
                items, selected, ..
            } => selected.map(|i| items[i].title.clone()),
            _ => None,
        }
    }

    #[test]
    fn test_wizard_choice_follows_the_calendar_not_the_row() {
        let mut s = wizard_screen("wizard-stale");
        let b = row(&s.current_view(), "b ");
        s.input(SettingsInput::Selected(FieldId::WizardList, b));
        // Another account's calendars arrive and sort before work's rows
        let mut more = ACCOUNTS_ONLY.to_string();
        more.push_str(
            "\n[accounts.alpha]\nurl = \"https://y/\"\nusername = \"u\"\npassword = \"p\"\n",
        );
        let path = std::env::temp_dir().join(format!(
            "capycal-{}-view-wizard-stale-2.toml",
            std::process::id()
        ));
        std::fs::write(&path, &more).unwrap();
        let (config, editor) = load(&path);
        s.update(&Update::ConfigChanged, Some((&config, &editor)));
        s.update(
            &Update::AccountCalendars {
                account: "alpha".into(),
                result: Ok(vec![found("/x/", true), found("/y/", true)]),
            },
            None,
        );
        assert_eq!(selected_row(&s.current_view()).as_deref(), Some("b"));
        press(&mut s, FieldId::WizardNext);
        let c = row(&s.current_view(), "c ");
        s.input(SettingsInput::Selected(FieldId::WizardList, c));
        press(&mut s, FieldId::WizardNext);
        press(&mut s, FieldId::WizardNext);
        assert!(
            matches!(find(&s.current_view(), &FieldId::WizardName), Control::Text { value, .. } if value == "b-to-c")
        );
    }

    #[test]
    fn test_wizard_back_keeps_the_choices_and_the_name() {
        let mut s = wizard_screen("wizard-back");
        s.input(SettingsInput::Selected(
            FieldId::WizardList,
            row(&s.current_view(), "a "),
        ));
        press(&mut s, FieldId::WizardNext);
        s.input(SettingsInput::Selected(
            FieldId::WizardList,
            row(&s.current_view(), "b "),
        ));
        press(&mut s, FieldId::WizardNext);
        press(&mut s, FieldId::WizardNext);
        s.input(SettingsInput::Text(FieldId::WizardName, "mine".into()));
        press(&mut s, FieldId::WizardBack);
        press(&mut s, FieldId::WizardBack);
        assert_eq!(selected_row(&s.current_view()).as_deref(), Some("b"));
        press(&mut s, FieldId::WizardBack);
        assert_eq!(selected_row(&s.current_view()).as_deref(), Some("a"));
        assert!(
            !button(&s.current_view(), FieldId::WizardBack),
            "no step before the source here"
        );
        press(&mut s, FieldId::WizardNext);
        press(&mut s, FieldId::WizardNext);
        press(&mut s, FieldId::WizardNext);
        assert!(
            matches!(find(&s.current_view(), &FieldId::WizardName), Control::Text { value, .. } if value == "mine")
        );
    }

    #[test]
    fn test_steps_text() {
        assert_eq!(steps_text(1, 3), "\u{25cf} \u{25cf} \u{25cb}");
    }

    #[test]
    fn test_wizard_groups_calendars_and_sums_up_the_choice() {
        let mut s = wizard_screen("wizard-groups");
        let view = s.current_view();
        let items = wizard_items(&view);
        assert!(items[0].header && items[0].title == "work");
        // A header is not a calendar
        s.input(SettingsInput::Selected(FieldId::WizardList, 0));
        assert!(!button(&s.current_view(), FieldId::WizardNext));
        s.input(SettingsInput::Selected(
            FieldId::WizardList,
            row(&s.current_view(), "a "),
        ));
        press(&mut s, FieldId::WizardNext);
        assert!(s.current_view().controls.iter().any(|c| matches!(
            c,
            Control::Hero { text, .. } if text.contains("a (work)")
        )));
        s.input(SettingsInput::Selected(
            FieldId::WizardList,
            row(&s.current_view(), "b "),
        ));
        press(&mut s, FieldId::WizardNext);
        let view = s.current_view();
        assert!(view.controls.iter().any(|c| matches!(
            c,
            Control::Hero { text, .. } if text.starts_with("a (work) \u{2192} b (work).")
        )));
        // Each preset shows its copy; Custom has none to show
        assert!(
            wizard_items(&view)
                .iter()
                .all(|c| c.preview.is_some() == (c.title != "Custom"))
        );
        assert!(view.controls.contains(&Control::Steps {
            current: 2,
            count: 4
        }));
    }

    #[test]
    fn test_wizard_lists_calendar_app_by_account_without_add_account() {
        let (mut s, _, _) = screen("wizard-sources", "", true);
        s.input(SettingsInput::Selected(FieldId::WizardList, 0));
        press(&mut s, FieldId::WizardNext);
        let cal = |title: &str, source: &str| capycal::CalendarInfo {
            title: title.into(),
            source: Some(source.into()),
            identifier: format!("{source}-{title}"),
            writable: true,
        };
        s.update(
            &Update::CalendarAppCalendars(Ok(vec![
                cal("Work", "iCloud"),
                cal("Calendar", "Exchange"),
                cal("Family", "iCloud"),
            ])),
            None,
        );
        let view = s.current_view();
        let shown: Vec<(String, bool)> = wizard_items(&view)
            .into_iter()
            .map(|c| (c.title, c.header))
            .collect();
        let expected = [
            ("Exchange", true),
            ("Calendar", false),
            ("iCloud", true),
            ("Family", false),
            ("Work", false),
        ];
        assert_eq!(shown, expected.map(|(t, h)| (t.to_string(), h)).to_vec());
        assert!(!view.controls.iter().any(|c| matches!(
            c,
            Control::Buttons(b) if b.iter().any(|(_, title, _)| title.starts_with("Add Account"))
        )));
        // CalDAV is one step back
        assert!(button(&view, FieldId::WizardBack));
    }

    #[test]
    fn test_a_caldav_wizard_leaves_calendar_app_out() {
        let (mut s, _, path) = screen("wizard-caldav-only", "", true);
        s.input(SettingsInput::Selected(FieldId::WizardList, 1));
        press(&mut s, FieldId::WizardNext);
        for (id, text) in [
            (FieldId::AccountName, "work"),
            (FieldId::Url, "https://x/"),
            (FieldId::Username, "u"),
            (FieldId::Password, "p"),
        ] {
            s.input(SettingsInput::Text(id, text.into()));
        }
        press(&mut s, FieldId::WizardNext);
        std::fs::write(&path, "[accounts.work]\nurl = \"https://x/\"\nusername = \"u\"\npassword = { keychain = \"work\" }\n").unwrap();
        let (config, editor) = load(&path);
        s.update(&Update::SettingsSaved(Ok(())), Some((&config, &editor)));
        s.update(
            &Update::AccountCalendars {
                account: "work".into(),
                result: Ok(vec![found("/a/", true)]),
            },
            None,
        );
        let view = s.current_view();
        assert!(
            !view
                .controls
                .iter()
                .any(|c| matches!(c, Control::Note(n) if n.starts_with("Calendar.app"))),
            "{:?}",
            view.controls
        );
        assert!(!wizard_rows(&view).iter().any(|r| r == "Calendar.app"));
        let replies = press(&mut s, FieldId::RefreshCalendars);
        assert!(
            !replies
                .iter()
                .any(|r| matches!(r, Reply::Send(Command::ListCalendarApp)))
        );
    }

    #[test]
    fn test_wizard_name_rules() {
        let mut s = wizard_screen("wizard-name");
        s.input(SettingsInput::Selected(
            FieldId::WizardList,
            row(&s.current_view(), "a "),
        ));
        press(&mut s, FieldId::WizardNext);
        s.input(SettingsInput::Selected(
            FieldId::WizardList,
            row(&s.current_view(), "b "),
        ));
        press(&mut s, FieldId::WizardNext);
        press(&mut s, FieldId::WizardNext);
        s.input(SettingsInput::Text(FieldId::WizardName, "../x".into()));
        assert!(!button(&s.current_view(), FieldId::WizardNext));
    }

    #[test]
    fn test_a_failed_account_save_keeps_the_step() {
        let (mut s, _, path) = screen("wizard-add-account", "", false);
        press(&mut s, FieldId::WizardNext);
        for (id, text) in [
            (FieldId::AccountName, "home"),
            (FieldId::Url, "https://h/"),
            (FieldId::Username, "u"),
            (FieldId::Password, "p"),
        ] {
            s.input(SettingsInput::Text(id, text.into()));
        }
        press(&mut s, FieldId::WizardNext);
        // A failed save keeps the step and shows why; a retry clears it
        s.update(&Update::SettingsSaved(Err("disk full".into())), None);
        assert!(
            s.current_view()
                .controls
                .contains(&Control::Error("disk full".into()))
        );
        press(&mut s, FieldId::WizardNext);
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("\n[accounts.home]\nurl = \"https://h/\"\nusername = \"u\"\npassword = { keychain = \"home\" }\n");
        std::fs::write(&path, text).unwrap();
        let (config, editor) = load(&path);
        s.update(&Update::SettingsSaved(Ok(())), Some((&config, &editor)));
        let view = s.current_view();
        assert!(view.controls.contains(&Control::Steps {
            current: 0,
            count: 4
        }));
        assert!(
            !view
                .controls
                .iter()
                .any(|c| matches!(c, Control::Error(e) if !e.is_empty()))
        );
    }

    #[test]
    fn test_leaving_a_started_wizard_asks() {
        let mut s = wizard_screen("wizard-leave");
        // Untouched: leaving needs no question
        assert!(
            !s.input(SettingsInput::Pressed(FieldId::SidebarAccount(
                "work".into()
            )))
            .iter()
            .any(|r| matches!(r, Reply::Ask { .. }))
        );
        press(&mut s, FieldId::AddSync);
        s.input(SettingsInput::Selected(
            FieldId::WizardList,
            row(&s.current_view(), "a "),
        ));
        press(&mut s, FieldId::WizardNext);
        let replies = s.input(SettingsInput::Pressed(FieldId::SidebarAccount(
            "work".into(),
        )));
        assert!(replies.iter().any(|r| matches!(r, Reply::Ask { .. })));
    }

    #[test]
    fn test_the_first_sync_ends_with_done_in_a_compact_window() {
        let mut s = wizard_screen("wizard-done");
        assert!(s.current_view().compact);
        s.input(SettingsInput::Selected(
            FieldId::WizardList,
            row(&s.current_view(), "a "),
        ));
        press(&mut s, FieldId::WizardNext);
        s.input(SettingsInput::Selected(
            FieldId::WizardList,
            row(&s.current_view(), "b "),
        ));
        press(&mut s, FieldId::WizardNext);
        press(&mut s, FieldId::WizardNext);
        press(&mut s, FieldId::WizardNext);
        let path = std::env::temp_dir().join(format!(
            "capycal-{}-view-wizard-done-2.toml",
            std::process::id()
        ));
        std::fs::write(&path, format!("{ACCOUNTS_ONLY}\n[syncs.a-to-b]\nsource = \"a\"\ntarget = \"b\"\ninterval = \"5m\"\n")).unwrap();
        let (config, editor) = load(&path);
        s.update(&Update::SettingsSaved(Ok(())), Some((&config, &editor)));
        let view = s.current_view();
        assert!(view.compact);
        assert!(view.controls.iter().any(|c| matches!(
            c,
            Control::Hero { image: Some(HeroImage::Done), text, .. } if text.contains("a-to-b")
        )));
        assert!(matches!(
            s.input(SettingsInput::Pressed(FieldId::WizardDone))[..],
            [Reply::Close]
        ));
        // Opening Settings later shows the editor with the sidebar
        let (s2, _) = SettingsScreen::new(&config, &editor, false);
        let view = s2.current_view();
        assert!(!view.compact);
        assert_eq!(view.title, "Settings");
    }

    #[test]
    fn test_a_custom_sync_starts_off_in_the_editor() {
        let mut s = wizard_screen("wizard-custom");
        for calendar in ["a ", "b "] {
            s.input(SettingsInput::Selected(
                FieldId::WizardList,
                row(&s.current_view(), calendar),
            ));
            press(&mut s, FieldId::WizardNext);
        }
        let view = s.current_view();
        let custom = row(&view, "Custom");
        assert!(wizard_items(&view)[custom].detail.contains("switched off"));
        s.input(SettingsInput::Selected(FieldId::WizardList, custom));
        press(&mut s, FieldId::WizardNext);
        let replies = press(&mut s, FieldId::WizardNext);
        assert!(replies.iter().any(|r| matches!(
            r,
            Reply::Send(Command::SaveSettings(c)) if c.edits.iter().any(
                |e| matches!(e, crate::settings::Edit::SetSync(sync) if !sync.enabled)
            )
        )));
        let path = std::env::temp_dir().join(format!(
            "capycal-{}-view-wizard-custom-2.toml",
            std::process::id()
        ));
        std::fs::write(
            &path,
            format!(
                "{ACCOUNTS_ONLY}\n[syncs.a-to-b]\nsource = \"a\"\ntarget = \"b\"\ninterval = \"5m\"\nenabled = false\n"
            ),
        )
        .unwrap();
        let (config, editor) = load(&path);
        s.update(&Update::SettingsSaved(Ok(())), Some((&config, &editor)));
        // Straight to the editor, to set it up
        let view = s.current_view();
        assert!(!view.compact);
        assert!(view.syncs.iter().any(|i| i.name == "a-to-b" && i.selected));
    }

    #[test]
    fn test_a_later_wizard_returns_to_the_editor() {
        let (mut s, _, _) = screen("wizard-later", CONFIG, false);
        assert!(!s.current_view().compact);
        press(&mut s, FieldId::AddSync);
        let view = s.current_view();
        assert!(view.compact);
        assert_eq!(view.title, "New Sync");
        press(&mut s, FieldId::WizardCancel);
        assert!(!s.current_view().compact);
    }

    /// The rules of docs/src/development/ui-guidelines.md a test can check.
    mod ui_rules {
        use super::*;

        /// Every text a view shows, and which of them are button titles.
        fn texts(view: &SettingsView) -> Vec<(String, bool)> {
            let mut out = Vec::new();
            let mut text = |t: &str| out.push((t.to_string(), false));
            let mut buttons = Vec::new();
            for control in &view.controls {
                match control {
                    Control::Header {
                        title,
                        subtitle,
                        status,
                        name,
                        ..
                    } => {
                        text(title);
                        text(subtitle);
                        if let Some((s, _)) = status {
                            text(s);
                        }
                        if let Some((_, Some(e))) = name {
                            text(e);
                        }
                    }
                    Control::Group(t)
                    | Control::AnyOf(t)
                    | Control::Row(t)
                    | Control::Note(t)
                    | Control::Error(t) => text(t),
                    Control::Stage { label, .. } => text(label),
                    Control::Segments { label, items, .. } => {
                        text(label);
                        items.iter().for_each(|i| text(i));
                    }
                    Control::Preview {
                        source_label,
                        copy_label,
                        ..
                    } => {
                        text(source_label);
                        text(copy_label);
                    }
                    Control::TransformRow { summary, .. } => text(summary),
                    Control::Hero { title, text: t, .. } => {
                        text(title);
                        text(t);
                    }
                    Control::Choices { items, .. } => {
                        for item in items {
                            text(&item.title);
                            text(&item.detail);
                            if let Some(h) = &item.hint {
                                text(h);
                            }
                        }
                    }
                    Control::Footer {
                        left,
                        text: t,
                        right,
                        ..
                    } => {
                        text(t);
                        buttons.extend(left.iter().chain(right).map(|(_, b, _)| b.clone()));
                    }
                    Control::Text {
                        label,
                        placeholder,
                        error,
                        ..
                    } => {
                        text(label);
                        text(placeholder);
                        if let Some(e) = error {
                            text(e);
                        }
                    }
                    Control::Check { label, hint, .. } => {
                        text(label);
                        if let Some(h) = hint {
                            text(h);
                        }
                    }
                    Control::Popup {
                        label,
                        items,
                        buttons: popup_buttons,
                        ..
                    } => {
                        text(label);
                        items.iter().for_each(|i| text(&i.title));
                        buttons.extend(popup_buttons.iter().map(|(_, b, _)| b.clone()));
                    }
                    Control::Joint { items, .. } => items.iter().for_each(|i| text(&i.title)),
                    Control::Buttons(list) => {
                        buttons.extend(list.iter().map(|(_, b, _)| b.clone()))
                    }
                    Control::Disclosure { label, .. } => text(label),
                    _ => {}
                }
            }
            out.extend(buttons.into_iter().map(|b| (b, true)));
            out
        }

        /// The views of the windows in many states: syncs with every kind
        /// of step, advanced settings, a mistake, an account, the assistant.
        fn views() -> Vec<SettingsView> {
            let mut views = Vec::new();
            let (mut s, _, _) = screen("rules", CONFIG, false);
            views.push(s.current_view());
            press(&mut s, FieldId::Advanced);
            for index in 1..=ACTIONS.len() + 1 + TEMPLATES.len() {
                s.input(SettingsInput::Chose(
                    FieldId::InsertTransform(usize::MAX),
                    index,
                    None,
                ));
                views.push(s.current_view());
            }
            s.input(SettingsInput::Text(FieldId::Interval, "1".into()));
            s.input(SettingsInput::Left(FieldId::Interval));
            views.push(s.current_view());
            press(&mut s, FieldId::Revert);
            press(&mut s, FieldId::SidebarAccount("work".into()));
            views.push(s.current_view());
            press(&mut s, FieldId::AddAccount);
            views.push(s.current_view());
            // The assistant, from the start
            let (mut w, _, _) = screen("rules-wizard", "", true);
            views.push(w.current_view());
            w.input(SettingsInput::Selected(FieldId::WizardList, 0));
            views.push(w.current_view());
            press(&mut w, FieldId::WizardNext);
            views.push(w.current_view());
            let (mut w, _, _) = screen("rules-caldav", "", false);
            views.push(w.current_view());
            press(&mut w, FieldId::WizardNext);
            views.push(w.current_view());
            views
        }

        #[test]
        fn test_no_dashes_or_three_dots() {
            for view in views() {
                for (text, _) in texts(&view) {
                    for bad in ["\u{2014}", "\u{2013}", "--", "..."] {
                        assert!(
                            !text.contains(bad),
                            "{text:?} has {bad:?}: use \u{2026} for an ellipsis and no dashes"
                        );
                    }
                }
            }
        }

        #[test]
        fn test_buttons_start_upper_case_and_errors_say_what_to_do() {
            for view in views() {
                for (text, button) in texts(&view) {
                    if button {
                        assert!(
                            text.chars().next().is_some_and(char::is_uppercase),
                            "button {text:?} in title case"
                        );
                    }
                    let lower = text.to_lowercase();
                    assert!(
                        !lower.starts_with("error") && !lower.starts_with("invalid"),
                        "{text:?}: say what to do instead"
                    );
                    assert_eq!(text.trim(), text, "{text:?} has outer spaces");
                }
            }
        }

        #[test]
        fn test_renderers_keep_the_palette() {
            let mac = include_str!("settings_window_macos.rs");
            let gtk = include_str!("../capycal-app/settings_window.rs");
            for (renderer, source, banned) in [
                ("macOS", mac, &["systemRedColor"][..]),
                (
                    "GTK",
                    gtk,
                    &["destructive-action", "\"error\"", "@error_color"][..],
                ),
            ] {
                for word in banned {
                    assert!(
                        !source.contains(word),
                        "the {renderer} Settings window uses {word}: problems are orange with a sign"
                    );
                }
            }
        }
    }
}
