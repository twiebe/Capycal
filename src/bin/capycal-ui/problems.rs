//! The "Problems" window as plain text: every current problem in full,
//! followed by the recent warnings and errors of the log.

use capycal::{LogLine, SyncStatus};
use tracing::Level;

use crate::backend::{Problem, Snapshot};
use crate::menu::failed;

/// Warnings and errors from the log shown below the problems.
const RECENT_LOG_LINES: usize = 30;

/// Whether `snapshot` has something the Problems window explains. A missing
/// config is a state, not a problem.
pub fn has_problems(snapshot: &Snapshot) -> bool {
    snapshot
        .problem
        .as_ref()
        .is_some_and(|p| *p != Problem::NoConfig)
        || snapshot.syncs.iter().any(|s| !s.paused && failed(s))
}

/// Nothing to show: no problem and no recent warnings or errors. The
/// windows then show an empty state instead of text.
pub fn nothing_to_show(snapshot: &Snapshot, logs: &[LogLine]) -> bool {
    !has_problems(snapshot) && !logs.iter().any(|l| l.level <= Level::WARN)
}

/// Header line of the window.
pub fn header(snapshot: &Snapshot) -> &'static str {
    if has_problems(snapshot) {
        "Something needs attention"
    } else {
        "No problems"
    }
}

/// The window text for `snapshot`, with the warnings and errors of `logs`.
pub fn text(snapshot: &Snapshot, logs: &[LogLine]) -> String {
    let mut sections: Vec<String> = Vec::new();
    match &snapshot.problem {
        None | Some(Problem::NoConfig) => {}
        Some(problem) => sections.push(problem_detail(problem)),
    }
    sections.extend(
        snapshot
            .syncs
            .iter()
            .filter(|s| !s.paused && failed(s))
            .map(sync_detail),
    );
    if sections.is_empty() {
        sections.push("Everything runs.".to_string());
    }
    let recent: Vec<&str> = logs
        .iter()
        .filter(|l| l.level <= Level::WARN)
        .map(|l| l.text.as_str())
        .collect();
    if !recent.is_empty() {
        let start = recent.len().saturating_sub(RECENT_LOG_LINES);
        sections.push(format!(
            "Recent warnings and errors (Show Log for everything):\n\n{}",
            recent[start..].join("\n")
        ));
    }
    let mut text = sections.join("\n\n\n");
    text.push('\n');
    text
}

fn problem_detail(problem: &Problem) -> String {
    match problem {
        Problem::NoConfig => String::new(),
        Problem::Config(e) => format!(
            "The config could not be loaded. Running syncs continue with the previous \
             config.\nFix the config and save it; the app reloads it by itself.\n\n{e}"
        ),
        Problem::Locked { pid } => format!(
            "Another Capycal process (pid {pid}) runs this config. Quit it, then choose \
             Reload Config."
        ),
        Problem::Purge(e) => format!("Removing synced events failed:\n\n{e}"),
    }
}

/// What to do about the common sync errors.
fn hint(error: &str) -> Option<&'static str> {
    let error = error.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| error.contains(w));
    if has(&["401", "unauthorized"]) {
        Some("Check the username and password; some providers need an app-specific password.")
    } else if has(&["403", "forbidden"]) {
        Some("The account may not access this calendar; check the calendar URL and its sharing.")
    } else if has(&["404", "not found"]) {
        Some("Check the calendar URL; Discover lists the calendars of an account.")
    } else if has(&["dns", "connect", "timed out", "timeout", "unreachable"]) {
        Some("Check the server address and the network connection.")
    } else if has(&["credential"]) {
        Some("Check the password setting of the calendar (env, file or command).")
    } else {
        None
    }
}

