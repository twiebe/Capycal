//! macOS glue: tao event loop on the main thread, tray menu, windows and
//! dialogs. The syncs run on a tokio runtime in a background thread; the two
//! sides talk through a command channel and `EventLoopProxy` user events.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use block2::RcBlock;
use capycal::{
    Config, ConfigEditor, DEFAULT_LOG_CAPACITY, LogBuffer, LogFile, LogLevel, default_log_file,
    init_with_buffer,
};
use chrono::Utc;
use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2::runtime::Bool;
use objc2_app_kit::{
    NSApplication, NSBezierPath, NSColor, NSCompositingOperation, NSImage, NSRectFillUsingOperation,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString, NSUserDefaults};
use rfd::{FileDialog, MessageDialog, MessageLevel};
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy, EventLoopWindowTarget};
use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tracing::{error, info};
use tray_icon::menu::{IsMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{TrayIcon, TrayIconBuilder};

use crate::about_window_macos::AboutWindow;
use crate::autostart;
use crate::backend::{Backend, Command, Snapshot, Update, ensure_config_file, other_desktop_app};
use crate::dialog::View;
use crate::dialog_window_macos::DialogWindow;
use crate::keychain_notice;
use crate::log_window_macos::LogWindow;
use crate::logview::{self, Input};
use crate::menu::{self, Action, Icon, Item};
use crate::panel_window_macos::{LARGE, PanelInput, PanelWindow};
use crate::problems;
use crate::purge::{PurgeDialog, Reply};
use crate::purge_all::PurgeAllDialog;
use crate::settings::Confirm;
use crate::settings_view::{self, SettingsInput, SettingsScreen};
use crate::settings_window_macos::{RawInput, SettingsWindow};
use crate::sponsor::{self, Answer, SponsorState};

/// Refresh of the "synced 2 min ago" texts.
const MENU_REFRESH: Duration = Duration::from_secs(30);
/// How often an open log window receives new lines.
const LOG_POLL: Duration = Duration::from_millis(250);
/// How long Quit waits for the syncs to stop: longer than the coordinator
/// waits for a sync (30 s), so a Calendar.app put that is still running can
/// record its event; quitting earlier would create it again after a restart.
const SHUTDOWN_WAIT: Duration = Duration::from_secs(35);

const PROBLEMS_BUTTONS: [&str; 3] = ["Edit Config", "Reload Config", "Show Log"];

enum UserEvent {
    Backend(Update),
    Menu(MenuEvent),
    Log(Input),
    Problems(PanelInput),
    Purge(PanelInput),
    PurgeAll(PanelInput),
    About(PanelInput),
    Acknowledgements(PanelInput),
    ErrorClosed,
    Settings(RawInput),
    Wizard(RawInput),
    SettingsQuestion(PanelInput),
    Sponsor(PanelInput),
    /// The Keychain note was answered or closed.
    KeychainNotice,
}

struct App {
    config_path: PathBuf,
    log_file: LogFile,
    logs: LogBuffer,
    commands: UnboundedSender<Command>,
    proxy: EventLoopProxy<UserEvent>,
    snapshot: Snapshot,
    tray: Option<TrayIcon>,
    /// The items the tray menu shows and their handles, to update it in place.
    menu: Option<(Vec<Item>, Vec<MenuHandle>)>,
    /// The icon and tooltip the menu bar shows.
    tray_status: Option<(Icon, String)>,
    /// Main menu with Edit items, so copy/paste work in the log window.
    _main_menu: Option<Menu>,
    /// The app shows in the Dock (a window is open).
    in_dock: bool,
    /// Settings asked to quit; done after the event.
    quitting: bool,
    log_window: Option<LogWindow>,
    about_window: Option<AboutWindow>,
    acknowledgements_window: Option<PanelWindow>,
    error_window: Option<DialogWindow>,
    problems_window: Option<PanelWindow>,
    /// The Remove Synced Events window and the syncs it removes once the
    /// count arrived.
    purge_window: Option<(DialogWindow, PurgeDialog)>,
    purge_all_window: Option<(DialogWindow, PurgeAllDialog)>,
    sponsor_window: Option<DialogWindow>,
    keychain_window: Option<DialogWindow>,
    /// The Settings screen lives on while its save runs, even if the
    /// window was closed.
    settings: Option<SettingsScreen>,
    /// The entry the Settings window showed when it closed.
    last_selection: Option<crate::settings::Selection>,
    settings_window: Option<SettingsWindow>,
    /// The new-sync wizard, a sheet on the Settings window.
    wizard_window: Option<SettingsWindow>,
    /// A question of the Settings window and the inputs for its confirming
    /// buttons: the main one and an optional alternative.
    settings_question: Option<(DialogWindow, SettingsInput, Option<SettingsInput>)>,
    /// Settings was opened for a config without syncs in this run.
    settings_offered: bool,
    /// The sponsor dialog was shown in this run of the app.
    sponsor_asked: bool,
    next_menu_refresh: Instant,
    next_log_poll: Instant,
}

pub fn run(config_path: PathBuf) -> ExitCode {
    // The log level comes from the config if it loads; problems are reported later
    let level = Config::load(&config_path)
        .map(|c| c.daemon.log_level)
        .unwrap_or_else(|_| "info".to_string());
    let log_file = default_log_file("capycal-ui")
        .unwrap_or_else(|| std::env::temp_dir().join("capycal-ui.log"));
    let (logs, log_file, log_level) =
        match init_with_buffer(&level, &log_file, DEFAULT_LOG_CAPACITY) {
            Ok(logs) => logs,
            Err(e) => {
                eprintln!("capycal-ui: {e}");
                return ExitCode::FAILURE;
            }
        };
    info!(version = env!("CARGO_PKG_VERSION"), config = %config_path.display(), "capycal-ui started");
    match ensure_config_file(&config_path) {
        Ok(true) => info!(config = %config_path.display(), "starter config written"),
        Ok(false) => {}
        Err(e) => {
            error!(config = %config_path.display(), error = %e, "cannot write the starter config")
        }
    }

    // Started a second time (from a terminal; Finder reopens the running
    // app), it would only find the config locked
    if let Some(holder) = other_desktop_app(&config_path) {
        info!(pid = %holder.pid, "Capycal already runs this config, quitting");
        alert_before_start(
            "Capycal is already running",
            &format!(
                "Another Capycal app (pid {}) runs this config. Use its menu bar icon, or \
                 quit it first.",
                holder.pid
            ),
        );
        return ExitCode::SUCCESS;
    }

    let mut event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    event_loop.set_activation_policy(ActivationPolicy::Accessory);
    let proxy = event_loop.create_proxy();
    let menu_proxy = proxy.clone();
    MenuEvent::set_event_handler(Some(move |event| {
        let _ = menu_proxy.send_event(UserEvent::Menu(event));
    }));

    let mut app = App {
        commands: spawn_backend(config_path.clone(), log_level, proxy.clone()),
        config_path,
        log_file,
        logs,
        proxy,
        snapshot: Snapshot::default(),
        tray: None,
        _main_menu: None,
        in_dock: false,
        quitting: false,
        log_window: None,
        about_window: None,
        acknowledgements_window: None,
        error_window: None,
        problems_window: None,
        purge_window: None,
        purge_all_window: None,
        sponsor_window: None,
        keychain_window: None,
        settings: None,
        last_selection: None,
        settings_window: None,
        wizard_window: None,
        settings_question: None,
        settings_offered: false,
        sponsor_asked: false,
        menu: None,
        tray_status: None,
        next_menu_refresh: Instant::now(),
        next_log_poll: Instant::now(),
    };
    event_loop.run(move |event, target, control_flow| app.handle(event, target, control_flow))
}

/// Start the tokio runtime with the backend on its own thread.
fn spawn_backend(
    config_path: PathBuf,
    log_level: LogLevel,
    proxy: EventLoopProxy<UserEvent>,
) -> UnboundedSender<Command> {
    let (tx, rx) = unbounded_channel();
    std::thread::Builder::new()
        .name("capycal-sync".to_string())
        .spawn(move || {
            let runtime = match tokio::runtime::Runtime::new() {
                Ok(runtime) => runtime,
                Err(e) => return error!(error = %e, "cannot start the async runtime"),
            };
            let backend = Backend::new(config_path, move |update| {
                let _ = proxy.send_event(UserEvent::Backend(update));
            })
            .with_log_level(log_level);
            runtime.block_on(backend.run(rx));
        })
        .expect("cannot spawn the sync thread");
    tx
}

impl App {
    fn handle(
        &mut self,
        event: Event<UserEvent>,
        _target: &EventLoopWindowTarget<UserEvent>,
        control_flow: &mut ControlFlow,
    ) {
        match event {
            // The tray icon needs a running event loop
            Event::NewEvents(StartCause::Init) => self.init(),
            // A click on the Dock icon with no window open shows Settings
            Event::Reopen {
                has_visible_windows: false,
                ..
            } => self.open_settings(),
            Event::UserEvent(UserEvent::Backend(update)) => self.on_update(update),
            Event::UserEvent(UserEvent::Menu(event)) => {
                if let Some(action) = Action::from_id(&event.id.0) {
                    if action == Action::Quit {
                        if !self.may_quit() {
                            return;
                        }
                        self.quit();
                        *control_flow = ControlFlow::Exit;
                        return;
                    }
                    self.on_action(action);
                }
            }
            Event::UserEvent(UserEvent::Log(input)) => match input {
                Input::Export => self.export_log(),
                // Dropping the window releases its views and the receiver;
                // the next Show Log starts again from the backlog
                Input::Closed => self.log_window = None,
                _ => {
                    if let Some(log) = &mut self.log_window {
                        log.handle(input);
                    }
                }
            },
            Event::UserEvent(UserEvent::Purge(input)) => self.on_purge_input(input),
            Event::UserEvent(UserEvent::PurgeAll(input)) => self.on_purge_all_input(input),
            Event::UserEvent(UserEvent::ErrorClosed) => self.error_window = None,
            Event::UserEvent(UserEvent::Settings(raw)) => {
                if let Some(input) = self.settings_window.as_ref().and_then(|w| w.translate(raw)) {
                    // The window moved on: an open question no longer applies
                    self.settings_question = None;
                    self.settings_input(input);
                }
            }
            Event::UserEvent(UserEvent::Wizard(raw)) => {
                if let Some(input) = self.wizard_window.as_ref().and_then(|w| w.translate(raw)) {
                    self.settings_question = None;
                    self.settings_input(input);
                }
            }
            Event::UserEvent(UserEvent::SettingsQuestion(input)) => {
                self.on_settings_question(input)
            }
            Event::UserEvent(UserEvent::About(input)) => match input {
                PanelInput::Button(crate::dialog::ABOUT_DESCRIPTION) => {
                    if let Some(window) = &self.about_window {
                        window.next_quip();
                    }
                }
                PanelInput::Button(crate::dialog::ABOUT_ACKNOWLEDGEMENTS) => {
                    self.show_acknowledgements()
                }
                PanelInput::Button(i) => {
                    if let Some((_, url, _)) = crate::dialog::about_links().get(i) {
                        open_url(url);
                    }
                }
                PanelInput::Closed => self.about_window = None,
            },
            Event::UserEvent(UserEvent::Acknowledgements(PanelInput::Closed)) => {
                self.acknowledgements_window = None;
            }
            Event::UserEvent(UserEvent::Sponsor(input)) => self.on_sponsor_input(input),
            // Dropping the window closes it without another Closed event
            Event::UserEvent(UserEvent::KeychainNotice)
                if self.keychain_window.take().is_some() =>
            {
                self.send(Command::KeychainNoticeClosed);
            }
            Event::UserEvent(UserEvent::Problems(input)) => match input {
                PanelInput::Button(0) => self.send(Command::EditConfig),
                PanelInput::Button(1) => self.send(Command::Reload),
                PanelInput::Button(_) => self.show_log(),
                PanelInput::Closed => self.problems_window = None,
            },
            _ => {}
        }

        let now = Instant::now();
        if now >= self.next_menu_refresh {
            self.refresh_menu();
        }
        let mut wake = self.next_menu_refresh;
        if let Some(log) = &mut self.log_window {
            if now >= self.next_log_poll {
                log.drain();
                self.next_log_poll = now + LOG_POLL;
            }
            wake = wake.min(self.next_log_poll);
        }
        *control_flow = ControlFlow::WaitUntil(wake);
        if self.quitting {
            self.quit();
            *control_flow = ControlFlow::Exit;
            return;
        }
        self.update_dock();
    }

    /// In the Dock, with a menu bar and Cmd+Tab, while a window is open;
    /// only in the menu bar otherwise.
    fn update_dock(&mut self) {
        let windows = self.log_window.is_some()
            || self.about_window.is_some()
            || self.acknowledgements_window.is_some()
            || self.error_window.is_some()
            || self.problems_window.is_some()
            || self.purge_window.is_some()
            || self.purge_all_window.is_some()
            || self.sponsor_window.is_some()
            || self.keychain_window.is_some()
            || self.settings_window.is_some();
        if windows == self.in_dock {
            return;
        }
        self.in_dock = windows;
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(if windows {
            objc2_app_kit::NSApplicationActivationPolicy::Regular
        } else {
            objc2_app_kit::NSApplicationActivationPolicy::Accessory
        });
        if windows {
            activate();
        }
    }

    /// Quitting drops a Settings draft: ask first.
    fn may_quit(&self) -> bool {
        if !self
            .settings
            .as_ref()
            .is_some_and(SettingsScreen::has_draft)
        {
            return true;
        }
        activate();
        let answer = MessageDialog::new()
            .set_level(MessageLevel::Warning)
            .set_title("Quit and discard your changes?")
            .set_description("Your changes in Settings are not saved.")
            // The first button is the default: Cancel, as discarding is
            // the risky answer
            .set_buttons(rfd::MessageButtons::OkCancelCustom(
                "Cancel".to_string(),
                "Quit".to_string(),
            ))
            .show();
        answer == rfd::MessageDialogResult::Custom("Quit".to_string())
    }

    fn init(&mut self) {
        match TrayIconBuilder::new().with_tooltip("Capycal").build() {
            Ok(tray) => self.tray = Some(tray),
            Err(e) => error!(error = %e, "cannot create the menu bar icon"),
        }
        self._main_menu = main_menu();
        self.refresh_menu();
    }

    fn refresh_menu(&mut self) {
        self.next_menu_refresh = Instant::now() + MENU_REFRESH;
        self.refresh_icon();
        self.refresh_problems();
        let Some(tray) = &self.tray else {
            return;
        };
        let items = menu::build(&self.snapshot, Utc::now());
        // Replacing the menu closes it if it is open, so only do that when
        // items come or go
        if let Some((shown, handles)) = &mut self.menu
            && menu::same_shape(shown, &items)
        {
            if *shown != items {
                for (handle, item) in handles.iter().zip(&items) {
                    handle.update(item);
                }
                *shown = items;
            }
            return;
        }
        match tray_menu(&items) {
            Ok((menu, handles)) => {
                tray.set_menu(Some(Box::new(menu)));
                self.menu = Some((items, handles));
            }
            Err(e) => error!(error = %e, "cannot build the menu"),
        }
    }

    fn refresh_icon(&mut self) {
        let Some(tray) = &self.tray else {
            return;
        };
        let status = menu::tray_status(&self.snapshot, Utc::now());
        if self.tray_status.as_ref() != Some(&status) {
            set_tray_icon(tray, status.0, &status.1);
            self.tray_status = Some(status);
        }
    }

    fn on_update(&mut self, update: Update) {
        match update {
            Update::Snapshot(snapshot) => {
                self.snapshot = snapshot;
                if let Some(screen) = &mut self.settings
                    && (screen.set_statuses(&self.snapshot.syncs, chrono::Utc::now())
                        | screen.set_problem(self.snapshot.problem.as_ref()))
                {
                    self.settings_replies(vec![settings_view::Reply::Show]);
                }
                self.refresh_menu();
                self.open_settings_on_first_start();
                self.ask_sponsor();
            }
            Update::ConfigReady(Ok(())) => self.open_config(),
            Update::ConfigReady(Err(message)) => self.show_error("Edit Config", &message),
            Update::ReloadFailed => self.show_config_problem(),
            Update::KeychainNotice(accounts) => self.explain_keychain(&accounts),
            Update::AccountCalendars { .. }
            | Update::AccountTested(_)
            | Update::CalendarAppCalendars(_)
            | Update::NextSourceEvent { .. }
            | Update::SettingsSaved(_)
            | Update::ConfigChanged => self.settings_update(&update),
            Update::PurgePreview(preview) => {
                // A closed window means the user cancelled while counting
                if let Some((window, dialog)) = &mut self.purge_window {
                    dialog.counted(&preview);
                    window.show(dialog.view());
                    window.focus();
                }
            }
            Update::PurgeProgress { ref sync, done, .. } => {
                self.settings_update(&update);
                if let Some((window, dialog)) = &mut self.purge_window {
                    dialog.progress(sync, done);
                    window.show(dialog.view());
                }
            }
            // The window may be closed while removing; the menu shows failures
            Update::Purged(outcomes) => {
                if let Some((window, dialog)) = &mut self.purge_window {
                    dialog.finished(&outcomes);
                    window.show(dialog.view());
                }
            }
            Update::CalendarCounted { calendar, events } => {
                if let Some((window, dialog)) = &mut self.purge_all_window
                    && dialog.calendar() == calendar
                {
                    dialog.counted(&events);
                    window.show(dialog.view());
                    window.focus();
                }
            }
            Update::CalendarPurgeProgress { calendar, done } => {
                if let Some((window, dialog)) = &mut self.purge_all_window
                    && dialog.calendar() == calendar
                {
                    dialog.progress(done);
                    window.show(dialog.view());
                }
            }
            Update::CalendarPurged { calendar, result } => {
                if let Some((window, dialog)) = &mut self.purge_all_window
                    && dialog.calendar() == calendar
                {
                    dialog.finished(&result);
                    window.show(dialog.view());
                }
            }
        }
    }

    fn on_action(&mut self, action: Action) {
        match action {
            Action::SyncNow => self.send(Command::SyncNow),
            Action::Settings => self.open_settings(),
            Action::ShowProblems => self.show_problems(),
            Action::About => self.show_about(),
            Action::Quit => {}
        }
    }

    fn send(&self, command: Command) {
        if self.commands.send(command).is_err() {
            error!("the sync thread is not running");
        }
    }

    /// Stop the syncs, waiting at most `SHUTDOWN_WAIT`.
    fn quit(&mut self) {
        info!("quitting");
        // The app disappears at once, while the syncs stop in the background
        self.tray = None;
        self.log_window = None;
        self.about_window = None;
        self.acknowledgements_window = None;
        self.error_window = None;
        self.problems_window = None;
        self.purge_window = None;
        self.purge_all_window = None;
        self.sponsor_window = None;
        self.keychain_window = None;
        // A running save finishes first: the backend handles commands in order
        self.settings_question = None;
        self.wizard_window = None;
        self.settings_window = None;
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        self.send(Command::Shutdown(done_tx));
        if done_rx.recv_timeout(SHUTDOWN_WAIT).is_err() {
            error!("syncs did not stop in time, quitting anyway");
        }
    }

    /// Open the config with the app chosen for `.toml` files, or with the
    /// default text editor if there is none. `open` returns at once.
    fn open_config(&self) {
        let open = |args: &[&str]| {
            std::process::Command::new("open")
                .args(args)
                .arg(&self.config_path)
                .status()
                .is_ok_and(|status| status.success())
        };
        if !open(&[]) && !open(&["-t"]) {
            error!(config = %self.config_path.display(), "cannot open the config in an editor");
        }
    }

    fn show_log(&mut self) {
        if let Some(log) = &self.log_window {
            return log.focus();
        }
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let proxy = self.proxy.clone();
        let notify = move |input| {
            let _ = proxy.send_event(UserEvent::Log(input));
        };
        match LogWindow::open(mtm, &self.logs, DEFAULT_LOG_CAPACITY, notify) {
            Ok(log) => {
                self.log_window = Some(log);
                self.next_log_poll = Instant::now() + LOG_POLL;
            }
            Err(e) => {
                error!(error = %e, "cannot open the log window");
                self.show_error("Show Log", &e);
            }
        }
    }

    /// Open the Remove Synced Events dialog at once and count the events on
    /// the sync thread; `Update::PurgePreview` fills it.
    fn purge(&mut self, sync: String) {
        if let Some((window, _)) = &self.purge_window {
            return window.focus();
        }
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let proxy = self.proxy.clone();
        let notify = move |input| {
            let _ = proxy.send_event(UserEvent::Purge(input));
        };
        let dialog = PurgeDialog::counting(Some(sync));
        let window = DialogWindow::open(mtm, "calendar.badge.minus", None, dialog.view(), notify);
        self.purge_window = Some((window, dialog));
        self.send(Command::CountPurge);
    }

    /// Ask for a sponsorship at a calm moment once it is due (see
    /// `sponsor`). `CAPYCAL_ASK_SPONSOR=1` asks at the next calm moment.
    fn ask_sponsor(&mut self) {
        if self.sponsor_asked || !sponsor::calm(&self.snapshot) {
            return;
        }
        let mut state = load_sponsor_state();
        let before = state;
        let due = state.due(Utc::now().timestamp());
        if state != before {
            save_sponsor_state(&state);
        }
        if !due && std::env::var_os("CAPYCAL_ASK_SPONSOR").is_none() {
            return;
        }
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let proxy = self.proxy.clone();
        let notify = move |input| {
            let _ = proxy.send_event(UserEvent::Sponsor(input));
        };
        self.sponsor_asked = true;
        // The pink of the GitHub Sponsors heart
        let pink = NSColor::colorWithSRGBRed_green_blue_alpha(0.86, 0.38, 0.64, 1.0);
        let window = DialogWindow::open(
            mtm,
            "heart.fill",
            Some(&pink),
            &sponsor::view(&state),
            notify,
        );
        self.sponsor_window = Some(window);
    }

    /// Say why macOS is about to ask for the Keychain; the backend waits
    /// until the note is closed.
    fn explain_keychain(&mut self, accounts: &[String]) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let proxy = self.proxy.clone();
        let notify = move |_| {
            let _ = proxy.send_event(UserEvent::KeychainNotice);
        };
        self.keychain_window = Some(DialogWindow::open(
            mtm,
            "key.fill",
            None,
            &keychain_notice::view(accounts),
            notify,
        ));
    }

    fn on_sponsor_input(&mut self, input: PanelInput) {
        // Dropping the window closes it without another Closed event
        if self.sponsor_window.take().is_none() {
            return;
        }
        let answer = match input {
            PanelInput::Button(i) => Answer::from_button(i),
            PanelInput::Closed => Answer::NotNow,
        };
        let mut state = load_sponsor_state();
        state.answer(answer, Utc::now().timestamp());
        save_sponsor_state(&state);
        info!(?answer, "sponsor dialog answered");
        if let Answer::Sponsor(i) = answer {
            open_url(sponsor::OPTIONS[i].url);
        }
    }

    /// Open the Remove All Events dialog at once and count the events on
    /// the sync thread; `Update::CalendarCounted` fills it.
    fn purge_calendar(&mut self, calendar: String, name: String, syncs: Vec<String>) {
        if let Some((window, _)) = &self.purge_all_window {
            return window.focus();
        }
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let proxy = self.proxy.clone();
        let notify = move |input| {
            let _ = proxy.send_event(UserEvent::PurgeAll(input));
        };
        let dialog = PurgeAllDialog::counting(calendar.clone(), name, syncs);
        let orange = NSColor::systemOrangeColor();
        let window = DialogWindow::open(
            mtm,
            "exclamationmark.triangle.fill",
            Some(&orange),
            dialog.view(),
            notify,
        );
        self.purge_all_window = Some((window, dialog));
        self.send(Command::CountCalendar(calendar));
    }

    fn on_purge_all_input(&mut self, input: PanelInput) {
        let Some((window, dialog)) = &mut self.purge_all_window else {
            return;
        };
        let reply = match input {
            PanelInput::Button(crate::dialog::TYPED) => {
                dialog.typed(&window.entry_text());
                crate::purge_all::Reply::Show
            }
            PanelInput::Button(i) => dialog.press(i),
            PanelInput::Closed => crate::purge_all::Reply::Close,
        };
        match reply {
            crate::purge_all::Reply::Close => self.purge_all_window = None,
            crate::purge_all::Reply::Remove(calendar) => {
                window.show(dialog.view());
                self.send(Command::PurgeCalendar(calendar));
            }
            crate::purge_all::Reply::ShowLog => self.show_log(),
            crate::purge_all::Reply::Show => window.show(dialog.view()),
        }
    }

    fn on_purge_input(&mut self, input: PanelInput) {
        let Some((window, dialog)) = &mut self.purge_window else {
            return;
        };
        let reply = match input {
            PanelInput::Button(i) => dialog.press(i),
            PanelInput::Closed => Reply::Close,
        };
        match reply {
            // Dropping the window closes it without another Closed event
            Reply::Close => self.purge_window = None,
            Reply::Remove(syncs) => {
                window.show(dialog.view());
                self.send(Command::Purge(syncs));
            }
            Reply::ShowLog => self.show_log(),
            Reply::Show => window.show(dialog.view()),
        }
    }

    /// Open the Problems window, or bring it to the front.
    fn show_problems(&mut self) {
        if self.problems_window.is_some() {
            self.refresh_problems();
            if let Some(window) = &self.problems_window {
                window.focus();
            }
            return;
        }
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let proxy = self.proxy.clone();
        let notify = move |input| {
            let _ = proxy.send_event(UserEvent::Problems(input));
        };
        let text = problems::text(&self.snapshot, &self.logs.snapshot());
        match PanelWindow::open(
            mtm,
            "Problems",
            LARGE,
            problems::header(&self.snapshot),
            &PROBLEMS_BUTTONS,
            &text,
            notify,
        ) {
            Ok(window) => self.problems_window = Some(window),
            Err(e) => {
                error!(error = %e, "cannot open the problems window");
                self.show_error("Problems", &e);
            }
        }
    }

    fn refresh_problems(&mut self) {
        if let Some(window) = &mut self.problems_window {
            let logs = self.logs.snapshot();
            window.set_header(problems::header(&self.snapshot));
            window.set_text(&problems::text(&self.snapshot, &logs));
            window.set_empty(problems::nothing_to_show(&self.snapshot, &logs));
        }
    }

    /// The config did not load: Settings shows why on General.
    fn show_config_problem(&mut self) {
        if self.settings_window.is_none() {
            self.last_selection = Some(crate::settings::Selection::General);
            self.open_settings();
        }
    }

    /// A loaded config without syncs: show Settings once per run.
    fn open_settings_on_first_start(&mut self) {
        if !self.snapshot.loaded || self.settings_offered {
            return;
        }
        // Only the first loaded config counts, not one emptied later
        self.settings_offered = true;
        if self.snapshot.problem.is_none() && self.snapshot.syncs.is_empty() {
            self.open_settings();
        }
    }

    /// The config file as it is now, for the Settings window.
    fn read_config(&self) -> Result<(Config, ConfigEditor), String> {
        let editor = ConfigEditor::load(&self.config_path).map_err(|e| e.to_string())?;
        let config = editor.config().map_err(|e| e.to_string())?;
        Ok((config, editor))
    }

    fn open_settings(&mut self) {
        if let Some(window) = &self.settings_window {
            return window.focus();
        }
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        if self.settings.is_none() {
            // A file that cannot be read opens General with the mistake
            let (mut screen, commands) = match self.read_config() {
                Ok((config, editor)) => SettingsScreen::new(&config, &editor, true),
                Err(e) => (
                    SettingsScreen::broken(&self.config_path, e, true),
                    Vec::new(),
                ),
            };
            screen.set_statuses(&self.snapshot.syncs, chrono::Utc::now());
            screen.set_problem(self.snapshot.problem.as_ref());
            screen.set_app_info(autostart::is_enabled(), &self.config_path);
            if let Some(last) = self.last_selection.take() {
                screen.restore(last);
            }
            for command in commands {
                self.send(command);
            }
            self.settings = Some(screen);
        }
        let Some(screen) = &self.settings else {
            return;
        };
        let proxy = self.proxy.clone();
        let notify = move |raw| {
            let _ = proxy.send_event(UserEvent::Settings(raw));
        };
        match SettingsWindow::open(mtm, &screen.view(), notify) {
            Ok(window) => self.settings_window = Some(window),
            Err(e) => error!(error = %e, "cannot open the Settings window"),
        }
        self.show_wizard();
    }

    /// Open, update or close the wizard sheet as the screen says.
    fn show_wizard(&mut self) {
        let (Some(screen), Some(settings)) = (&self.settings, &self.settings_window) else {
            return;
        };
        let Some(view) = screen.wizard_view() else {
            self.wizard_window = None;
            return;
        };
        if let Some(window) = &mut self.wizard_window {
            return window.show(&view);
        }
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let proxy = self.proxy.clone();
        let notify = move |raw| {
            let _ = proxy.send_event(UserEvent::Wizard(raw));
        };
        match SettingsWindow::open_sheet(mtm, &view, notify, settings) {
            Ok(window) => self.wizard_window = Some(window),
            Err(e) => error!(error = %e, "cannot open the wizard"),
        }
    }

    fn settings_input(&mut self, input: SettingsInput) {
        let Some(screen) = &mut self.settings else {
            return;
        };
        let replies = screen.input(input);
        self.settings_replies(replies);
    }

    fn settings_update(&mut self, update: &Update) {
        if self.settings.is_none() {
            return;
        }
        let config = match update {
            Update::ConfigChanged | Update::SettingsSaved(_) => self.read_config().ok(),
            _ => None,
        };
        let Some(screen) = &mut self.settings else {
            return;
        };
        let replies = screen.update(update, config.as_ref().map(|(c, e)| (c, e)));
        self.settings_replies(replies);
        // Closed while saving and the save failed: show the draft and the error
        if self.settings_window.is_none() && matches!(update, Update::SettingsSaved(Err(_))) {
            self.open_settings();
        }
        // Closed while saving: the screen is done once the save is
        if self.settings_window.is_none() && self.settings.as_ref().is_some_and(|s| !s.saving()) {
            self.forget_settings();
        }
    }

    /// Drop the Settings screen, keeping the entry it showed for the next
    /// time it opens.
    fn forget_settings(&mut self) {
        if let Some(screen) = self.settings.take() {
            self.last_selection = Some(screen.selection());
        }
    }

    fn settings_replies(&mut self, replies: Vec<settings_view::Reply>) {
        for reply in replies {
            match reply {
                settings_view::Reply::Show => {
                    if let (Some(window), Some(screen)) =
                        (&mut self.settings_window, &self.settings)
                    {
                        window.show(&screen.view());
                    }
                    self.show_wizard();
                }
                settings_view::Reply::Send(command) => self.send(command),
                settings_view::Reply::Ask {
                    confirm,
                    then,
                    alternative,
                } => self.ask_settings_question(&confirm, *then, alternative),
                settings_view::Reply::Quit => self.quitting = true,
                settings_view::Reply::SetStartAtLogin(on) => {
                    if let Err(e) = autostart::set_enabled(on) {
                        error!(error = %e, "cannot change start at login");
                        self.show_error("Start at Login", &e);
                    }
                }
                settings_view::Reply::Purge(sync) => self.purge(sync),
                settings_view::Reply::PurgeCalendar {
                    calendar,
                    name,
                    syncs,
                } => self.purge_calendar(calendar, name, syncs),
                settings_view::Reply::ShowLog => self.show_log(),
                settings_view::Reply::OpenUrl(url) => open_url(url),
                settings_view::Reply::RevealConfig => {
                    if let Err(e) = std::process::Command::new("open")
                        .arg("-R")
                        .arg(&self.config_path)
                        .spawn()
                    {
                        error!(error = %e, "cannot show the config file");
                    }
                }
                settings_view::Reply::Close => {
                    self.settings_question = None;
                    self.wizard_window = None;
                    self.settings_window = None;
                    if self.settings.as_ref().is_some_and(|s| !s.saving()) {
                        self.forget_settings();
                    }
                }
            }
        }
    }

    fn ask_settings_question(
        &mut self,
        confirm: &Confirm,
        then: SettingsInput,
        alternative: Option<(&'static str, SettingsInput)>,
    ) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let proxy = self.proxy.clone();
        let notify = move |input| {
            let _ = proxy.send_event(UserEvent::SettingsQuestion(input));
        };
        let view = settings_view::question_view(confirm, alternative.as_ref().map(|(l, _)| *l));
        let window = DialogWindow::open(mtm, "questionmark.circle", None, &view, notify);
        self.settings_question = Some((window, then, alternative.map(|(_, input)| input)));
    }

    fn on_settings_question(&mut self, input: PanelInput) {
        let Some((_, then, alternative)) = self.settings_question.take() else {
            return;
        };
        // Buttons: Cancel, the alternative if any, the main button last
        let chosen = match input {
            PanelInput::Button(0) | PanelInput::Closed => None,
            PanelInput::Button(1) if alternative.is_some() => alternative,
            PanelInput::Button(_) => Some(then),
        };
        if let Some(input) = chosen {
            self.settings_input(SettingsInput::Confirmed(Box::new(input)));
        }
    }

    /// Report that `what` failed, in the app's dialog style.
    fn show_error(&mut self, what: &str, message: &str) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let proxy = self.proxy.clone();
        let notify = move |_| {
            let _ = proxy.send_event(UserEvent::ErrorClosed);
        };
        let view = View {
            heading: format!("{what} failed"),
            message: message.to_string(),
            lines: Vec::new(),
            buttons: &["OK"],
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
        self.error_window = Some(DialogWindow::open(
            mtm,
            "exclamationmark.triangle.fill",
            Some(&NSColor::systemOrangeColor()),
            &view,
            notify,
        ));
    }

    fn show_about(&mut self) {
        if let Some(window) = &self.about_window {
            return window.focus();
        }
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let proxy = self.proxy.clone();
        let notify = move |input| {
            let _ = proxy.send_event(UserEvent::About(input));
        };
        self.about_window = Some(AboutWindow::open(mtm, notify));
    }

    /// Open the Acknowledgements window, or bring it to the front.
    fn show_acknowledgements(&mut self) {
        if let Some(window) = &self.acknowledgements_window {
            return window.focus();
        }
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let proxy = self.proxy.clone();
        let notify = move |input| {
            let _ = proxy.send_event(UserEvent::Acknowledgements(input));
        };
        let (title, header) = crate::dialog::ACKNOWLEDGEMENTS;
        match PanelWindow::open(
            mtm,
            title,
            LARGE,
            header,
            &[],
            &crate::dialog::acknowledgements(),
            notify,
        ) {
            Ok(window) => self.acknowledgements_window = Some(window),
            Err(e) => {
                error!(error = %e, "cannot open the acknowledgements window");
                self.show_error(title, &e);
            }
        }
    }

    fn export_log(&mut self) {
        let Some(path) = FileDialog::new()
            .set_title("Export Log")
            .set_file_name(logview::export_file_name(Utc::now()))
            .save_file()
        else {
            return;
        };
        let content = logview::export_content(&self.log_file, &self.logs);
        match std::fs::write(&path, content) {
            Ok(()) => info!(path = %path.display(), "log exported"),
            Err(e) => {
                let message = format!("{}: {e}", path.display());
                error!(error = %message, "cannot export the log");
                self.show_error("Export Log", &message);
            }
        }
    }
}

