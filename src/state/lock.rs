use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Seek, Write};
use std::path::{Path, PathBuf};

use uuid::Uuid;

use crate::config::Config;
use crate::error::{Error, Result};

/// Exclusive claim on a config's state, released when dropped (or when the
/// process exits).
///
/// The lock is an advisory `flock` on `<state_dir>/locks/<hash>.lock`, where
/// the hash is derived from the canonical config path. The file records pid,
/// config path and sync names, so other processes can report the holder and
/// detect configs that would write the same state files.
#[derive(Debug)]
pub struct ConfigLock {
    file: File,
    path: PathBuf,
    content: String,
}

/// What a lock file says about its holder.
struct Holder {
    pid: String,
    program: String,
    config: String,
    syncs: Vec<String>,
}

/// The process that holds a config's lock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockHolder {
    pub pid: String,
    /// File name of its executable, e.g. `capycal` or `capycal-app`.
    pub program: String,
}

impl LockHolder {
    /// The holder is one of the desktop apps, not the CLI.
    pub fn is_desktop_app(&self) -> bool {
        ["capycal-ui", "capycal-app"].contains(&self.program.as_str())
    }
}

impl ConfigLock {
    /// Lock the config at `config_path` for this process. Fails if another
    /// process holds the same config, or holds a different config with the
    /// same state directory and a sync of the same name.
    pub fn acquire(config: &Config, config_path: &Path) -> Result<Self> {
        let (config_path, path) = lock_path(config, config_path)?;
        let dir = path.parent().unwrap_or(&path);
        std::fs::create_dir_all(dir).map_err(state_err(dir))?;

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(state_err(&path))?;
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                return Err(Error::Locked {
                    config: config_path,
                    pid: read_holder(&path).pid,
                });
            }
            Err(TryLockError::Error(e)) => return Err(state_err(&path)(e)),
        }

        let mut lock = Self {
            file,
            path,
            content: String::new(),
        };
        lock.write(holder_content(config, &config_path))?;
        // Both of two configs starting at once write their own lock file
        // before scanning, so at least one of them sees the other's syncs
        check_collisions(&lock.path, &sync_names(config))?;
        Ok(lock)
    }

    /// Record the sync names of a reloaded `config` without releasing the
    /// lock. A config whose state directory changed is locked anew. On a
    /// collision the lock keeps its previous sync names.
    pub fn update(&mut self, config: &Config, config_path: &Path) -> Result<()> {
        let (config_path, path) = lock_path(config, config_path)?;
        if path != self.path {
            *self = Self::acquire(config, &config_path)?;
            return Ok(());
        }
        let previous = std::mem::take(&mut self.content);
        self.write(holder_content(config, &config_path))?;
        if let Err(e) = check_collisions(&self.path, &sync_names(config)) {
            self.write(previous)?;
            return Err(e);
        }
        Ok(())
    }

    fn write(&mut self, content: String) -> Result<()> {
        self.file
            .set_len(0)
            .and_then(|()| self.file.rewind())
            .and_then(|()| self.file.write_all(content.as_bytes()))
            .map_err(state_err(&self.path))?;
        self.content = content;
        Ok(())
    }
}

/// The canonical config path and its lock file `<state_dir>/locks/<hash>.lock`.
fn lock_path(config: &Config, config_path: &Path) -> Result<(PathBuf, PathBuf)> {
    let config_path =
        std::fs::canonicalize(config_path).unwrap_or_else(|_| config_path.to_path_buf());
    let path = config
        .resolve_state_dir()?
        .join("locks")
        .join(format!("{}.lock", short_hash(&config_path)));
    Ok((config_path, path))
}

fn sync_names(config: &Config) -> Vec<&str> {
    config.syncs.iter().map(|s| s.name.as_str()).collect()
}

