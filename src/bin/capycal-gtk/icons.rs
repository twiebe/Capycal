//! The app's icons in the icon theme. Packages install them to
//! /usr/share/icons/hicolor; a build run from elsewhere writes them to the
//! user cache and adds that to the theme's search path.

use gtk::{gdk, glib};
use tracing::error;

pub const APP: &str = "app.capycal";
pub const HEART: &str = "capycal-heart-symbolic";
pub const CALENDAR_REMOVE: &str = "capycal-calendar-remove-symbolic";
pub const SYNC: &str = "capycal-sync-symbolic";
pub const CHECK: &str = "capycal-check-symbolic";
/// The tray icon per state (see `tray::CapycalTray::icon_name`).
pub const TRAY: &str = "capycal-tray-symbolic";
pub const TRAY_ERROR: &str = "capycal-tray-error-symbolic";
pub const TRAY_SYNCING: &str = "capycal-tray-syncing-symbolic";
pub const TRAY_INACTIVE: &str = "capycal-tray-inactive-symbolic";

const APP_PNG: &[u8] =
    include_bytes!("../../../packaging/macos/icon/AppIcon.iconset/icon_256x256.png");
const SYMBOLIC: [(&str, &[u8]); 8] = [
    (TRAY, include_bytes!("icons/capycal-tray-symbolic.svg")),
    (
        TRAY_ERROR,
        include_bytes!("icons/capycal-tray-error-symbolic.svg"),
    ),
    (
        TRAY_SYNCING,
        include_bytes!("icons/capycal-tray-syncing-symbolic.svg"),
    ),
    (
        TRAY_INACTIVE,
        include_bytes!("icons/capycal-tray-inactive-symbolic.svg"),
    ),
    (CHECK, include_bytes!("icons/capycal-check-symbolic.svg")),
    (SYNC, include_bytes!("icons/capycal-sync-symbolic.svg")),
    (HEART, include_bytes!("icons/capycal-heart-symbolic.svg")),
    (
        CALENDAR_REMOVE,
        include_bytes!("icons/capycal-calendar-remove-symbolic.svg"),
    ),
];

/// Make the bundled icons available unless the theme has them already.
pub fn install() {
    if theme().is_some_and(|theme| theme.has_icon(APP)) {
        return;
    }
    write(&format!("256x256/apps/{APP}.png"), APP_PNG);
    for (name, svg) in SYMBOLIC {
        write(&format!("scalable/apps/{name}.svg"), svg);
    }
}

/// Register `svg` as a symbolic icon named after `title` and return its
/// name, so GTK draws it in the text color.
pub fn symbolic(title: &str, svg: &[u8]) -> String {
    let slug: String = title
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let name = format!("capycal-{slug}-symbolic");
    write(&format!("scalable/apps/{name}.svg"), svg);
    name
}

/// Where `install` writes the icons when they are not installed; the tray
/// host looks there too.
pub fn cache_dir() -> std::path::PathBuf {
    glib::user_cache_dir().join("capycal").join("icons")
}

fn theme() -> Option<gtk::IconTheme> {
    gdk::Display::default().map(|display| gtk::IconTheme::for_display(&display))
}

/// Write an icon below the cache's hicolor directory and search there.
fn write(relative: &str, data: &[u8]) {
    let dir = cache_dir();
    let file = dir.join("hicolor").join(relative);
    let written = std::fs::create_dir_all(file.parent().unwrap_or(&dir))
        .and_then(|()| std::fs::write(&file, data));
    if let Err(e) = written {
        return error!(file = %file.display(), error = %e, "cannot write an icon");
    }
    if let Some(theme) = theme()
        && !theme.search_path().contains(&dir)
    {
        theme.add_search_path(&dir);
    }
}
