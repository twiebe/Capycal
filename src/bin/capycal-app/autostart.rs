//! "Start at Login" on Linux desktops: an XDG autostart entry in
//! `~/.config/autostart`. A Flatpak cannot write it and asks the Background
//! portal instead, which writes `<app id>.desktop` there.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk::gio;
use gtk::glib::{self, prelude::ToVariant};

use crate::icons;

const APP_ID: &str = "app.capycal";

/// The autostart entry that starts `exe` with the config at `config`.
pub fn desktop_entry(exe: &Path, config: &Path) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Capycal\n\
         Comment=Calendar sync in the background\n\
         Exec={} --config {}\n\
         Icon=app.capycal\n\
         Terminal=false\n\
         X-GNOME-Autostart-enabled=true\n",
        exec_arg(&exe.to_string_lossy()),
        exec_arg(&config.to_string_lossy())
    )
}

/// One argument of an `Exec` line: quoted, with `"`, `` ` ``, `$` and `\`
/// escaped, and `%` doubled (Desktop Entry Specification).
fn exec_arg(arg: &str) -> String {
    let mut quoted = String::from('"');
    for c in arg.chars() {
        match c {
            '"' | '`' | '$' | '\\' => {
                quoted.push_str("\\\\");
                quoted.push(c);
            }
            '%' => quoted.push_str("%%"),
            c => quoted.push(c),
        }
    }
    quoted.push('"');
    quoted
}

fn entry_path() -> Option<PathBuf> {
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    let id = icons::flatpak_id().unwrap_or_else(|| APP_ID.to_string());
    Some(config_home.join("autostart").join(format!("{id}.desktop")))
}

pub fn is_enabled() -> bool {
    entry_path().is_some_and(|path| path.exists())
}

/// Write or remove the autostart entry for this binary and `config`.
pub fn set_enabled(enabled: bool, config: &Path) -> Result<(), String> {
    let path = entry_path().ok_or("neither XDG_CONFIG_HOME nor HOME is set")?;
    if !enabled {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(format!("cannot remove {}: {e}", path.display()))
            }
            _ => Ok(()),
        };
    }
    let exe = std::env::current_exe().map_err(|e| format!("cannot find capycal-app: {e}"))?;
    let config = std::fs::canonicalize(config).unwrap_or_else(|_| config.to_path_buf());
    let dir = path.parent().unwrap_or(&path);
    std::fs::create_dir_all(dir)
        .and_then(|()| std::fs::write(&path, desktop_entry(&exe, &config)))
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// Point an existing autostart entry at this binary when it starts another
/// one, e.g. `capycal-gtk`, as the app was called up to 0.1.0. The portal's
/// entry of a Flatpak starts `flatpak run` and stays as it is.
pub fn repair(config: &Path) -> Result<(), String> {
    if icons::flatpak_id().is_some() {
        return Ok(());
    }
    let Some(entry) = entry_path().and_then(|path| std::fs::read_to_string(path).ok()) else {
        return Ok(());
    };
    let exe = std::env::current_exe().map_err(|e| format!("cannot find capycal-app: {e}"))?;
    if starts(&entry, &exe) {
        return Ok(());
    }
    set_enabled(true, config)
}

/// The options of the Background portal's `RequestBackground`: start the
/// Flatpak's command with `config` at login, or no longer.
pub fn portal_options(enabled: bool, config: &Path, token: &str) -> glib::Variant {
    let options = glib::VariantDict::new(None);
    options.insert_value("handle_token", &token.to_variant());
    options.insert_value(
        "reason",
        &"Run your calendar syncs after you log in".to_variant(),
    );
    options.insert_value("autostart", &enabled.to_variant());
    let command = ["capycal-app", "--config", &config.to_string_lossy()].map(String::from);
    options.insert_value("commandline", &command.to_vec().to_variant());
    options.insert_value("dbus-activatable", &false.to_variant());
    options.end()
}