/// Menu bar icons drawn by `packaging/macos/icon/make-icon.py`, monochrome
/// PDFs used as template images. The error icon has a gap for the red dot.
const TRAY: &[u8] = include_bytes!("assets/tray.pdf");
const TRAY_SYNCING: &[u8] = include_bytes!("assets/tray-syncing.pdf");
const TRAY_ERROR: &[u8] = include_bytes!("assets/tray-error.pdf");
/// The size of the icon in points, the calendar about as big as other menu bar icons
const TRAY_SIZE: f64 = 18.0;
/// The dot of the error icon in the 16 x 16 box of the PDFs, from the bottom left
const TRAY_DOT: (f64, f64, f64) = (12.2, 16.0 - 12.4, 3.0);

fn tray_image(icon: Icon) -> Option<Retained<NSImage>> {
    let pdf = match icon {
        Icon::Error => TRAY_ERROR,
        Icon::Syncing => TRAY_SYNCING,
        Icon::Inactive | Icon::Ok => TRAY,
    };
    let image = crate::dialog_window_macos::pdf_image(pdf)?;
    let size = NSSize::new(TRAY_SIZE, TRAY_SIZE);
    image.setSize(size);
    if icon != Icon::Error {
        return Some(image);
    }
    // The calendar in the menu bar's text color and a red dot. The handler
    // runs when the icon is drawn, so the color follows the appearance
    let draw = RcBlock::new(move |rect: NSRect| {
        image.drawInRect(rect);
        // Opaque like the other menu bar icons, labelColor is 85 % white in dark mode
        NSColor::labelColor().colorWithAlphaComponent(1.0).set();
        NSRectFillUsingOperation(rect, NSCompositingOperation::SourceAtop);
        let scale = rect.size.width / 16.0;
        let (x, y, r) = TRAY_DOT;
        NSColor::systemRedColor().set();
        NSBezierPath::bezierPathWithOvalInRect(NSRect::new(
            NSPoint::new((x - r) * scale, (y - r) * scale),
            NSSize::new(2.0 * r * scale, 2.0 * r * scale),
        ))
        .fill();
        Bool::YES
    });
    Some(NSImage::imageWithSize_flipped_drawingHandler(
        size, false, &draw,
    ))
}

