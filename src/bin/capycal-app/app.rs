//! GTK glue: the application, the backend thread, the tray and the actions
//! of its menu. Logic stays in the shared models.

use std::cell::RefCell;
use std::path::PathBuf;
use std::process::ExitCode;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use capycal::{
    Config, ConfigEditor, DEFAULT_LOG_CAPACITY, LogBuffer, LogFile, LogLevel, default_log_file,
    init_with_buffer,
};
use chrono::Utc;
use gtk::{gdk_pixbuf, gio, glib};
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tracing::{error, info, warn};

use crate::autostart;
use crate::backend::{Backend, Command, Snapshot, Update, ensure_config_file, other_desktop_app};
use crate::dialog_window::{DialogInput, DialogWindow};
use crate::icons;
use crate::log_window::LogWindow;
use crate::menu::{self, Action};
use crate::menu_window::MenuWindow;
use crate::problems;
use crate::purge::{PurgeDialog, Reply};
use crate::purge_all::PurgeAllDialog;
use crate::settings_view::{self, SettingsInput, SettingsScreen};
use crate::settings_window::SettingsWindow;
use crate::sponsor::{self, Answer, SponsorState};
use crate::text_window::TextWindow;
use crate::tray_icon::Rgba;

const APP_ID: &str = icons::APP;
/// How often the menu is refreshed for "synced N min ago".
/// Buttons, the chosen item of a drop-down and segments in regular weight,
/// like on the Mac (Adwaita draws them bold).
const BASE_CSS: &str = "button { font-weight: normal; }";
const MENU_REFRESH: Duration = Duration::from_secs(30);
/// How long to wait for a tray host before saying there is none.
const TRAY_HOST_WAIT: Duration = Duration::from_secs(10);
/// How long Quit waits for the syncs to stop (see the macOS app).
const SHUTDOWN_WAIT: Duration = Duration::from_secs(35);

/// The app icon in the sizes the tray may pick from.
const PROBLEMS_BUTTONS: [&str; 3] = ["Edit Config", "Reload Config", "Show Log"];

const ICONS: [&[u8]; 2] = [
    include_bytes!("../../../packaging/macos/icon/AppIcon.iconset/icon_32x32.png"),
    include_bytes!("../../../packaging/macos/icon/AppIcon.iconset/icon_32x32@2x.png"),
];

struct App {
    app: adw::Application,
    config_path: PathBuf,
    log_file: LogFile,
    logs: LogBuffer,
    commands: UnboundedSender<Command>,
    snapshot: Snapshot,
    #[cfg(target_os = "linux")]
    tray: Option<crate::tray::Tray>,
    /// A tray host shows the icon.
    hosted: bool,
    log_window: Option<LogWindow>,
    problems_window: Option<TextWindow>,
    acknowledgements_window: Option<TextWindow>,
    menu_window: Option<MenuWindow>,
    purge_window: Option<(DialogWindow, PurgeDialog)>,
    purge_all_window: Option<(DialogWindow, PurgeAllDialog)>,
    sponsor_window: Option<DialogWindow>,
    /// The Settings screen lives on while its save runs, even if the
    /// window was closed.
    settings: Option<SettingsScreen>,
    /// The entry the Settings window showed when it closed.
    last_selection: Option<crate::settings::Selection>,
    settings_window: Option<SettingsWindow>,
    /// The new-sync wizard, modal on the Settings window.
    wizard_window: Option<SettingsWindow>,
    /// A question of the Settings window and the inputs for its confirming
    /// buttons: the main one and an optional alternative.
    settings_question: Option<(DialogWindow, SettingsInput, Option<SettingsInput>)>,
    /// Settings was opened for a config without syncs in this run.
    settings_offered: bool,
    /// The sponsor dialog was shown in this run of the app.
    sponsor_asked: bool,
    /// To let windows report back.
    this: std::rc::Weak<RefCell<App>>,
}