/// Ask the Background portal to start the Flatpak at login, or no longer.
/// `done` gets the outcome once the desktop answered (it may ask the user).
pub fn request_background(
    enabled: bool,
    config: &Path,
    done: impl FnOnce(Result<(), String>) + 'static,
) {
    let config = std::fs::canonicalize(config).unwrap_or_else(|_| config.to_path_buf());
    gio::bus_get(gio::BusType::Session, gio::Cancellable::NONE, move |bus| {
        let bus = match bus {
            Ok(bus) => bus,
            Err(e) => return done(Err(format!("no session bus: {e}"))),
        };
        // The portal answers on a request object named after us and the token
        let token = format!("capycal{}", glib::random_int());
        let sender = bus.unique_name().unwrap_or_default();
        let handle = format!(
            "/org/freedesktop/portal/desktop/request/{}/{token}",
            sender.trim_start_matches(':').replace('.', "_")
        );
        let done = Rc::new(RefCell::new(Some(done)));
        let subscription = Rc::new(RefCell::new(None));
        let (answer, held) = (done.clone(), subscription.clone());
        *subscription.borrow_mut() = Some(bus.subscribe_to_signal(
            Some("org.freedesktop.portal.Desktop"),
            Some("org.freedesktop.portal.Request"),
            Some("Response"),
            Some(&handle),
            None,
            gio::DBusSignalFlags::NONE,
            move |signal| {
                held.borrow_mut().take();
                let Some(done) = answer.borrow_mut().take() else {
                    return;
                };
                let (response, results) = signal
                    .parameters
                    .get::<(u32, glib::VariantDict)>()
                    .unwrap_or((2, glib::VariantDict::new(None)));
                let autostart = results.lookup::<bool>("autostart").ok().flatten();
                done(if response == 0 && autostart == Some(enabled) {
                    Ok(())
                } else {
                    Err("the desktop did not allow it".to_string())
                });
            },
        ));
        bus.call(
            Some("org.freedesktop.portal.Desktop"),
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.Background",
            "RequestBackground",
            Some(&("", portal_options(enabled, &config, &token)).to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            -1,
            gio::Cancellable::NONE,
            move |result| {
                if let Err(e) = result {
                    subscription.borrow_mut().take();
                    if let Some(done) = done.borrow_mut().take() {
                        done(Err(format!("the Background portal failed: {e}")));
                    }
                }
            },
        );
    });
}

fn starts(entry: &str, exe: &Path) -> bool {
    entry.contains(&format!("\nExec={} ", exec_arg(&exe.to_string_lossy())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_desktop_entry_quotes_its_arguments() {
        let entry = desktop_entry(
            Path::new("/usr/bin/capycal-app"),
            Path::new("/home/a/my \"cal\"/100%.toml"),
        );
        assert!(entry.starts_with("[Desktop Entry]\nType=Application\n"));
        assert!(
            entry.contains(
                "Exec=\"/usr/bin/capycal-app\" --config \"/home/a/my \\\\\"cal\\\\\"/100%%.toml\"\n"
            ),
            "{entry}"
        );
    }

    #[test]
    fn test_portal_options_start_the_wrapper_with_the_config() {
        let config = Path::new("/home/u/.config/capycal/capycal.toml");
        let options = glib::VariantDict::new(Some(&portal_options(true, config, "capycal1")));
        assert_eq!(
            options.lookup::<Vec<String>>("commandline").unwrap(),
            Some(vec![
                "capycal-app".to_string(),
                "--config".to_string(),
                "/home/u/.config/capycal/capycal.toml".to_string()
            ])
        );
        assert_eq!(options.lookup::<bool>("autostart").unwrap(), Some(true));
        assert_eq!(
            options.lookup::<String>("handle_token").unwrap().as_deref(),
            Some("capycal1")
        );
    }

    #[test]
    fn test_portal_options_off_only_clear_autostart() {
        let options =
            glib::VariantDict::new(Some(&portal_options(false, Path::new("/c.toml"), "t")));
        assert_eq!(options.lookup::<bool>("autostart").unwrap(), Some(false));
    }

    #[test]
    fn test_an_entry_of_another_binary_is_repaired() {
        let entry = desktop_entry(Path::new("/usr/bin/capycal-gtk"), Path::new("/c.toml"));
        assert!(starts(&entry, Path::new("/usr/bin/capycal-gtk")));
        assert!(!starts(&entry, Path::new("/usr/bin/capycal-app")));
    }
}