/// Put the icon on the status item directly. tray-icon's own icon path
/// re-encodes the image as PNG for ImageIO, which crashed with SIGBUS in
/// `PNGReadPlugin` depending on the process environment.
fn set_tray_icon(tray: &TrayIcon, icon: Icon, tooltip: &str) {
    let (Some(mtm), Some(item)) = (MainThreadMarker::new(), tray.ns_status_item()) else {
        return;
    };
    let Some(button) = item.button(mtm) else {
        return;
    };
    let Some(image) = tray_image(icon) else {
        error!("cannot load the menu bar icon");
        return;
    };
    image.setAccessibilityDescription(Some(&NSString::from_str(tooltip)));
    button.setImage(Some(&image));
    button.setAppearsDisabled(icon == Icon::Inactive);
    // tray-icon's click target copies the button frame, which was empty
    // without an image; setting the tooltip resizes it
    if let Err(e) = tray.set_tooltip(Some(tooltip)) {
        error!(error = %e, "cannot update the menu bar icon");
    }
}

fn tray_menu(items: &[Item]) -> tray_icon::menu::Result<(Menu, Vec<MenuHandle>)> {
    let menu = Menu::new();
    let handles = items
        .iter()
        .map(MenuHandle::new)
        .collect::<tray_icon::menu::Result<Vec<_>>>()?;
    for handle in &handles {
        menu.append(handle.as_item())?;
    }
    Ok((menu, handles))
}

