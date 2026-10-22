//! capycal-ui: menu bar app that runs the syncs of one config.
//!
//! Only macOS has a GUI for now. The display-free parts (menu model, log view
//! model, autostart plist, backend) also build on other platforms for their
//! tests.

#[cfg(target_os = "macos")]
mod about_window_macos;
#[cfg(any(target_os = "macos", test))]
mod account_form;
#[cfg(target_os = "macos")]
mod app;
#[cfg(any(target_os = "macos", test))]
mod args;
#[cfg(any(target_os = "macos", test))]
mod autostart;
#[cfg(any(target_os = "macos", test))]
mod backend;
#[cfg(any(target_os = "macos", test))]
mod dialog;
#[cfg(target_os = "macos")]
mod dialog_window_macos;
#[cfg(any(target_os = "macos", test))]
mod keychain_notice;
#[cfg(target_os = "macos")]
mod log_window_macos;
#[cfg(any(target_os = "macos", test))]
mod logview;
#[cfg(any(target_os = "macos", test))]
mod menu;
#[cfg(target_os = "macos")]
mod panel_window_macos;
#[cfg(any(target_os = "macos", test))]
mod picker;
#[cfg(any(target_os = "macos", test))]
mod preview;
#[cfg(any(target_os = "macos", test))]
mod problems;
#[cfg(any(target_os = "macos", test))]
mod purge;
#[cfg(any(target_os = "macos", test))]
mod purge_all;
#[cfg(any(target_os = "macos", test))]
mod settings;
#[cfg(any(target_os = "macos", test))]
mod settings_view;
#[cfg(target_os = "macos")]
mod settings_window_macos;
#[cfg(any(target_os = "macos", test))]
mod sponsor;
#[cfg(any(target_os = "macos", test))]
mod sync_form;
#[cfg(target_os = "macos")]
mod text_window_macos;
#[cfg(any(target_os = "macos", test))]
mod transform_form;

#[cfg(target_os = "macos")]
fn main() -> std::process::ExitCode {
    let config_path = args::config_path_from_args(std::env::args().skip(1))
        .or_else(capycal::default_config_path)
        .unwrap_or_else(|| "capycal.toml".into());
    app::run(config_path)
}

#[cfg(not(target_os = "macos"))]
fn main() -> std::process::ExitCode {
    eprintln!("capycal-ui is only available on macOS");
    std::process::ExitCode::FAILURE
}
