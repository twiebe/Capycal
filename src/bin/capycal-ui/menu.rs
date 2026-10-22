//! The tray menu as plain data, built from the backend snapshot.

use chrono::{DateTime, Utc};

use crate::backend::{Problem, Snapshot};
use capycal::SyncStatus;

/// Longest error text shown in a status line.
const MAX_ERROR_CHARS: usize = 60;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    SyncNow,
    Settings,
    ShowProblems,
    About,
    Quit,
}

const ACTIONS: [Action; 5] = [
    Action::SyncNow,
    Action::Settings,
    Action::ShowProblems,
    Action::About,
    Action::Quit,
];

impl Action {
    /// Menu item id, stable across menu rebuilds.
    pub fn id(&self) -> String {
        match self {
            Action::SyncNow => "sync-now",
            Action::Settings => "settings",
            Action::ShowProblems => "show-problems",
            Action::About => "about",
            Action::Quit => "quit",
        }
        .to_string()
    }

    pub fn from_id(id: &str) -> Option<Self> {
        ACTIONS.into_iter().find(|a| a.id() == id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    /// A disabled line of text.
    Status(String),
    Separator,
    Action {
        action: Action,
        label: String,
        enabled: bool,
    },
}

const NOT_CONFIGURED: &str = "Not set up yet: open Settings\u{2026}";

/// The tray menu for `snapshot` at time `now`.
pub fn build(snapshot: &Snapshot, now: DateTime<Utc>) -> Vec<Item> {
    let mut items = Vec::new();
    if !snapshot.loaded {
        items.push(Item::Status("Starting".to_string()));
    }
    if let Some(activity) = &snapshot.activity {
        items.push(Item::Status(activity.clone()));
    }
    // Problem lines open the Problems window
    let line = |text: String, problem: bool| {
        if problem {
            Item::Action {
                action: Action::ShowProblems,
                label: text,
                enabled: true,
            }
        } else {
            Item::Status(text)
        }
    };
    if let Some(problem) = &snapshot.problem {
        items.push(line(problem_text(problem), *problem != Problem::NoConfig));
    } else if snapshot.loaded && snapshot.syncs.is_empty() {
        items.push(Item::Status(NOT_CONFIGURED.to_string()));
    }
    items.extend(
        snapshot
            .syncs
            .iter()
            .map(|s| line(status_line(s, now), !s.paused && failed(s))),
    );
    // Syncs are not started while another operation runs
    let running =
        snapshot.activity.is_none() && snapshot.syncs.iter().any(|s| !s.paused && !s.disabled);
    let action = |action, label: &str, enabled| Item::Action {
        action,
        label: label.to_string(),
        enabled,
    };
    // Everything else, repairs included, is in Settings; a problem line
    // above opens the Problems window
    items.extend([
        Item::Separator,
        action(Action::SyncNow, "Sync Now", running),
        action(Action::Settings, "Settings\u{2026}", true),
        Item::Separator,
        action(Action::About, "About Capycal", true),
        action(Action::Quit, "Quit Capycal", true),
    ]);
    items
}

fn problem_text(problem: &Problem) -> String {
    match problem {
        Problem::NoConfig => "No config: open Settings\u{2026} to set one up".to_string(),
        Problem::Config(e) => format!("Config error: {}", shorten(e)),
        Problem::Locked { pid } => format!("Another Capycal runs this config (pid {pid})"),
        Problem::Purge(e) => format!("Removing events failed: {}", shorten(e)),
    }
}

/// "<name>: synced 2 min ago", "<name>: syncing", "<name>: full sync, 5 of
/// 20" or "<name>: error: ...".
pub fn status_line(status: &SyncStatus, now: DateTime<Utc>) -> String {
    let state = if status.disabled {
        "disabled".to_string()
    } else if status.paused {
        "paused - events removed".to_string()
    } else if let (true, Some(progress)) = (status.running, status.full_sync) {
        match progress.total {
            Some(total) => format!("full sync, {} of {total}", progress.done.min(total)),
            None => "full sync, reading".to_string(),
        }
    } else if status.running {
        "syncing".to_string()
    } else {
        match (&status.last_result, status.last_run) {
            (Some(Err(e)), _) => format!("error: {}", shorten(e)),
            (Some(Ok(stats)), Some(run)) if stats.errors > 0 => format!(
                "synced {}, {} failed",
                relative_time(run, now),
                stats.errors
            ),
            (Some(Ok(stats)), Some(run)) if stats.deferred > 0 => format!(
                "synced {}, {} to remove next cycle",
                relative_time(run, now),
                stats.deferred
            ),
            (Some(Ok(_)), Some(run)) => format!("synced {}", relative_time(run, now)),
            _ => "waiting".to_string(),
        }
    };
    format!("{}: {state}", status.name)
}

/// "just now", "5 min ago", "3 h ago" or "2 d ago".
pub fn relative_time(then: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let secs = (now - then).num_seconds().max(0);
    match secs {
        0..60 => "just now".to_string(),
        60..3600 => format!("{} min ago", secs / 60),
        3600..86400 => format!("{} h ago", secs / 3600),
        _ => format!("{} d ago", secs / 86400),
    }
}

/// First line of an error, cut to `MAX_ERROR_CHARS`.
fn shorten(error: &str) -> String {
    let line = error.lines().next().unwrap_or_default();
    match line.char_indices().nth(MAX_ERROR_CHARS) {
        Some((cut, _)) => format!("{}…", &line[..cut]),
        None => line.to_string(),
    }
}

/// The state the menu bar icon shows, most urgent first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Error,
    /// Starting, not configured or all syncs paused or disabled: shown dimmed.
    Inactive,
    Syncing,
    Ok,
}

/// The menu bar icon and its tooltip for `snapshot`.
pub fn tray_status(snapshot: &Snapshot, now: DateTime<Utc>) -> (Icon, String) {
    let tooltip = |text: &str| format!("Capycal - {text}");
    match &snapshot.problem {
        Some(Problem::NoConfig) => return (Icon::Inactive, tooltip("not configured")),
        Some(problem) => return (Icon::Error, tooltip(&problem_text(problem))),
        None => {}
    }
    let active = || snapshot.syncs.iter().filter(|s| !s.paused && !s.disabled);
    if let Some(failed) = active().find(|s| failed(s)) {
        return (Icon::Error, tooltip(&status_line(failed, now)));
    }
    if !snapshot.loaded {
        return (Icon::Inactive, tooltip("starting"));
    }
    if snapshot.syncs.is_empty() {
        return (Icon::Inactive, tooltip("not configured"));
    }
    if let Some(activity) = &snapshot.activity {
        return (Icon::Syncing, tooltip(activity));
    }
    if active().any(|s| s.running) {
        let names: Vec<&str> = active()
            .filter(|s| s.running)
            .map(|s| s.name.as_str())
            .collect();
        return (
            Icon::Syncing,
            tooltip(&format!("syncing {}", names.join(", "))),
        );
    }
    if active().next().is_none() {
        let state = if snapshot.syncs.iter().any(|s| s.paused) {
            "paused"
        } else {
            "disabled"
        };
        return (Icon::Inactive, tooltip(state));
    }
    let text = match active().filter_map(|s| s.last_run).max() {
        Some(run) => format!("synced {}", relative_time(run, now)),
        None => "waiting".to_string(),
    };
    (Icon::Ok, tooltip(&text))
}

/// Whether the last cycle of `status` failed or had failed events.
pub fn failed(status: &SyncStatus) -> bool {
    match &status.last_result {
        Some(Err(_)) => true,
        Some(Ok(stats)) => stats.errors > 0,
        None => false,
    }
}

/// Whether `b` differs from `a` only in texts, enabled and checked states,
/// so the shown menu can be updated in place. Replacing the menu closes it
/// while it is open.
pub fn same_shape(a: &[Item], b: &[Item]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|pair| match pair {
            (Item::Status(_), Item::Status(_)) | (Item::Separator, Item::Separator) => true,
            (Item::Action { action: x, .. }, Item::Action { action: y, .. }) => x == y,
            _ => false,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use capycal::CycleStats;
    use chrono::TimeDelta;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-12T12:00:00Z")
            .unwrap()
            .to_utc()
    }

    fn status(running: bool, result: Option<Result<CycleStats, String>>) -> SyncStatus {
        SyncStatus {
            running,
            last_run: result.as_ref().map(|_| now() - TimeDelta::minutes(2)),
            last_result: result,
            ..SyncStatus::new("work")
        }
    }

    #[test]
    fn test_relative_time() {
        let ago = |secs| relative_time(now() - TimeDelta::seconds(secs), now());
        assert_eq!(ago(-5), "just now");
        assert_eq!(ago(59), "just now");
        assert_eq!(ago(60), "1 min ago");
        assert_eq!(ago(3599), "59 min ago");
        assert_eq!(ago(7200), "2 h ago");
        assert_eq!(ago(3 * 86400 + 5), "3 d ago");
    }

    #[test]
    fn test_status_line() {
        let line = |s: &SyncStatus| status_line(s, now());
        assert_eq!(line(&status(false, None)), "work: waiting");
        assert_eq!(
            line(&status(true, Some(Ok(CycleStats::default())))),
            "work: syncing"
        );
        assert_eq!(
            line(&status(false, Some(Ok(CycleStats::default())))),
            "work: synced 2 min ago"
        );
        let stats = CycleStats {
            errors: 3,
            ..Default::default()
        };
        assert_eq!(
            line(&status(false, Some(Ok(stats)))),
            "work: synced 2 min ago, 3 failed"
        );
        let long = format!("{}\nsecond line", "x".repeat(80));
        assert_eq!(
            line(&status(false, Some(Err(long)))),
            format!("work: error: {}…", "x".repeat(60))
        );
        let full = |done, total| SyncStatus {
            full_sync: Some(capycal::FullSyncProgress { done, total }),
            ..status(true, None)
        };
        let waiting = CycleStats {
            deferred: 6,
            ..Default::default()
        };
        assert_eq!(
            line(&status(false, Some(Ok(waiting)))),
            "work: synced 2 min ago, 6 to remove next cycle"
        );
        let disabled = SyncStatus {
            disabled: true,
            ..status(false, None)
        };
        assert_eq!(line(&disabled), "work: disabled");
        assert_eq!(line(&full(0, None)), "work: full sync, reading");
        assert_eq!(line(&full(5, Some(20))), "work: full sync, 5 of 20");
    }

    #[test]
    fn test_shorten_respects_char_boundaries() {
        assert_eq!(shorten(&"ä".repeat(61)), format!("{}…", "ä".repeat(60)));
        assert_eq!(shorten("short"), "short");
    }

    fn statuses(items: &[Item]) -> Vec<&str> {
        items
            .iter()
            .filter_map(|i| match i {
                Item::Status(s) => Some(s.as_str()),
                _ => None,
            })
            .collect()
    }

    /// Status lines that open the Problems window.
    fn problem_lines(items: &[Item]) -> Vec<&str> {
        items
            .iter()
            .filter_map(|i| match i {
                Item::Action {
                    action: Action::ShowProblems,
                    label,
                    ..
                } => Some(label.as_str()),
                _ => None,
            })
            .collect()
    }

    /// Whether `wanted` is an enabled item.
    fn enabled(items: &[Item], wanted: Action) -> bool {
        items.iter().any(|i| match i {
            Item::Action {
                action, enabled, ..
            } => *enabled && *action == wanted,
            _ => false,
        })
    }

    fn snapshot(problem: Option<Problem>, syncs: Vec<SyncStatus>) -> Snapshot {
        Snapshot {
            loaded: true,
            problem,
            activity: None,
            syncs,
        }
    }

    #[test]
    fn test_menu_for_running_syncs() {
        let snapshot = snapshot(None, vec![status(true, None), SyncStatus::new("home")]);
        let items = build(&snapshot, now());
        assert_eq!(statuses(&items), ["work: syncing", "home: waiting"]);
        assert!(enabled(&items, Action::SyncNow));
        assert!(enabled(&items, Action::About));
        assert_eq!(
            items.last(),
            Some(&Item::Action {
                action: Action::Quit,
                label: "Quit Capycal".to_string(),
                enabled: true
            })
        );
    }

    #[test]
    fn test_top_level() {
        let items = build(&snapshot(None, vec![SyncStatus::new("work")]), now());
        let top: Vec<&str> = items
            .iter()
            .filter_map(|i| match i {
                Item::Action { label, .. } => Some(label.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            top,
            [
                "Sync Now",
                "Settings\u{2026}",
                "About Capycal",
                "Quit Capycal"
            ]
        );
    }

    #[test]
    fn test_menu_for_paused_syncs() {
        let paused = SyncStatus {
            paused: true,
            ..SyncStatus::new("work")
        };
        let items = build(&snapshot(None, vec![paused.clone()]), now());
        assert_eq!(statuses(&items), ["work: paused - events removed"]);
        assert!(!enabled(&items, Action::SyncNow));

        let items = build(
            &snapshot(None, vec![paused, SyncStatus::new("home")]),
            now(),
        );
        assert!(enabled(&items, Action::SyncNow));
    }

    #[test]
    fn test_disabled_syncs_are_not_active() {
        let disabled = SyncStatus {
            disabled: true,
            ..SyncStatus::new("work")
        };
        let items = build(&snapshot(None, vec![disabled.clone()]), now());
        assert_eq!(statuses(&items), ["work: disabled"]);
        assert!(!enabled(&items, Action::SyncNow));
        assert_eq!(
            tray_status(&snapshot(None, vec![disabled]), now()),
            (Icon::Inactive, "Capycal - disabled".to_string())
        );
    }

    #[test]
    fn test_menu_while_busy() {
        let busy = Snapshot {
            activity: Some("Counting synced events".to_string()),
            ..snapshot(
                None,
                vec![
                    SyncStatus::new("home"),
                    SyncStatus {
                        paused: true,
                        ..SyncStatus::new("work")
                    },
                ],
            )
        };
        let items = build(&busy, now());
        assert_eq!(statuses(&items)[0], "Counting synced events");
        assert!(!enabled(&items, Action::SyncNow));
        assert!(enabled(&items, Action::Settings));
    }

    #[test]
    fn test_menu_for_problems() {
        let menu = |loaded, problem| {
            let snapshot = Snapshot {
                loaded,
                ..snapshot(problem, vec![])
            };
            build(&snapshot, now())
        };
        let items = menu(false, None);
        assert_eq!(statuses(&items), ["Starting"]);
        assert!(!enabled(&items, Action::SyncNow));
        assert!(enabled(&items, Action::Settings));

        let items = menu(true, None);
        assert_eq!(statuses(&items), ["Not set up yet: open Settings\u{2026}"]);
        assert!(!enabled(&items, Action::SyncNow));

        let items = menu(true, Some(Problem::NoConfig));
        assert_eq!(
            statuses(&items),
            ["No config: open Settings\u{2026} to set one up"]
        );
        assert!(!enabled(&items, Action::ShowProblems));
        // Real problems open the Problems window
        let items = menu(true, Some(Problem::Locked { pid: "42".into() }));
        assert_eq!(
            problem_lines(&items),
            ["Another Capycal runs this config (pid 42)"]
        );
        assert!(!enabled(&items, Action::SyncNow));
        let items = menu(true, Some(Problem::Config("bad".into())));
        assert_eq!(problem_lines(&items), ["Config error: bad"]);
        let items = menu(true, Some(Problem::Purge("offline".into())));
        assert_eq!(problem_lines(&items), ["Removing events failed: offline"]);

        let failed = status(false, Some(Err("timeout".into())));
        let items = build(
            &snapshot(None, vec![failed, SyncStatus::new("home")]),
            now(),
        );
        assert_eq!(problem_lines(&items), ["work: error: timeout"]);
        assert_eq!(statuses(&items), ["home: waiting"]);
    }

    #[test]
    fn test_action_ids_round_trip() {
        for action in ACTIONS {
            assert_eq!(Action::from_id(&action.id()), Some(action));
        }
        assert_eq!(Action::from_id("unknown"), None);
    }

    #[test]
    fn test_same_shape() {
        let running = build(&snapshot(None, vec![status(true, None)]), now());
        let idle = build(&snapshot(None, vec![SyncStatus::new("work")]), now());
        // Status texts and the checkmark change in place
        assert_ne!(running, idle);
        assert!(same_shape(&running, &idle));

        let two = build(
            &snapshot(None, vec![SyncStatus::new("work"), SyncStatus::new("home")]),
            now(),
        );
        assert!(!same_shape(&idle, &two));
    }

    #[test]
    fn test_tray_status() {
        let tray = |snapshot: &Snapshot| tray_status(snapshot, now());
        let starting = Snapshot {
            loaded: false,
            ..snapshot(None, vec![])
        };
        assert_eq!(tray(&starting).0, Icon::Inactive);
        assert_eq!(
            tray(&snapshot(None, vec![])),
            (Icon::Inactive, "Capycal - not configured".to_string())
        );
        assert_eq!(
            tray(&snapshot(Some(Problem::NoConfig), vec![])).0,
            Icon::Inactive
        );
        assert_eq!(
            tray(&snapshot(Some(Problem::Locked { pid: "7".into() }), vec![])),
            (
                Icon::Error,
                "Capycal - Another Capycal runs this config (pid 7)".to_string()
            )
        );

        let synced = status(false, Some(Ok(CycleStats::default())));
        assert_eq!(
            tray(&snapshot(None, vec![synced.clone()])),
            (Icon::Ok, "Capycal - synced 2 min ago".to_string())
        );
        assert_eq!(
            tray(&snapshot(None, vec![SyncStatus::new("home")])),
            (Icon::Ok, "Capycal - waiting".to_string())
        );

        let running = snapshot(None, vec![status(true, None), synced.clone()]);
        assert_eq!(
            tray(&running),
            (Icon::Syncing, "Capycal - syncing work".to_string())
        );

        // An error wins over syncing, but not when its sync is paused
        let failed = SyncStatus {
            name: "home".into(),
            ..status(false, Some(Err("timeout".into())))
        };
        assert_eq!(
            tray(&snapshot(None, vec![status(true, None), failed.clone()])),
            (Icon::Error, "Capycal - home: error: timeout".to_string())
        );
        let partly = status(
            false,
            Some(Ok(CycleStats {
                errors: 2,
                ..CycleStats::default()
            })),
        );
        assert_eq!(tray(&snapshot(None, vec![partly])).0, Icon::Error);
        let paused = SyncStatus {
            paused: true,
            ..failed
        };
        assert_eq!(
            tray(&snapshot(None, vec![paused])),
            (Icon::Inactive, "Capycal - paused".to_string())
        );
    }

    #[test]
    fn test_settings_is_on_the_top_level() {
        let items = build(&snapshot(None, vec![]), now());
        assert!(items.iter().any(|i| matches!(
            i,
            Item::Action {
                action: Action::Settings,
                ..
            }
        )));
        assert!(enabled(&items, Action::Settings));
        assert_eq!(Action::from_id("settings"), Some(Action::Settings));
    }
}
