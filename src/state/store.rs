use std::path::PathBuf;

use tracing::warn;

use crate::error::{Error, Result};
use crate::state::{StateStore, SyncState};

pub struct FsStateStore {
    dir: PathBuf,
}

impl FsStateStore {
    pub fn new(dir: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&dir).map_err(|e| Error::State {
            path: dir.clone(),
            source: e,
        })?;
        Ok(Self { dir })
    }

    fn path_for(&self, sync_name: &str) -> PathBuf {
        // Sanitize: only allow alphanumeric, dash, underscore, dot
        let safe_name: String = sync_name
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        self.dir.join(format!("{safe_name}.json"))
    }
}

impl FsStateStore {
    /// Read a sync's state and when it was last saved, without moving a
    /// corrupt file aside like `load`. None if there is none; an unreadable
    /// file is an error.
    pub fn peek(
        &self,
        sync_name: &str,
    ) -> std::result::Result<Option<(SyncState, std::time::SystemTime)>, String> {
        let path = self.path_for(sync_name);
        let content = match std::fs::read_to_string(&path) {
            Ok(content) => content,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.to_string()),
        };
        let state = serde_json::from_str(&content).map_err(|e| e.to_string())?;
        let modified = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .map_err(|e| e.to_string())?;
        Ok(Some((state, modified)))
    }
}

