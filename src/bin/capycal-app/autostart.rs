//! "Start at Login" on Linux desktops: an XDG autostart entry in
//! `~/.config/autostart`.

use std::path::{Path, PathBuf};

const FILE_NAME: &str = "app.capycal.desktop";

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
    Some(config_home.join("autostart").join(FILE_NAME))
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
/// one, e.g. `capycal-gtk`, as the app was called up to 0.1.0.
pub fn repair(config: &Path) -> Result<(), String> {
    let Some(entry) = entry_path().and_then(|path| std::fs::read_to_string(path).ok()) else {
        return Ok(());
    };
    let exe = std::env::current_exe().map_err(|e| format!("cannot find capycal-app: {e}"))?;
    if starts(&entry, &exe) {
        return Ok(());
    }
    set_enabled(true, config)
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
    fn test_an_entry_of_another_binary_is_repaired() {
        let entry = desktop_entry(Path::new("/usr/bin/capycal-gtk"), Path::new("/c.toml"));
        assert!(starts(&entry, Path::new("/usr/bin/capycal-gtk")));
        assert!(!starts(&entry, Path::new("/usr/bin/capycal-app")));
    }
}