/// A shown menu item, mirroring an `Item`.
enum MenuHandle {
    Text(MenuItem),
    Separator(PredefinedMenuItem),
}

impl MenuHandle {
    fn new(item: &Item) -> tray_icon::menu::Result<Self> {
        Ok(match item {
            Item::Status(text) => Self::Text(MenuItem::new(text, false, None)),
            Item::Separator => Self::Separator(PredefinedMenuItem::separator()),
            Item::Action {
                action,
                label,
                enabled,
            } => Self::Text(MenuItem::with_id(action.id(), label, *enabled, None)),
        })
    }

    fn as_item(&self) -> &dyn IsMenuItem {
        match self {
            Self::Text(item) => item,
            Self::Separator(item) => item,
        }
    }

    /// Show `item`, which has the same shape (`menu::same_shape`).
    fn update(&self, item: &Item) {
        match (self, item) {
            (Self::Text(handle), Item::Status(text)) => handle.set_text(text),
            (Self::Text(handle), Item::Action { label, enabled, .. }) => {
                handle.set_text(label);
                handle.set_enabled(*enabled);
            }
            _ => {}
        }
    }
}

const SPONSOR_FIRST_SEEN: &str = "SponsorFirstSeen";
const SPONSOR_NEXT: &str = "SponsorNext";
const SPONSOR_SPONSORING: &str = "SponsorSponsoring";
const SPONSOR_NEVER: &str = "SponsorNever";

