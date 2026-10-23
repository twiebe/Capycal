//! Brings a config file written by an older Capycal up to the current
//! version, one step per version, keeping comments and order.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use toml_edit::{DocumentMut, Item, Key, TableLike, value};

use crate::config_edit::{max_position, table_at};
use tracing::{info, warn};

/// The config version this Capycal writes. A file without `version` is 1.
pub const CONFIG_VERSION: i64 = 2;

/// Migrate `doc` to `CONFIG_VERSION`. Returns what changed, or nothing when
/// the document is current and untouched.
pub fn migrate(doc: &mut DocumentMut) -> Result<Vec<String>, String> {
    let version = match doc.get("version") {
        None => 1,
        Some(item) => match item.as_integer() {
            Some(v) if v >= 1 => v,
            _ => {
                return Err(format!(
                    "version must be a positive integer, not {}",
                    item.to_string().trim()
                ));
            }
        },
    };
    if version > CONFIG_VERSION {
        return Err(format!(
            "config version {version} is newer than this Capycal reads (up to \
             {CONFIG_VERSION}): update Capycal"
        ));
    }
    if version == CONFIG_VERSION {
        return Ok(Vec::new());
    }
    let mut notes = Vec::new();
    if version < 2 {
        v1_to_v2(doc, &mut notes);
    }
    set_version(doc);
    notes.push(format!("set version to {CONFIG_VERSION}"));
    Ok(notes)
}

/// Replace the file at `path` (its text `old`) with the migrated `new`,
/// keeping `old` as a timestamped backup next to the real file. A file that
/// cannot be written stays as it is: the caller runs with `new` in memory.
pub(crate) fn write_back(path: &Path, old: &str, new: &str, notes: &[String]) {
    let written = (|| -> std::io::Result<PathBuf> {
        let real = std::fs::canonicalize(path)?;
        let name = real.file_name().unwrap_or_default().to_string_lossy();
        let stamp = chrono::Utc::now()
            .with_timezone(&crate::ical::transform::local_tz())
            .format("%Y%m%d-%H%M%S");
        let backup = real.with_file_name(format!("{name}.{stamp}.bak"));
        crate::config_edit::write_private(&backup, old, &real)?;
        crate::config_edit::write_atomic(&real, new)?;
        Ok(backup)
    })();
    match written {
        Ok(backup) => {
            for note in notes {
                info!(config = %path.display(), "{note}");
            }
            info!(backup = %backup.display(), "updated the config to version {CONFIG_VERSION}");
        }
        Err(e) => warn!(
            config = %path.display(),
            error = %e,
            "could not update the config to version {CONFIG_VERSION}, running with the update \
             in memory. Make the file writable and Capycal updates it on the next start or reload"
        ),
    }
}

/// The window of a Calendar.app source applies to every source in 0.3.0.
const WINDOW_KEYS: [(&str, &str); 3] = [
    ("eventkit_days_past", "days_past"),
    ("eventkit_days_future", "days_future"),
    ("eventkit_all_events", "all_events"),
];

/// 0.3.0: a full sync runs once a day instead of every `full_sync_interval`;
/// the `eventkit_*` window keys are generic.
fn v1_to_v2(doc: &mut DocumentMut, notes: &mut Vec<String>) {
    let eventkit: HashSet<String> = doc
        .get("calendars")
        .and_then(Item::as_table_like)
        .map(|calendars| {
            calendars
                .iter()
                .filter(|(_, c)| c.get("eventkit_calendar").is_some())
                .map(|(name, _)| name.to_string())
                .collect()
        })
        .unwrap_or_default();
    for (name, sync) in syncs(doc) {
        if sync.remove("full_sync_interval").is_some() {
            notes.push(format!(
                "sync '{name}': removed full_sync_interval, Capycal now runs a full sync once a day"
            ));
        }
        // Without Calendar.app they did nothing; kept they would turn the window on
        let from_eventkit = sync
            .get("source")
            .and_then(Item::as_str)
            .is_some_and(|source| eventkit.contains(source));
        for (old, new) in WINDOW_KEYS {
            if !sync.contains_key(old) {
                continue;
            }
            if from_eventkit {
                rename(sync, old, new);
                notes.push(format!("sync '{name}': renamed {old} to {new}"));
            } else {
                sync.remove(old);
                notes.push(format!(
                    "sync '{name}': removed {old} (only used for Calendar.app sources)"
                ));
            }
        }
    }
}