fn sync_detail(status: &SyncStatus) -> String {
    match &status.last_result {
        Some(Err(e)) => {
            let hint = hint(e).map(|h| format!("\n\n{h}")).unwrap_or_default();
            format!(
                "Sync '{}' failed. It is retried after its interval.{hint}\n\n{e}",
                status.name
            )
        }
        Some(Ok(stats)) => format!(
            "Sync '{}' could not write or delete {} event(s) in its last run. They are \
             retried in the next run; the log below names them.",
            status.name, stats.errors
        ),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use capycal::CycleStats;
    use chrono::Utc;

    fn snapshot(problem: Option<Problem>, syncs: Vec<SyncStatus>) -> Snapshot {
        Snapshot {
            loaded: true,
            problem,
            activity: None,
            syncs,
        }
    }

    fn line(level: Level, text: &str) -> LogLine {
        LogLine {
            timestamp: Utc::now(),
            level,
            text: text.to_string(),
        }
    }

    fn with_result(name: &str, result: Result<CycleStats, String>) -> SyncStatus {
        SyncStatus {
            last_result: Some(result),
            last_run: Some(Utc::now()),
            ..SyncStatus::new(name)
        }
    }

    #[test]
    fn test_no_problems() {
        let ok = snapshot(None, vec![with_result("a", Ok(CycleStats::default()))]);
        assert!(!has_problems(&ok));
        assert!(!has_problems(&snapshot(Some(Problem::NoConfig), vec![])));
        assert_eq!(header(&ok), "No problems");
        assert_eq!(
            text(&ok, &[line(Level::INFO, "synced")]),
            "Everything runs.\n"
        );
    }

    #[test]
    fn test_problems_in_full_with_recent_warnings() {
        let error = "failed to parse capycal.toml: TOML parse error at line 3, column 1\n  |\n3 | bogus = 1\n  | ^^^^^";
        let partly = CycleStats {
            errors: 2,
            ..CycleStats::default()
        };
        let paused = SyncStatus {
            paused: true,
            ..with_result("paused", Err("old".to_string()))
        };
        let snapshot = snapshot(
            Some(Problem::Config(error.to_string())),
            vec![
                with_result("a", Err("connection refused".to_string())),
                with_result("b", Ok(partly)),
                paused,
            ],
        );
        assert!(has_problems(&snapshot));
        let logs: Vec<LogLine> = (0..40)
            .map(|i| line(Level::WARN, &format!("warning {i}")))
            .chain([
                line(Level::INFO, "info line"),
                line(Level::ERROR, "put failed"),
            ])
            .collect();
        let text = text(&snapshot, &logs);
        assert!(text.contains(error), "{text}");
        assert!(text.contains("Sync 'a' failed"), "{text}");
        assert!(text.contains("connection refused"), "{text}");
        assert!(
            text.contains("could not write or delete 2 event(s)"),
            "{text}"
        );
        assert!(!text.contains("'paused'"), "{text}");
        assert!(
            text.contains("put failed") && !text.contains("info line"),
            "{text}"
        );
        // Only the most recent warnings
        assert!(
            text.contains("warning 39") && !text.contains("warning 10\n"),
            "{text}"
        );
    }

    #[test]
    fn test_hints_for_common_errors() {
        assert!(
            hint("HTTP status 401 Unauthorized")
                .unwrap()
                .contains("password")
        );
        assert!(
            hint("error sending request: dns error")
                .unwrap()
                .contains("network")
        );
        assert!(
            hint("PROPFIND (list) failed with status 404 Not Found")
                .unwrap()
                .contains("Discover")
        );
        assert_eq!(hint("something else"), None);
    }

    #[test]
    fn test_nothing_to_show_needs_no_problem_and_no_warnings() {
        let ok = snapshot(None, vec![]);
        assert!(nothing_to_show(&ok, &[]));
        let broken = snapshot(Some(Problem::Config("bad".into())), vec![]);
        assert!(!nothing_to_show(&broken, &[]));
        let warning = capycal::LogLine {
            timestamp: Utc::now(),
            level: Level::WARN,
            text: "careful".into(),
        };
        assert!(!nothing_to_show(&ok, &[warning]));
    }
}
