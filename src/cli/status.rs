use std::path::Path;
use std::time::SystemTime;

use chrono::{DateTime, Utc};

use crate::config::Config;
use crate::ical::transform::local_tz;
use crate::state::SyncState;
use crate::state::lock::lock_holder;
use crate::state::store::FsStateStore;

use super::{CliError, CliResult};

/// A sync and what its state file says.
struct Row {
    name: String,
    source: String,
    target: String,
    enabled: bool,
    state: Result<Option<(SyncState, SystemTime)>, String>,
}

/// Print whether a process runs the config and, per sync, what its state
/// file says. Reads only; takes no lock and contacts no server.
pub fn status(config: &Config, config_path: &Path) -> CliResult {
    let state_dir = config.resolve_state_dir().map_err(CliError::msg)?;
    let holder = lock_holder(config, config_path)
        .map_err(CliError::msg)?
        .map(|h| h.pid);
    let store = FsStateStore::new(state_dir.clone()).map_err(CliError::msg)?;
    let rows: Vec<Row> = config
        .syncs
        .iter()
        .map(|s| Row {
            name: s.name.clone(),
            source: s.source.clone(),
            target: s.target.clone(),
            enabled: s.enabled,
            state: store.peek(&s.name),
        })
        .collect();
    let local = |time: DateTime<Utc>| {
        time.with_timezone(&local_tz())
            .format("%Y-%m-%d %H:%M")
            .to_string()
    };
    print!(
        "{}",
        render(config_path, &state_dir, holder.as_deref(), &rows, local)
    );
    Ok(())
}

fn render(
    config_path: &Path,
    state_dir: &Path,
    holder: Option<&str>,
    rows: &[Row],
    time: impl Fn(DateTime<Utc>) -> String,
) -> String {
    let running = match holder {
        Some(pid) => format!("running (pid {pid})"),
        None => "not running".to_string(),
    };
    let mut out = format!(
        "config:  {}\nstate:   {}\ndaemon:  {running}\n",
        config_path.display(),
        state_dir.display()
    );
    if rows.is_empty() {
        out.push_str("\nno syncs configured\n");
        return out;
    }

    let header = [
        "SYNC",
        "FROM -> TO",
        "EVENTS",
        "LAST CHANGE",
        "LAST FULL SYNC",
        "",
    ];
    let mut table = vec![header.map(str::to_string)];
    for row in rows {
        let route = format!("{} -> {}", row.source, row.target);
        let cells = match &row.state {
            Ok(Some((state, saved))) => {
                let full = match state.last_full_sync {
                    Some(t) if t == DateTime::UNIX_EPOCH => "never".to_string(),
                    Some(t) => time(t),
                    None => "-".to_string(),
                };
                let mut notes = Vec::new();
                if state.full_sync_pending {
                    notes.push("full sync pending".to_string());
                }
                if state.removals_deferred {
                    notes.push("removals wait for the next cycle".to_string());
                }
                match state.pending_removals.len() {
                    0 => {}
                    1 => notes.push("1 delete to retry".to_string()),
                    n => notes.push(format!("{n} deletes to retry")),
                }
                [
                    state.uid_mapping.len().to_string(),
                    time(DateTime::<Utc>::from(*saved)),
                    full,
                    notes.join(", "),
                ]
            }
            Ok(None) => [
                "-".to_string(),
                "-".to_string(),
                "-".to_string(),
                "no state yet, starts with a full sync".to_string(),
            ],
            Err(e) => [
                "-".to_string(),
                "-".to_string(),
                "-".to_string(),
                format!("state unreadable: {e}"),
            ],
        };
        let [events, change, full, mut notes] = cells;
        if !row.enabled {
            notes = match notes.as_str() {
                "" => "disabled".to_string(),
                _ => format!("disabled, {notes}"),
            };
        }
        table.push([row.name.clone(), route, events, change, full, notes]);
    }

    let widths: Vec<usize> = (0..header.len())
        .map(|col| {
            table
                .iter()
                .map(|r| r[col].chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    out.push('\n');
    for row in &table {
        let line: Vec<String> = row
            .iter()
            .zip(&widths)
            .enumerate()
            .map(|(col, (cell, width))| {
                // Numbers align right
                if col == 2 {
                    format!("{cell:>width$}")
                } else {
                    format!("{cell:<width$}")
                }
            })
            .collect();
        out.push_str(line.join("  ").trim_end());
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::mapping::SinkRecord;
    use std::time::Duration;

    fn utc(time: DateTime<Utc>) -> String {
        time.format("%Y-%m-%d %H:%M").to_string()
    }

    #[test]
    fn test_render_shows_each_sync_and_the_daemon() {
        let mut state = SyncState::default();
        for uid in ["a", "b", "c"] {
            state.uid_mapping.insert(
                uid.to_string(),
                SinkRecord {
                    sink_uid: format!("capycal-{uid}"),
                    sink_href: format!("/home/{uid}.ics"),
                    source_href: format!("/work/{uid}.ics"),
                    source_etag: "e".to_string(),
                },
            );
        }
        state.last_full_sync = DateTime::from_timestamp(1_790_000_000, 0);
        state.pending_removals = vec!["/work/x.ics".to_string()];
        let saved = SystemTime::UNIX_EPOCH + Duration::from_secs(1_790_100_000);
        let rows = [
            Row {
                name: "work-to-home".to_string(),
                source: "work".to_string(),
                target: "home".to_string(),
                enabled: true,
                state: Ok(Some((state, saved))),
            },
            Row {
                name: "new".to_string(),
                source: "home".to_string(),
                target: "work".to_string(),
                enabled: true,
                state: Ok(None),
            },
            Row {
                name: "off".to_string(),
                source: "home".to_string(),
                target: "work".to_string(),
                enabled: false,
                state: Ok(None),
            },
            Row {
                name: "broken".to_string(),
                source: "home".to_string(),
                target: "work".to_string(),
                enabled: true,
                state: Err("expected value at line 1".to_string()),
            },
        ];
        let out = render(
            Path::new("/etc/capycal/capycal.toml"),
            Path::new("/var/lib/capycal"),
            Some("4242"),
            &rows,
            utc,
        );
        let expected = "\
config:  /etc/capycal/capycal.toml
state:   /var/lib/capycal
daemon:  running (pid 4242)

SYNC          FROM -> TO    EVENTS  LAST CHANGE       LAST FULL SYNC
work-to-home  work -> home       3  2026-09-22 18:00  2026-09-21 14:13  1 delete to retry
new           home -> work       -  -                 -                 no state yet, starts with a full sync
off           home -> work       -  -                 -                 disabled, no state yet, starts with a full sync
broken        home -> work       -  -                 -                 state unreadable: expected value at line 1
";
        assert_eq!(out, expected);
    }

    #[test]
    fn test_render_without_syncs() {
        let out = render(Path::new("c.toml"), Path::new("s"), None, &[], utc);
        assert!(out.contains("daemon:  not running"));
        assert!(out.ends_with("no syncs configured\n"));
    }
}