/// Rename the key `old` in place, keeping its comments and position.
fn rename(table: &mut dyn TableLike, old: &str, new: &str) {
    let names: Vec<String> = table.iter().map(|(name, _)| name.to_string()).collect();
    let entries: Vec<(Key, Item)> = names
        .iter()
        .filter_map(|name| {
            let (key, item) = table.get_key_value(name)?;
            let key = if name == old {
                Key::new(new).with_leaf_decor(key.leaf_decor().clone())
            } else {
                key.clone()
            };
            Some((key, item.clone()))
        })
        .collect();
    table.clear();
    for (key, item) in entries {
        table.entry_format(&key).or_insert(item);
    }
}

/// Every `[syncs.<name>]` table (or inline table) by name.
fn syncs(doc: &mut DocumentMut) -> Vec<(String, &mut dyn TableLike)> {
    let Some(syncs) = doc.get_mut("syncs").and_then(Item::as_table_like_mut) else {
        return Vec::new();
    };
    syncs
        .iter_mut()
        .filter_map(|(name, item)| Some((name.get().to_string(), item.as_table_like_mut()?)))
        .collect()
}

/// Set `version` as the first key, below a header comment of the file.
fn set_version(doc: &mut DocumentMut) {
    if let Some(item) = doc.get_mut("version") {
        *item = value(CONFIG_VERSION);
        return;
    }
    doc.insert("version", value(CONFIG_VERSION));
    // The comment above the first table: what ends with a blank line is the
    // file's header and stays on top, the rest belongs to the table
    let Some(first) = first_table(doc) else {
        return;
    };
    let prefix = table_at(doc.as_table_mut(), first)
        .and_then(|t| t.decor().prefix())
        .and_then(|p| p.as_str())
        .unwrap_or_default()
        .to_string();
    let (header, rest) = match prefix.rfind("\n\n") {
        Some(i) => prefix.split_at(i + 2),
        None => ("", prefix.trim_start_matches('\n')),
    };
    if let Some(mut key) = doc.as_table_mut().key_mut("version") {
        key.leaf_decor_mut().set_prefix(header);
    }
    if let Some(table) = table_at(doc.as_table_mut(), first) {
        table.decor_mut().set_prefix(format!("\n{rest}"));
    }
}