impl StateStore for FsStateStore {
    fn load(&self, sync_name: &str) -> Result<Option<SyncState>> {
        let path = self.path_for(sync_name);
        let error = match std::fs::read_to_string(&path) {
            Ok(content) => match serde_json::from_str(&content) {
                Ok(state) => return Ok(Some(state)),
                Err(e) => std::io::Error::new(std::io::ErrorKind::InvalidData, e),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            // Not UTF-8: as broken as invalid JSON
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => e,
            Err(e) => return Err(Error::State { path, source: e }),
        };
        // Keep the corrupt file for inspection; the sync rebuilds its state
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let aside = path.with_extension(format!("json.corrupt-{secs}"));
        std::fs::rename(&path, &aside).map_err(|_| Error::State {
            path: path.clone(),
            source: error,
        })?;
        warn!(
            sync = sync_name,
            moved_to = %aside.display(),
            "state file corrupt, moved aside; the next cycle is a full sync"
        );
        Ok(None)
    }

    fn save(&self, sync_name: &str, state: &SyncState) -> Result<()> {
        let path = self.path_for(sync_name);
        let safe_name = path.file_stem().unwrap_or_default().to_string_lossy();
        let tmp_path = self.dir.join(format!(".{safe_name}.tmp"));
        let content = serde_json::to_string_pretty(state).map_err(|e| Error::State {
            path: path.clone(),
            source: std::io::Error::other(e),
        })?;
        // Atomic write: write to temp, then rename
        std::fs::write(&tmp_path, content.as_bytes()).map_err(|e| Error::State {
            path: tmp_path.clone(),
            source: e,
        })?;
        std::fs::rename(&tmp_path, &path).map_err(|e| Error::State {
            path: path.clone(),
            source: e,
        })?;
        Ok(())
    }

    fn remove(&self, sync_name: &str) -> Result<()> {
        let path = self.path_for(sync_name);
        match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(Error::State { path, source: e })
            }
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::mapping::SinkRecord;

    fn test_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("capycal-{}-{}", std::process::id(), name))
    }

    #[test]
    fn test_store_roundtrip() {
        let dir = test_dir("store");
        let _ = std::fs::remove_dir_all(&dir);
        let store = FsStateStore::new(dir.clone()).unwrap();

        let mut state = SyncState {
            sync_token: Some("token-123".to_string()),
            ..Default::default()
        };
        state.uid_mapping.insert(
            "src-uid-1".to_string(),
            SinkRecord {
                sink_uid: "sink-uid-1".to_string(),
                sink_href: "/cal/sink-uid-1.ics".to_string(),
                source_href: "/cal/src-uid-1.ics".to_string(),
                source_etag: "\"etag1\"".to_string(),
            },
        );

        store.save("test-sync", &state).unwrap();

        let loaded = store.load("test-sync").unwrap().unwrap();
        assert_eq!(loaded.sync_token, Some("token-123".to_string()));
        assert!(loaded.uid_mapping.get("src-uid-1").is_some());
        assert_eq!(
            loaded.uid_mapping.get("src-uid-1").unwrap().sink_uid,
            "sink-uid-1"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_load_missing_returns_default() {
        let dir = test_dir("missing");
        let _ = std::fs::remove_dir_all(&dir);
        let store = FsStateStore::new(dir.clone()).unwrap();

        assert!(store.load("nonexistent").unwrap().is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_remove_deletes_state_and_tolerates_missing() {
        let dir = test_dir("remove");
        let _ = std::fs::remove_dir_all(&dir);
        let store = FsStateStore::new(dir.clone()).unwrap();

        let state = SyncState {
            sync_token: Some("token".to_string()),
            ..Default::default()
        };
        store.save("to-remove", &state).unwrap();
        store.save("to-keep", &state).unwrap();

        store.remove("to-remove").unwrap();
        assert!(!store.path_for("to-remove").exists());
        assert!(store.load("to-remove").unwrap().is_none());
        assert!(store.load("to-keep").unwrap().is_some());

        store.remove("to-remove").unwrap();

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_new_fails_when_dir_is_a_file() {
        let file = test_dir("new-file");
        let _ = std::fs::remove_dir_all(&file);
        std::fs::write(&file, "").unwrap();

        let err = FsStateStore::new(file.join("state")).err().unwrap();
        assert!(matches!(err, Error::State { .. }), "{err}");

        let _ = std::fs::remove_file(&file);
    }

    #[test]
    fn test_load_moves_corrupt_state_aside() {
        let dir = test_dir("corrupt");
        let _ = std::fs::remove_dir_all(&dir);
        let store = FsStateStore::new(dir.clone()).unwrap();
        std::fs::write(dir.join("broken.json"), "{not json").unwrap();

        assert!(store.load("broken").unwrap().is_none());
        assert!(!dir.join("broken.json").exists());
        let names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 1);
        assert!(names[0].starts_with("broken.json.corrupt-"), "{names:?}");
        assert_eq!(
            std::fs::read_to_string(dir.join(&names[0])).unwrap(),
            "{not json"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_io_errors_are_reported_with_path() {
        let dir = test_dir("io-errors");
        let _ = std::fs::remove_dir_all(&dir);
        let store = FsStateStore::new(dir.clone()).unwrap();
        // A directory where a state file is expected makes read, remove and rename fail
        std::fs::create_dir(dir.join("blocked.json")).unwrap();

        for result in [
            store.load("blocked").map(|_| ()),
            store.save("blocked", &SyncState::default()),
            store.remove("blocked"),
        ] {
            let Err(Error::State { path, .. }) = result else {
                panic!("expected state error");
            };
            assert_eq!(path, dir.join("blocked.json"));
        }

        std::fs::remove_dir_all(&dir).unwrap();
        let err = store.save("gone", &SyncState::default()).unwrap_err();
        assert!(matches!(err, Error::State { path, .. } if path == dir.join(".gone.tmp")));
    }

    #[test]
    fn test_path_for_sanitizes_special_chars() {
        let dir = test_dir("sanitize");
        let _ = std::fs::remove_dir_all(&dir);
        let store = FsStateStore::new(dir.clone()).unwrap();

        // Slashes are replaced with underscore, dots are allowed (but harmless after slash removal)
        let path = store.path_for("../../etc/passwd");
        let filename = path.file_name().unwrap().to_str().unwrap();
        assert!(!filename.contains('/'));
        assert!(filename.ends_with(".json"));
        // The path should stay within the state dir
        assert!(path.starts_with(&dir));

        // Normal names preserved
        let path = store.path_for("work-to-personal");
        let filename = path.file_name().unwrap().to_str().unwrap();
        assert_eq!(filename, "work-to-personal.json");

        // With dots in name (not leading)
        let path = store.path_for("sync.v2");
        let filename = path.file_name().unwrap().to_str().unwrap();
        assert_eq!(filename, "sync.v2.json");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_save_and_load_preserves_all_fields() {
        let dir = test_dir("fields");
        let _ = std::fs::remove_dir_all(&dir);
        let store = FsStateStore::new(dir.clone()).unwrap();

        let mut state = SyncState {
            sync_token: Some("token-abc".to_string()),
            ctag: Some("ctag-xyz".to_string()),
            pending_removals: vec!["/cal/gone.ics".to_string()],
            last_full_sync: chrono::DateTime::from_timestamp(1767268800, 0),
            ..Default::default()
        };
        state.uid_mapping.insert(
            "src-1".to_string(),
            SinkRecord {
                sink_uid: "sink-1".to_string(),
                sink_href: "/cal/sink-1.ics".to_string(),
                source_href: "/cal/src-1.ics".to_string(),
                source_etag: "\"e1\"".to_string(),
            },
        );
        state.uid_mapping.insert(
            "src-2".to_string(),
            SinkRecord {
                sink_uid: "sink-2".to_string(),
                sink_href: "/cal/sink-2.ics".to_string(),
                source_href: "/cal/src-2.ics".to_string(),
                source_etag: "\"e2\"".to_string(),
            },
        );

        store.save("full-test", &state).unwrap();
        let loaded = store.load("full-test").unwrap().unwrap();

        assert_eq!(loaded.sync_token, Some("token-abc".to_string()));
        assert_eq!(loaded.ctag, Some("ctag-xyz".to_string()));
        assert_eq!(loaded.pending_removals, vec!["/cal/gone.ics".to_string()]);
        assert_eq!(loaded.last_full_sync, state.last_full_sync);
        assert!(loaded.uid_mapping.get("src-1").is_some());
        assert!(loaded.uid_mapping.get("src-2").is_some());
        assert_eq!(
            loaded.uid_mapping.get("src-1").unwrap().source_href,
            "/cal/src-1.ics"
        );
        assert_eq!(
            loaded.uid_mapping.get("src-2").unwrap().source_etag,
            "\"e2\""
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_load_state_without_pending_removals() {
        let dir = test_dir("legacy");
        let _ = std::fs::remove_dir_all(&dir);
        let store = FsStateStore::new(dir.clone()).unwrap();
        std::fs::write(
            dir.join("old.json"),
            r#"{"sync_token":"t1","uid_mapping":{"entries":{}}}"#,
        )
        .unwrap();

        let state = store.load("old").unwrap().unwrap();
        assert_eq!(state.sync_token.as_deref(), Some("t1"));
        assert!(state.pending_removals.is_empty());
        assert!(state.last_full_sync.is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