/// The process that holds the lock of the config at `config_path`, None if
/// no process does. Probes with a shared lock, which fails while a holder
/// has the exclusive one.
pub fn lock_holder(config: &Config, config_path: &Path) -> Result<Option<LockHolder>> {
    let (_, path) = lock_path(config, config_path)?;
    let file = match File::open(&path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(state_err(&path)(e)),
    };
    match file.try_lock_shared() {
        Ok(()) => Ok(None),
        Err(TryLockError::WouldBlock) => {
            let holder = read_holder(&path);
            Ok(Some(LockHolder {
                pid: holder.pid,
                program: holder.program,
            }))
        }
        Err(TryLockError::Error(e)) => Err(state_err(&path)(e)),
    }
}

fn holder_content(config: &Config, config_path: &Path) -> String {
    let program = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_default();
    let mut content = format!(
        "pid={}\nprogram={program}\nconfig={}\n",
        std::process::id(),
        config_path.display()
    );
    for sync in sync_names(config) {
        content.push_str(&format!("sync={sync}\n"));
    }
    content
}

/// Fail if a lock file next to `own`, held by a running process, names one of `syncs`.
fn check_collisions(own: &Path, syncs: &[&str]) -> Result<()> {
    let dir = own.parent().unwrap_or(own);
    for entry in std::fs::read_dir(dir).map_err(state_err(dir))? {
        let path = entry.map_err(state_err(dir))?.path();
        if path == own || path.extension().is_none_or(|ext| ext != "lock") {
            continue;
        }
        // A lock nobody holds is left over from a process that exited
        let Ok(file) = File::open(&path) else {
            continue;
        };
        if !matches!(file.try_lock_shared(), Err(TryLockError::WouldBlock)) {
            continue;
        }
        let holder = read_holder(&path);
        if let Some(sync) = syncs.iter().find(|s| holder.syncs.iter().any(|h| h == *s)) {
            return Err(Error::StateCollision {
                sync: sync.to_string(),
                other_config: holder.config,
                pid: holder.pid,
            });
        }
    }
    Ok(())
}

fn read_holder(path: &Path) -> Holder {
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let mut holder = Holder {
        pid: "unknown".to_string(),
        program: String::new(),
        config: "unknown".to_string(),
        syncs: Vec::new(),
    };
    for line in content.lines() {
        match line.split_once('=') {
            Some(("pid", pid)) => holder.pid = pid.to_string(),
            Some(("program", program)) => holder.program = program.to_string(),
            Some(("config", config)) => holder.config = config.to_string(),
            Some(("sync", sync)) => holder.syncs.push(sync.to_string()),
            _ => {}
        }
    }
    holder
}

/// Stable across builds and platforms, unlike `std::hash`, so every capycal
/// binary picks the same lock file for a config.
fn short_hash(path: &Path) -> String {
    let uuid = Uuid::new_v5(&Uuid::NAMESPACE_URL, path.as_os_str().as_encoded_bytes());
    uuid.simple().to_string()[..16].to_string()
}