/// The position of the first table in the file.
fn first_table(doc: &mut DocumentMut) -> Option<isize> {
    let max = max_position(doc.as_table())?;
    (0..=max).find(|p| table_at(doc.as_table_mut(), *p).is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(text: &str) -> (Result<Vec<String>, String>, String) {
        let mut doc: DocumentMut = text.parse().unwrap();
        let notes = migrate(&mut doc);
        (notes, doc.to_string())
    }

    const SYNC: &str = "[syncs.a]\nsource = \"x\"\ntarget = \"y\"\ninterval = \"5m\"\n";

    #[test]
    fn test_current_version_unchanged() {
        let text = format!("version = 2\n\n{SYNC}");
        let (notes, out) = run(&text);
        assert!(notes.unwrap().is_empty());
        assert_eq!(out, text);
    }

    #[test]
    fn test_v1_gets_version() {
        let (notes, out) = run(SYNC);
        assert!(notes.unwrap().contains(&"set version to 2".to_string()));
        assert_eq!(out, format!("version = 2\n\n{SYNC}"));
        let doc: DocumentMut = out.parse().unwrap();
        assert_eq!(doc["version"].as_integer(), Some(2));
    }

    #[test]
    fn test_explicit_v1_is_raised() {
        let (notes, out) = run(&format!("# Mine\nversion = 1\n\n{SYNC}"));
        assert!(!notes.unwrap().is_empty());
        assert_eq!(out, format!("# Mine\nversion = 2\n\n{SYNC}"));
    }

    #[test]
    fn test_version_below_header_comment() {
        let text = "# My config\n# second line\n\n# Accounts\n[accounts.a]\nurl = \"u\"\n\n\
                    # Syncs\n[syncs.a]\nsource = \"x\" # inline\n";
        let (_, out) = run(text);
        assert_eq!(
            out,
            "# My config\n# second line\n\nversion = 2\n\n# Accounts\n[accounts.a]\nurl = \"u\"\n\n\
             # Syncs\n[syncs.a]\nsource = \"x\" # inline\n"
        );
    }

    #[test]
    fn test_version_above_comment_of_first_table() {
        let (_, out) = run("# Accounts\n[accounts.a]\nurl = \"u\"\n");
        assert_eq!(
            out,
            "version = 2\n\n# Accounts\n[accounts.a]\nurl = \"u\"\n"
        );
    }

    #[test]
    fn test_empty_document() {
        let (_, out) = run("");
        assert_eq!(out, "version = 2\n");
    }

    #[test]
    fn test_removes_full_sync_interval() {
        let (notes, out) = run(
            "[syncs.a]\nsource = \"x\"\nfull_sync_interval = \"7d\"\n# keep me\nenabled = false\n\n\
             [syncs.b]\nsource = \"y\"\n",
        );
        let notes = notes.unwrap();
        assert!(
            notes.contains(
                &"sync 'a': removed full_sync_interval, Capycal now runs a full sync once a day"
                    .to_string()
            ),
            "{notes:?}"
        );
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert_eq!(
            out,
            "version = 2\n\n[syncs.a]\nsource = \"x\"\n# keep me\nenabled = false\n\n\
             [syncs.b]\nsource = \"y\"\n"
        );
    }

    #[test]
    fn test_window_keys_renamed_for_calendar_app() {
        let (notes, out) = run(
            "[calendars.mac]\neventkit_calendar = \"Home\"\n\n[calendars.dav]\nurl = \"u\"\n\n\
             [syncs.a]\nsource = \"mac\"\n# how far back\neventkit_days_past = 7 # a week\n\
             target = \"dav\"\neventkit_all_events = true\n\n\
             [syncs.b]\nsource = \"dav\"\neventkit_days_past = 7\neventkit_days_future = 9\n\n\
             [syncs.c]\nsource = \"gone\"\neventkit_all_events = false\n",
        );
        let notes = notes.unwrap();
        for note in [
            "sync 'a': renamed eventkit_days_past to days_past",
            "sync 'a': renamed eventkit_all_events to all_events",
            "sync 'b': removed eventkit_days_past (only used for Calendar.app sources)",
            "sync 'b': removed eventkit_days_future (only used for Calendar.app sources)",
            "sync 'c': removed eventkit_all_events (only used for Calendar.app sources)",
        ] {
            assert!(notes.contains(&note.to_string()), "{note}: {notes:?}");
        }
        assert_eq!(
            out,
            "version = 2\n\n[calendars.mac]\neventkit_calendar = \"Home\"\n\n[calendars.dav]\nurl = \"u\"\n\n\
             [syncs.a]\nsource = \"mac\"\n# how far back\ndays_past = 7 # a week\n\
             target = \"dav\"\nall_events = true\n\n\
             [syncs.b]\nsource = \"dav\"\n\n\
             [syncs.c]\nsource = \"gone\"\n"
        );
    }

    #[test]
    fn test_newer_version_refused() {
        let err = run("version = 3\n").0.unwrap_err();
        assert!(err.contains("newer") && err.contains('3'), "{err}");
    }

    #[test]
    fn test_bad_version_refused() {
        assert!(run("version = \"2\"\n").0.is_err());
        assert!(run("version = 0\n").0.is_err());
    }

    const CONFIG: &str = "# Mine\n\n[accounts.acc]\nurl = \"https://x/\"\nusername = \"u\"\n\
                          password = \"p\"\n\n[calendars.a]\nurl = \"https://x/a/\"\n\
                          account = \"acc\"\n";

    fn test_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("capycal-{}-migrate-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn backups(dir: &Path) -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == "bak"))
            .collect()
    }

    #[cfg(unix)]
    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    #[cfg(unix)]
    fn test_load_migrates_and_backs_up() {
        use std::os::unix::fs::PermissionsExt;
        let dir = test_dir("backup");
        let path = dir.join("capycal.toml");
        std::fs::write(&path, CONFIG).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        let config = crate::config::Config::load(&path).unwrap();
        assert_eq!(config.version, Some(2));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# Mine\n\nversion = 2\n"), "{text}");
        let backups = backups(&dir);
        assert_eq!(backups.len(), 1);
        let name = backups[0]
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();
        assert!(
            name.starts_with("capycal.toml.")
                && name.len() == "capycal.toml.20261023-142501.bak".len(),
            "{name}"
        );
        assert_eq!(std::fs::read_to_string(&backups[0]).unwrap(), CONFIG);
        assert_eq!(mode(&backups[0]), 0o640);
        assert_eq!(mode(&path), 0o640);
    }

    #[test]
    fn test_load_current_writes_nothing() {
        let dir = test_dir("current");
        let path = dir.join("capycal.toml");
        let text = format!("version = 2\n{CONFIG}");
        std::fs::write(&path, &text).unwrap();
        crate::config::Config::load(&path).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        assert!(backups(&dir).is_empty());
    }

    #[test]
    #[cfg(unix)]
    fn test_load_through_symlink() {
        let dir = test_dir("symlink");
        let real = dir.join("real");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("capycal.toml"), CONFIG).unwrap();
        let link = dir.join("capycal.toml");
        std::os::unix::fs::symlink(real.join("capycal.toml"), &link).unwrap();
        crate::config::Config::load(&link).unwrap();
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(
            std::fs::read_to_string(&link)
                .unwrap()
                .contains("version = 2")
        );
        assert_eq!(backups(&real).len(), 1);
        assert!(backups(&dir).is_empty());
    }

    #[test]
    #[cfg(unix)]
    fn test_load_read_only_runs_in_memory() {
        use std::os::unix::fs::PermissionsExt;
        let dir = test_dir("read-only");
        let path = dir.join("capycal.toml");
        std::fs::write(&path, CONFIG).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
        // root writes anyway
        let writable = std::fs::write(dir.join("probe"), "").is_ok();
        let config = crate::config::Config::load(&path);
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        if writable {
            return;
        }
        assert_eq!(config.unwrap().version, Some(2));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), CONFIG);
        assert!(backups(&dir).is_empty());
    }

    #[test]
    fn test_load_newer_version_error() {
        let dir = test_dir("newer");
        let path = dir.join("capycal.toml");
        std::fs::write(&path, format!("version = 9\n{CONFIG}")).unwrap();
        let err = crate::config::Config::load(&path).unwrap_err().to_string();
        assert!(
            err.contains("capycal.toml") && err.contains("newer"),
            "{err}"
        );
    }

    #[test]
    fn test_load_invalid_migrated_config_writes_nothing() {
        let dir = test_dir("invalid");
        let path = dir.join("capycal.toml");
        let text =
            format!("{CONFIG}[syncs.s]\nsource = \"a\"\ntarget = \"missing\"\ninterval = \"1m\"\n");
        std::fs::write(&path, &text).unwrap();
        assert!(crate::config::Config::load(&path).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        assert!(backups(&dir).is_empty());
    }

    #[test]
    fn test_editor_migrates_in_memory() {
        let dir = test_dir("editor");
        let path = dir.join("capycal.toml");
        std::fs::write(&path, CONFIG).unwrap();
        crate::config_edit::ConfigEditor::load(&path)
            .unwrap()
            .save()
            .unwrap();
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("version = 2")
        );
        assert!(backups(&dir).is_empty());
    }
}
