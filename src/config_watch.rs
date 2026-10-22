//! Watch the config file for changes, for automatic reloads in the front ends.

use std::path::Path;
use std::time::Duration;

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;
use tracing::warn;

use crate::error::{Error, Result};

/// A change is reported once the file was left alone this long.
const DEBOUNCE: Duration = Duration::from_millis(500);

/// Watches the directory of a config file and reports changes to the file
/// itself, including a replace by rename (how many editors save), deletion
/// and re-creation. Changes stop when the watcher is dropped.
pub struct ConfigWatcher {
    _watcher: RecommendedWatcher,
}

impl ConfigWatcher {
    /// Start watching `path`, whose directory must exist. The receiver gets
    /// one `()` per debounced change.
    pub fn new(path: &Path) -> Result<(Self, mpsc::Receiver<()>)> {
        let watch_err = |source| Error::Watch {
            path: path.to_path_buf(),
            source,
        };
        let name = path
            .file_name()
            .ok_or_else(|| Error::Config(format!("{} is not a file path", path.display())))?
            .to_owned();
        let dir = match path.parent() {
            Some(dir) if !dir.as_os_str().is_empty() => dir,
            _ => Path::new("."),
        };
        let (raw_tx, raw_rx) = std::sync::mpsc::channel();
        let mut watcher = notify::recommended_watcher(move |event: notify::Result<Event>| {
            match event {
                // Access events (inotify reports opens) would turn every
                // reload into the next change
                Ok(event) => {
                    if !matches!(event.kind, EventKind::Access(_))
                        && event.paths.iter().any(|p| p.file_name() == Some(&name))
                    {
                        let _ = raw_tx.send(());
                    }
                }
                Err(e) => warn!(error = %e, "config watcher error"),
            }
        })
        .map_err(watch_err)?;
        watcher
            .watch(dir, RecursiveMode::NonRecursive)
            .map_err(watch_err)?;

        let (tx, rx) = mpsc::channel(1);
        // Ends when the watcher, and with it `raw_tx`, is dropped
        std::thread::spawn(move || {
            while raw_rx.recv().is_ok() {
                while raw_rx.recv_timeout(DEBOUNCE).is_ok() {}
                // A full channel already holds an unhandled change
                let _ = tx.try_send(());
            }
        });
        Ok((Self { _watcher: watcher }, rx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tokio::time::timeout;

    const EXPECT: Duration = Duration::from_secs(5);
    const QUIET: Duration = Duration::from_millis(800);

    fn setup(name: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("capycal-{}-watch-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("capycal.toml");
        std::fs::write(&path, "a").unwrap();
        (dir, path)
    }

    async fn changed(rx: &mut mpsc::Receiver<()>) {
        timeout(EXPECT, rx.recv())
            .await
            .expect("no change reported")
            .unwrap();
    }

    async fn quiet(rx: &mut mpsc::Receiver<()>) {
        assert!(
            timeout(QUIET, rx.recv()).await.is_err(),
            "unexpected change"
        );
    }

    #[tokio::test]
    async fn test_quick_writes_are_reported_once() {
        let (dir, path) = setup("writes");
        let (_watcher, mut rx) = ConfigWatcher::new(&path).unwrap();
        for i in 0..5 {
            std::fs::write(&path, format!("v{i}")).unwrap();
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        changed(&mut rx).await;
        quiet(&mut rx).await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_replace_by_rename_is_reported() {
        let (dir, path) = setup("rename");
        let (_watcher, mut rx) = ConfigWatcher::new(&path).unwrap();
        let tmp = dir.join(".capycal.toml.tmp");
        std::fs::write(&tmp, "b").unwrap();
        std::fs::rename(&tmp, &path).unwrap();
        changed(&mut rx).await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_other_files_are_ignored() {
        let (dir, path) = setup("other");
        let (_watcher, mut rx) = ConfigWatcher::new(&path).unwrap();
        // FSEvents may still report the write of `setup` after the start
        let _ = timeout(QUIET, rx.recv()).await;
        std::fs::write(dir.join("other.toml"), "x").unwrap();
        std::fs::create_dir(dir.join("sub")).unwrap();
        // Reading the config, as a reload does, is not a change
        std::fs::read_to_string(&path).unwrap();
        quiet(&mut rx).await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_delete_and_recreate_are_reported() {
        let (dir, path) = setup("recreate");
        let (_watcher, mut rx) = ConfigWatcher::new(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        changed(&mut rx).await;
        std::fs::write(&path, "c").unwrap();
        changed(&mut rx).await;
        let _ = std::fs::remove_dir_all(&dir);
    }
}