fn state_err(path: &Path) -> impl FnOnce(std::io::Error) -> Error {
    let path: PathBuf = path.to_path_buf();
    move |source| Error::State { path, source }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("capycal-{}-lock-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn config(state_dir: &Path, sync: &str) -> Config {
        toml::from_str(&format!(
            r#"
[daemon]
state_dir = "{}"

[calendars.a]
url = "https://example.com/a/"

[calendars.b]
url = "https://example.com/b/"

[syncs."{sync}"]
source = "a"
target = "b"
interval = "1h"
fields = ["DTSTART"]
"#,
            state_dir.display()
        ))
        .unwrap()
    }

    #[test]
    fn test_second_lock_on_same_config_fails_until_released() {
        let dir = test_dir("same");
        let cfg = config(&dir, "a-to-b");
        let path = dir.join("capycal.toml");

        let lock = ConfigLock::acquire(&cfg, &path).unwrap();
        let err = ConfigLock::acquire(&cfg, &path).unwrap_err();
        let pid = std::process::id().to_string();
        assert!(
            matches!(&err, Error::Locked { config, pid: p } if config == &path && *p == pid),
            "{err:?}"
        );
        assert!(err.to_string().contains("capycal.toml"), "{err}");
        assert!(err.to_string().contains(&format!("pid {pid}")), "{err}");

        drop(lock);
        ConfigLock::acquire(&cfg, &path).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_lock_holder_names_the_running_process() {
        let dir = test_dir("holder");
        let cfg = config(&dir, "a-to-b");
        let path = dir.join("capycal.toml");
        assert_eq!(lock_holder(&cfg, &path).unwrap(), None);

        let lock = ConfigLock::acquire(&cfg, &path).unwrap();
        let holder = lock_holder(&cfg, &path).unwrap().unwrap();
        assert_eq!(holder.pid, std::process::id().to_string());
        // The test binary, named after the crate
        assert!(holder.program.starts_with("capycal-"), "{holder:?}");
        assert!(!holder.is_desktop_app());
        // Probing does not take the lock away
        assert!(ConfigLock::acquire(&cfg, &path).is_err());

        drop(lock);
        assert_eq!(lock_holder(&cfg, &path).unwrap(), None);
        ConfigLock::acquire(&cfg, &path).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_different_configs_lock_in_parallel() {
        let dir = test_dir("parallel");
        let _first = ConfigLock::acquire(&config(&dir, "one"), &dir.join("one.toml")).unwrap();
        let _second = ConfigLock::acquire(&config(&dir, "two"), &dir.join("two.toml")).unwrap();
        assert_eq!(std::fs::read_dir(dir.join("locks")).unwrap().count(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_same_sync_name_in_same_state_dir_collides() {
        let dir = test_dir("collision");
        let first_path = dir.join("first.toml");
        let first = ConfigLock::acquire(&config(&dir, "shared"), &first_path).unwrap();

        let second = config(&dir, "shared");
        let err = ConfigLock::acquire(&second, &dir.join("second.toml")).unwrap_err();
        assert!(
            matches!(&err, Error::StateCollision { sync, other_config, .. }
                if sync == "shared" && *other_config == first_path.display().to_string()),
            "{err:?}"
        );

        // The lock file of a config that is no longer running is ignored
        drop(first);
        ConfigLock::acquire(&second, &dir.join("second.toml")).unwrap();

        // A different state dir never collides
        let other_dir = test_dir("collision-other");
        let _third = ConfigLock::acquire(&config(&other_dir, "shared"), &first_path).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&other_dir);
    }

    #[test]
    fn test_update_records_new_syncs_and_keeps_old_ones_on_collision() {
        let dir = test_dir("update");
        let path = dir.join("capycal.toml");
        let mut lock = ConfigLock::acquire(&config(&dir, "old"), &path).unwrap();
        let _other = ConfigLock::acquire(&config(&dir, "taken"), &dir.join("other.toml")).unwrap();

        lock.update(&config(&dir, "new"), &path).unwrap();
        assert_eq!(read_holder(&lock.path).syncs, ["new"]);
        // Still locked: a second process cannot take the config
        assert!(ConfigLock::acquire(&config(&dir, "new"), &path).is_err());

        let err = lock.update(&config(&dir, "taken"), &path).unwrap_err();
        assert!(matches!(err, Error::StateCollision { .. }), "{err:?}");
        assert_eq!(read_holder(&lock.path).syncs, ["new"]);

        // A new state dir moves the lock
        let moved = test_dir("update-moved");
        lock.update(&config(&moved, "new"), &path).unwrap();
        assert!(lock.path.starts_with(&moved));
        ConfigLock::acquire(&config(&dir, "new"), &path).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&moved);
    }

    #[test]
    fn test_short_hash_is_stable() {
        // Pinned: a changed hash would let an old and a new binary run the same config
        assert_eq!(
            short_hash(Path::new("/etc/capycal.toml")),
            "04c9633e0c065dbf"
        );
        assert_ne!(
            short_hash(Path::new("/etc/capycal.toml")),
            short_hash(Path::new("/etc/other.toml"))
        );
    }
}