/// The sponsor state from the app's user defaults; 0 is a missing time.
fn load_sponsor_state() -> SponsorState {
    let defaults = NSUserDefaults::standardUserDefaults();
    let time = |key: &str| {
        let secs = defaults.doubleForKey(&NSString::from_str(key)) as i64;
        (secs != 0).then_some(secs)
    };
    SponsorState {
        first_seen: time(SPONSOR_FIRST_SEEN),
        next: time(SPONSOR_NEXT),
        sponsoring: defaults.boolForKey(&NSString::from_str(SPONSOR_SPONSORING)),
        never: defaults.boolForKey(&NSString::from_str(SPONSOR_NEVER)),
    }
}

fn save_sponsor_state(state: &SponsorState) {
    let defaults = NSUserDefaults::standardUserDefaults();
    for (key, time) in [
        (SPONSOR_FIRST_SEEN, state.first_seen),
        (SPONSOR_NEXT, state.next),
    ] {
        defaults.setDouble_forKey(time.unwrap_or(0) as f64, &NSString::from_str(key));
    }
    for (key, set) in [
        (SPONSOR_SPONSORING, state.sponsoring),
        (SPONSOR_NEVER, state.never),
    ] {
        defaults.setBool_forKey(set, &NSString::from_str(key));
    }
}