pub fn run(config_path: PathBuf) -> ExitCode {
    // The log level comes from the config if it loads; problems are reported later
    let level = capycal::Config::load(&config_path)
        .map(|c| c.daemon.log_level)
        .unwrap_or_else(|_| "info".to_string());
    let log_file = default_log_file("capycal-app")
        .unwrap_or_else(|| std::env::temp_dir().join("capycal-app.log"));
    let (logs, log_file, log_level) =
        match init_with_buffer(&level, &log_file, DEFAULT_LOG_CAPACITY) {
            Ok(logs) => logs,
            Err(e) => {
                eprintln!("capycal-app: {e}");
                return ExitCode::FAILURE;
            }
        };
    info!(version = capycal::VERSION, config = %config_path.display(), "capycal-app started");
    match ensure_config_file(&config_path) {
        Ok(true) => info!(config = %config_path.display(), "starter config written"),
        Ok(false) => {}
        Err(e) => {
            error!(config = %config_path.display(), error = %e, "cannot write the starter config")
        }
    }
    if let Err(e) = autostart::repair(&config_path) {
        warn!(error = %e, "cannot update the autostart entry");
    }

    // A second start activates the running instance instead
    let application = adw::Application::builder().application_id(APP_ID).build();
    let state: Rc<RefCell<Option<Rc<RefCell<App>>>>> = Rc::default();
    let started = state.clone();
    // Startup runs once; it takes what the app needs
    let setup = RefCell::new(Some((config_path, log_file, logs, log_level)));
    application.connect_startup(move |application| {
        if let Some((config_path, log_file, logs, log_level)) = setup.take() {
            // Without D-Bus (macOS) a second start is not forwarded to the
            // running app; it would only find the config locked
            if let Some(holder) = other_desktop_app(&config_path) {
                return already_running(application, &holder.pid);
            }
            let app = App::start(application, config_path, log_file, logs, log_level);
            *started.borrow_mut() = Some(app);
        }
    });
    // The first activation is the start; later ones are the app started
    // again, which shows the menu as a window. Without a tray (macOS) the
    // window is the menu from the start.
    let activated = std::cell::Cell::new(false);
    application.connect_activate(move |_| {
        let again = activated.replace(true);
        if (again || cfg!(not(target_os = "linux")))
            && let Some(app) = state.borrow().as_ref()
        {
            App::show_menu_window(app);
        }
    });
    let code = application.run_with_args::<&str>(&[]);
    if code == glib::ExitCode::SUCCESS {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

impl App {
    fn start(
        application: &adw::Application,
        config_path: PathBuf,
        log_file: LogFile,
        logs: LogBuffer,
        log_level: LogLevel,
    ) -> Rc<RefCell<App>> {
        // Without windows the app keeps running for its tray icon
        std::mem::forget(application.hold());
        if let Some(display) = gtk::gdk::Display::default() {
            let provider = gtk::CssProvider::new();
            provider.load_from_string(BASE_CSS);
            gtk::style_context_add_provider_for_display(
                &display,
                &provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }

        let (updates_tx, updates) = async_channel::unbounded();
        let (actions_tx, actions) = async_channel::unbounded::<Action>();
        let (hosted_tx, hosted) = async_channel::unbounded::<bool>();
        let commands = spawn_backend(config_path.clone(), log_level, updates_tx);
        #[cfg(not(target_os = "linux"))]
        drop((actions_tx, hosted_tx));

        let app = Rc::new(RefCell::new(App {
            app: application.clone(),
            config_path,
            log_file,
            logs,
            commands,
            snapshot: Snapshot::default(),
            #[cfg(target_os = "linux")]
            tray: crate::tray::Tray::spawn(app_icons(), actions_tx, hosted_tx),
            hosted: false,
            log_window: None,
            problems_window: None,
            acknowledgements_window: None,
            menu_window: None,
            purge_window: None,
            purge_all_window: None,
            sponsor_window: None,
            settings: None,
            last_selection: None,
            settings_window: None,
            wizard_window: None,
            settings_question: None,
            settings_offered: false,
            sponsor_asked: false,
            this: std::rc::Weak::new(),
        }));
        app.borrow_mut().this = Rc::downgrade(&app);
        icons::install();

        let receiver = app.clone();
        glib::spawn_future_local(async move {
            while let Ok(update) = updates.recv().await {
                receiver.borrow_mut().on_update(update);
            }
        });
        let receiver = app.clone();
        glib::spawn_future_local(async move {
            while let Ok(action) = actions.recv().await {
                App::on_action(&receiver, action);
            }
        });
        let receiver = app.clone();
        glib::spawn_future_local(async move {
            while let Ok(online) = hosted.recv().await {
                receiver.borrow_mut().hosted = online;
            }
        });
        // At login the panel may start after the app; then the icon shows up.
        // Elsewhere there is no tray and the menu is a window from the start
        if cfg!(target_os = "linux") {
            let checker = app.clone();
            glib::timeout_add_local_once(TRAY_HOST_WAIT, move || {
                if !checker.borrow().hosted {
                    App::no_tray_host(&checker);
                }
            });
        }
        let refresher = app.clone();
        glib::timeout_add_seconds_local(MENU_REFRESH.as_secs() as u32, move || {
            refresher.borrow_mut().refresh_tray();
            glib::ControlFlow::Continue
        });
        app.borrow_mut().refresh_tray();
        app
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
                self.refresh_tray();
                self.refresh_problems();
                self.open_settings_on_first_start();
                self.ask_sponsor();
            }
            Update::ConfigReady(Ok(())) => self.open_config(),
            Update::ConfigReady(Err(message)) => show_error("Edit Config", &message),
            Update::ReloadFailed => self.show_config_problem(),
            // Only macOS asks before an app reads a Keychain entry
            Update::KeychainNotice(_) => self.send(Command::KeychainNoticeClosed),
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

    fn refresh_tray(&mut self) {
        let now = Utc::now();
        let items = menu::build(&self.snapshot, now);
        if let Some(window) = &mut self.menu_window {
            window.update(&items);
        }
        let (state, tooltip) = menu::tray_status(&self.snapshot, now);
        #[cfg(target_os = "linux")]
        if let Some(tray) = &self.tray {
            tray.update(&items, state, &tooltip);
        }
        #[cfg(not(target_os = "linux"))]
        let _ = (items, state, tooltip);
    }

    fn on_action(this: &Rc<RefCell<App>>, action: Action) {
        let app = this.borrow();
        match action {
            Action::SyncNow => app.send(Command::SyncNow),
            Action::Settings => {
                drop(app);
                this.borrow_mut().open_settings();
            }
            Action::ShowProblems => {
                drop(app);
                this.borrow_mut().show_problems();
            }
            Action::About => {
                let this = Rc::downgrade(this);
                crate::about_window::show(move || {
                    defer(&this, |app| app.show_acknowledgements());
                });
            }
            Action::Quit => {
                drop(app);
                this.borrow_mut().quit();
            }
        }
    }

    /// Without a tray host the app has no visible UI; show the menu as a
    /// window with a hint about the missing tray.
    fn no_tray_host(this: &Rc<RefCell<App>>) {
        error!("no tray host (StatusNotifierWatcher) shows the icon");
        App::show_menu_window(this);
        if let Some(window) = &this.borrow().menu_window {
            window.show_tray_hint(true);
        }
    }

    fn show_menu_window(this: &Rc<RefCell<App>>) {
        let mut app = this.borrow_mut();
        if let Some(window) = &app.menu_window {
            return window.present();
        }
        let activate = {
            let this = app.this.clone();
            move |action| {
                let this = this.clone();
                glib::idle_add_local_once(move || {
                    if let Some(app) = this.upgrade() {
                        App::on_action(&app, action);
                    }
                });
            }
        };
        let weak = app.this.clone();
        let closed = move || {
            defer(&weak, |app| app.menu_window = None);
        };
        app.menu_window = Some(MenuWindow::open(&app.app, activate, closed));
        drop(app);
        this.borrow_mut().refresh_tray();
    }

    fn show_log(&mut self) {
        if let Some(window) = &self.log_window {
            return window.present();
        }
        let this = self.this.clone();
        // The window is closing itself; forget it after that
        let closed = move || defer(&this, |app| app.log_window = None);
        self.log_window = Some(LogWindow::open(
            &self.app,
            &self.logs,
            &self.log_file,
            DEFAULT_LOG_CAPACITY,
            closed,
        ));
    }

    fn show_problems(&mut self) {
        if let Some(window) = &self.problems_window {
            window.present();
            return self.refresh_problems();
        }
        let this = self.this.clone();
        let later = self.this.clone();
        let window = TextWindow::new(
            &self.app,
            "Problems",
            &PROBLEMS_BUTTONS,
            move |index| {
                defer(&this, move |app| match index {
                    0 => app.send(Command::EditConfig),
                    1 => app.send(Command::Reload),
                    _ => app.show_log(),
                });
            },
            move || {
                defer(&later, |app| app.problems_window = None);
            },
        );
        self.problems_window = Some(window);
        self.refresh_problems();
    }

    /// Open the Acknowledgements window, or bring it to the front.
    fn show_acknowledgements(&mut self) {
        if let Some(window) = &self.acknowledgements_window {
            return window.present();
        }
        let later = self.this.clone();
        let (title, header) = crate::dialog::ACKNOWLEDGEMENTS;
        let mut window = TextWindow::new(
            &self.app,
            title,
            &[],
            |_| {},
            move || defer(&later, |app| app.acknowledgements_window = None),
        );
        window.set_heading(header);
        window.set_text(&crate::dialog::acknowledgements());
        self.acknowledgements_window = Some(window);
    }

    fn refresh_problems(&mut self) {
        if let Some(window) = &mut self.problems_window {
            let logs = self.logs.snapshot();
            window.set_heading(problems::header(&self.snapshot));
            window.set_text(&problems::text(&self.snapshot, &logs));
            window.set_empty(problems::nothing_to_show(&self.snapshot, &logs));
        }
    }

    /// Open the Remove Synced Events dialog at once and count the events on
    /// the sync thread; `Update::PurgePreview` fills it.
    fn purge(&mut self, sync: String) {
        if self.purge_window.is_some() {
            return;
        }
        let this = self.this.clone();
        let notify = move |input| {
            defer(&this, move |app| app.on_purge_input(input));
        };
        let dialog = PurgeDialog::counting(Some(sync));
        let window = DialogWindow::open(
            "Remove Synced Events",
            icons::CALENDAR_REMOVE,
            None,
            dialog.view(),
            notify,
        );
        self.purge_window = Some((window, dialog));
        self.send(Command::CountPurge);
    }

    /// Open the Remove All Events dialog at once and count the events on
    /// the sync thread; `Update::CalendarCounted` fills it.
    fn purge_calendar(&mut self, calendar: String, name: String, syncs: Vec<String>) {
        if self.purge_all_window.is_some() {
            return;
        }
        let this = self.this.clone();
        let notify = move |input| {
            defer(&this, move |app| app.on_purge_all_input(input));
        };
        let dialog = PurgeAllDialog::counting(calendar.clone(), name, syncs);
        let window = DialogWindow::open(
            "Remove All Events",
            "dialog-warning-symbolic",
            Some("#e66100"),
            dialog.view(),
            notify,
        );
        self.purge_all_window = Some((window, dialog));
        self.send(Command::CountCalendar(calendar));
    }

    fn on_purge_all_input(&mut self, input: DialogInput) {
        let Some((window, dialog)) = &mut self.purge_all_window else {
            return;
        };
        let reply = match input {
            DialogInput::Button(crate::dialog::TYPED) => {
                dialog.typed(&window.entry_text());
                crate::purge_all::Reply::Show
            }
            DialogInput::Button(i) => dialog.press(i),
            DialogInput::Closed => crate::purge_all::Reply::Close,
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

    fn on_purge_input(&mut self, input: DialogInput) {
        let Some((window, dialog)) = &mut self.purge_window else {
            return;
        };
        let reply = match input {
            DialogInput::Button(i) => dialog.press(i),
            DialogInput::Closed => Reply::Close,
        };
        match reply {
            Reply::Close => self.purge_window = None,
            Reply::Remove(syncs) => {
                window.show(dialog.view());
                self.send(Command::Purge(syncs));
            }
            Reply::ShowLog => self.show_log(),
            Reply::Show => window.show(dialog.view()),
        }
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
        self.sponsor_asked = true;
        let this = self.this.clone();
        let notify = move |input| {
            defer(&this, move |app| app.on_sponsor_input(input));
        };
        // The pink of the GitHub Sponsors heart
        let window = DialogWindow::open(
            "Support Capycal",
            icons::HEART,
            Some("#db61a2"),
            &sponsor::view(&state),
            notify,
        );
        self.sponsor_window = Some(window);
    }

    fn on_sponsor_input(&mut self, input: DialogInput) {
        // Dropping the window closes it without another Closed event
        if self.sponsor_window.take().is_none() {
            return;
        }
        let answer = match input {
            DialogInput::Button(i) => Answer::from_button(i),
            DialogInput::Closed => Answer::NotNow,
        };
        let mut state = load_sponsor_state();
        state.answer(answer, Utc::now().timestamp());
        save_sponsor_state(&state);
        info!(?answer, "sponsor dialog answered");
        if let Answer::Sponsor(i) = answer {
            open_uri(sponsor::OPTIONS[i].url);
        }
    }

    fn send(&self, command: Command) {
        if self.commands.send(command).is_err() {
            error!("the sync thread is not running");
        }
    }

    fn open_config(&self) {
        let file = gio::File::for_path(&self.config_path);
        gtk::FileLauncher::new(Some(&file)).launch(
            None::<&gtk::Window>,
            None::<&gio::Cancellable>,
            |result| {
                if let Err(e) = result {
                    error!(error = %e, "cannot open the config in an editor");
                }
            },
        );
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
            return window.present();
        }
        if self.settings.is_none() {
            // A file that cannot be read opens General with the mistake
            let (mut screen, commands) = match self.read_config() {
                Ok((config, editor)) => {
                    SettingsScreen::new(&config, &editor, cfg!(target_os = "macos"))
                }
                Err(e) => (
                    SettingsScreen::broken(&self.config_path, e, cfg!(target_os = "macos")),
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
        let this = self.this.clone();
        let notify = move |input| {
            defer(&this, move |app| {
                // The window moved on: an open question no longer applies
                app.settings_question = None;
                app.settings_input(input);
            });
        };
        self.settings_window = Some(SettingsWindow::open(
            &self.app,
            &screen.view(),
            notify,
            None,
        ));
        self.show_wizard();
    }

    /// Open, update or close the wizard window as the screen says.
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
        let this = self.this.clone();
        let notify = move |input| {
            // Closing the wizard cancels it, Settings stays
            let input = match input {
                SettingsInput::CloseRequested => {
                    SettingsInput::Pressed(settings_view::FieldId::WizardCancel)
                }
                other => other,
            };
            defer(&this, move |app| {
                app.settings_question = None;
                app.settings_input(input);
            });
        };
        self.wizard_window = Some(SettingsWindow::open(
            &self.app,
            &view,
            notify,
            Some(settings),
        ));
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
                } => {
                    let view = settings_view::question_view(
                        &confirm,
                        alternative.as_ref().map(|(l, _)| *l),
                    );
                    let this = self.this.clone();
                    let notify = move |input| {
                        defer(&this, move |app| app.on_settings_question(input));
                    };
                    let window = DialogWindow::open("Settings", icons::APP, None, &view, notify);
                    self.settings_question =
                        Some((window, *then, alternative.map(|(_, input)| input)));
                }
                settings_view::Reply::Quit => {
                    self.quit();
                    return;
                }
                settings_view::Reply::SetStartAtLogin(on) => {
                    if let Err(e) = autostart::set_enabled(on, &self.config_path) {
                        error!(error = %e, "cannot change start at login");
                        show_error("Start at Login", &e);
                    }
                }
                settings_view::Reply::Purge(sync) => self.purge(sync),
                settings_view::Reply::PurgeCalendar {
                    calendar,
                    name,
                    syncs,
                } => self.purge_calendar(calendar, name, syncs),
                settings_view::Reply::ShowLog => self.show_log(),
                settings_view::Reply::OpenUrl(url) => open_uri(url),
                // The Linux app is updated by its packages
                settings_view::Reply::SetCheckUpdates(_)
                | settings_view::Reply::CheckUpdatesNow => {}
                settings_view::Reply::RevealConfig => {
                    let file = gtk::gio::File::for_path(&self.config_path);
                    gtk::FileLauncher::new(Some(&file)).open_containing_folder(
                        None::<&gtk::Window>,
                        None::<&gtk::gio::Cancellable>,
                        |result| {
                            if let Err(e) = result {
                                error!(error = %e, "cannot show the config file");
                            }
                        },
                    );
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

    fn on_settings_question(&mut self, input: DialogInput) {
        let Some((_, then, alternative)) = self.settings_question.take() else {
            return;
        };
        // Buttons: Cancel, the alternative if any, the main button last
        let chosen = match input {
            DialogInput::Button(0) | DialogInput::Closed => None,
            DialogInput::Button(1) if alternative.is_some() => alternative,
            DialogInput::Button(_) => Some(then),
        };
        if let Some(input) = chosen {
            self.settings_input(SettingsInput::Confirmed(Box::new(input)));
        }
    }

    /// Stop the syncs, waiting at most `SHUTDOWN_WAIT`, then leave.
    fn quit(&mut self) {
        info!("quitting");
        #[cfg(target_os = "linux")]
        {
            self.tray = None;
        }
        // Dropping the windows destroys them without their close
        // callbacks, which would need the app borrowed by this call
        self.log_window = None;
        self.problems_window = None;
        self.acknowledgements_window = None;
        self.menu_window = None;
        self.purge_window = None;
        self.purge_all_window = None;
        self.sponsor_window = None;
        // A running save finishes first: the backend handles commands in order
        self.settings_question = None;
        self.wizard_window = None;
        self.settings_window = None;
        for window in self.app.windows() {
            window.destroy();
        }
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        self.send(Command::Shutdown(done_tx));
        if done_rx.recv_timeout(SHUTDOWN_WAIT).is_err() {
            error!("syncs did not stop in time, quitting anyway");
        }
        self.app.quit();
    }
}

/// Run `f` on the app from the main loop, once the callback that asks for
/// it (a window callback, while the app may be borrowed) has returned.
fn defer(app: &std::rc::Weak<RefCell<App>>, f: impl FnOnce(&mut App) + 'static) {
    let app = app.clone();
    glib::idle_add_local_once(move || {
        if let Some(app) = app.upgrade() {
            f(&mut app.borrow_mut());
        }
    });
}

/// Start the tokio runtime with the backend on its own thread.
fn spawn_backend(
    config_path: PathBuf,
    log_level: LogLevel,
    updates: async_channel::Sender<Update>,
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
                let _ = updates.try_send(update);
            })
            .with_log_level(log_level);
            runtime.block_on(backend.run(rx));
        })
        .expect("cannot spawn the sync thread");
    tx
}

/// Where the GTK app keeps the sponsor dialog's answers.
fn sponsor_file() -> PathBuf {
    glib::user_config_dir().join("capycal").join("sponsor")
}

fn load_sponsor_state() -> SponsorState {
    std::fs::read_to_string(sponsor_file())
        .map(|text| SponsorState::from_text(&text))
        .unwrap_or_default()
}

fn save_sponsor_state(state: &SponsorState) {
    let file = sponsor_file();
    let written = std::fs::create_dir_all(file.parent().unwrap_or(&file))
        .and_then(|()| std::fs::write(&file, state.to_text()));
    if let Err(e) = written {
        error!(file = %file.display(), error = %e, "cannot save the sponsor answer");
    }
}

/// The app icon decoded to RGBA in each size of `ICONS`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn app_icons() -> Vec<Rgba> {
    ICONS
        .iter()
        .filter_map(|png| {
            let stream = gio::MemoryInputStream::from_bytes(&glib::Bytes::from_static(png));
            let pixbuf =
                gdk_pixbuf::Pixbuf::from_stream(&stream, None::<&gio::Cancellable>).ok()?;
            let pixbuf = if pixbuf.has_alpha() {
                pixbuf
            } else {
                pixbuf.add_alpha(false, 0, 0, 0).ok()?
            };
            let size = pixbuf.width() as usize;
            let stride = pixbuf.rowstride() as usize;
            let bytes = pixbuf.read_pixel_bytes();
            let pixels = (0..pixbuf.height() as usize)
                .flat_map(|row| bytes[row * stride..row * stride + size * 4].to_vec())
                .collect();
            Some(Rgba { size, pixels })
        })
        .collect()
}

/// Say that another instance runs the config, then quit.
fn already_running(application: &adw::Application, pid: &str) {
    info!(pid, "Capycal already runs this config, quitting");
    let hold = application.hold();
    let dialog = adw::AlertDialog::new(
        Some("Capycal is already running"),
        Some(&format!(
            "Another Capycal app (pid {pid}) runs this config. Use its tray icon or menu \
             window, or quit it first."
        )),
    );
    dialog.add_response("close", "Close");
    let application = application.clone();
    dialog.connect_response(None, move |_, _| {
        let _ = &hold;
        application.quit();
    });
    dialog.present(None::<&gtk::Widget>);
}

fn show_error(heading: &str, message: &str) {
    let dialog = adw::AlertDialog::new(Some(heading), Some(message));
    dialog.add_response("close", "Close");
    dialog.present(None::<&gtk::Widget>);
}

/// Open `url` in the default browser.
pub fn open_uri(url: &str) {
    gtk::UriLauncher::new(url).launch(None::<&gtk::Window>, None::<&gio::Cancellable>, |result| {
        if let Err(e) = result {
            error!(error = %e, "cannot open the link");
        }
    });
}
