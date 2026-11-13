//! capycal-app: the desktop app for Linux. A tray icon (StatusNotifierItem)
//! runs the syncs of one config; its windows are GTK 4 and libadwaita.
//!
//! The models are shared with the macOS app in `src/bin/capycal-ui/`. The
//! windows also build on macOS (Homebrew's gtk4 and libadwaita) for
//! development; the tray only exists on Linux.

// The shared models serve both apps; each uses only part of them
#[path = "../capycal-ui/account_form.rs"]
#[allow(dead_code)]
mod account_form;
#[path = "../capycal-ui/args.rs"]
mod args;
#[path = "../capycal-ui/backend.rs"]
#[allow(dead_code)]
mod backend;
#[path = "../capycal-ui/dialog.rs"]
#[allow(dead_code)]
mod dialog;
#[path = "../capycal-ui/logview.rs"]
#[allow(dead_code)]
mod logview;
#[path = "../capycal-ui/menu.rs"]
#[allow(dead_code)]
mod menu;
#[path = "../capycal-ui/password_prompt.rs"]
mod password_prompt;
#[path = "../capycal-ui/picker.rs"]
#[allow(dead_code)]
mod picker;
#[path = "../capycal-ui/preview.rs"]
mod preview;
#[path = "../capycal-ui/problems.rs"]
#[allow(dead_code)]
mod problems;
#[path = "../capycal-ui/purge.rs"]
#[allow(dead_code)]
mod purge;
#[path = "../capycal-ui/purge_all.rs"]
#[allow(dead_code)]
mod purge_all;
#[path = "../capycal-ui/settings.rs"]
#[allow(dead_code)]
mod settings;
#[path = "../capycal-ui/settings_view.rs"]
#[allow(dead_code)]
mod settings_view;
#[path = "../capycal-ui/sponsor.rs"]
#[allow(dead_code)]
mod sponsor;
#[path = "../capycal-ui/sync_form.rs"]
#[allow(dead_code)]
mod sync_form;
#[path = "../capycal-ui/transform_form.rs"]
#[allow(dead_code)]
mod transform_form;

mod about_window;
mod app;
mod autostart;
mod dialog_window;
mod icons;
mod log_window;
mod menu_window;
mod settings_window;
mod text_window;
#[cfg(target_os = "linux")]
mod tray;
// Only the Linux tray draws state icons
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod tray_icon;

fn main() -> std::process::ExitCode {
    let config_path = args::config_path_from_args(std::env::args().skip(1))
        .or_else(capycal::default_config_path)
        .unwrap_or_else(|| "capycal.toml".into());
    app::run(config_path)
}

/// Ctrl+W closes a window or dialog like its close button, Ctrl+Q quits
/// (the app's `quit` action).
fn window_keys(widget: &impl gtk::prelude::IsA<gtk::Widget>) {
    use adw::prelude::*;
    let close = gtk::CallbackAction::new(|widget, _| {
        if let Some(dialog) = widget.downcast_ref::<adw::Dialog>() {
            dialog.close();
        } else if let Some(window) = widget.downcast_ref::<gtk::Window>() {
            window.close();
        }
        gtk::glib::Propagation::Stop
    });
    let quit = gtk::CallbackAction::new(|_, _| {
        if let Some(app) = gtk::gio::Application::default() {
            app.activate_action("quit", None);
        }
        gtk::glib::Propagation::Stop
    });
    let keys = gtk::ShortcutController::new();
    for (trigger, action) in [("<Control>w", close), ("<Control>q", quit)] {
        keys.add_shortcut(gtk::Shortcut::new(
            gtk::ShortcutTrigger::parse_string(trigger),
            Some(action),
        ));
    }
    widget.add_controller(keys);
}