fn open_url(url: &str) {
    if let Err(e) = std::process::Command::new("open").arg(url).spawn() {
        error!(error = %e, url, "cannot open the link");
    }
}

/// The menu bar of the app while one of its windows is open (the app is
/// then in the Dock): the app menu, Edit for the text fields, Window.
fn main_menu() -> Option<Menu> {
    use tray_icon::menu::accelerator::{Accelerator, Code, Modifiers};
    let command = |code| Some(Accelerator::new(Modifiers::META, code));
    let app = Submenu::with_items(
        "Capycal",
        true,
        &[
            &MenuItem::with_id(Action::About.id(), "About Capycal", true, None),
            &PredefinedMenuItem::separator(),
            &MenuItem::with_id(
                Action::Settings.id(),
                "Settings\u{2026}",
                true,
                command(Code::Comma),
            ),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::hide(None),
            &PredefinedMenuItem::hide_others(None),
            &PredefinedMenuItem::show_all(None),
            &PredefinedMenuItem::separator(),
            &MenuItem::with_id(Action::Quit.id(), "Quit Capycal", true, command(Code::KeyQ)),
        ],
    );
    let window = Submenu::with_items(
        "Window",
        true,
        &[
            &PredefinedMenuItem::minimize(None),
            &PredefinedMenuItem::close_window(None),
        ],
    );
    let edit = Submenu::with_items(
        "Edit",
        true,
        &[
            &PredefinedMenuItem::undo(None),
            &PredefinedMenuItem::redo(None),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::cut(None),
            &PredefinedMenuItem::copy(None),
            &PredefinedMenuItem::paste(None),
            &PredefinedMenuItem::select_all(None),
        ],
    );
    let menu = match (app, edit, window) {
        (Ok(app), Ok(edit), Ok(window)) => Menu::with_items(&[&app, &edit, &window]),
        (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => Err(e),
    };
    match menu {
        Ok(menu) => {
            menu.init_for_nsapp();
            Some(menu)
        }
        Err(e) => {
            error!(error = %e, "cannot build the main menu");
            None
        }
    }
}

/// Bring the app to the front, so its dialogs are not hidden behind other apps.
/// `activate()` may decline for an accessory app since macOS 14.
#[allow(deprecated)]
fn activate() {
    if let Some(mtm) = MainThreadMarker::new() {
        NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
    }
}

/// An error before the event loop runs, as a system alert.
fn alert_before_start(title: &str, message: &str) {
    activate();
    MessageDialog::new()
        .set_level(MessageLevel::Error)
        .set_title(title)
        .set_description(message)
        .show();
}
