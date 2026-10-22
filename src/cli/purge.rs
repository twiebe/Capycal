use crate::config::Config;
use crate::error::Error;
use crate::sync::maintenance;

use super::{CliError, CliResult};

/// Delete the events a sync created (by marker) from its sink and forget its state.
pub async fn purge(config: &Config, sync_name: &str) -> CliResult {
    let Some(sync) = config.syncs.iter().find(|s| s.name == sync_name) else {
        return Err(CliError::Stderr(format!(
            "error: sync '{sync_name}' not found in config\navailable syncs: {}",
            super::sync_names(config)
        )));
    };
    if cfg!(not(target_os = "macos")) && config.calendars[&sync.target].is_eventkit() {
        return Err(CliError::msg(
            "EventKit calendars are only supported on macOS",
        ));
    }

    maintenance::purge_sync(config, sync_name, maintenance::no_progress())
        .await
        .map_err(|e| match e {
            Error::SinkEvents { .. } => CliError::Stderr(e.to_string()),
            e => CliError::msg(e),
        })?;
    Ok(())
}

/// Ask on the terminal before `--purge-all` deletes every event of
/// `cal_name`: the calendar name must be typed. Without a terminal it
/// refuses; `--yes` skips this. An unknown calendar or a feed, which
/// cannot be written, is left to `purge_all`.
pub fn confirm_purge_all(
    config: &Config,
    cal_name: &str,
    mut input: impl std::io::BufRead,
    interactive: bool,
) -> CliResult {
    let Some(cal) = config.calendars.get(cal_name).filter(|c| !c.is_ics()) else {
        return Ok(());
    };
    if !interactive {
        return Err(CliError::Stderr(format!(
            "error: --purge-all deletes every event in '{cal_name}'; \
             add --yes to confirm when not running in a terminal"
        )));
    }
    let location = cal.eventkit_calendar.as_deref().or(cal.url.as_deref());
    eprint!(
        "This deletes EVERY event in calendar '{cal_name}' ({}), including events \
         Capycal did not create.\nType the calendar name to confirm: ",
        location.unwrap_or_default()
    );
    let mut answer = String::new();
    input.read_line(&mut answer).map_err(CliError::msg)?;
    if answer.trim() == cal_name {
        Ok(())
    } else {
        Err(CliError::Stderr("aborted, nothing was deleted".to_string()))
    }
}

/// Delete all events from a calendar and forget the state of every sync writing to it.
pub async fn purge_all(config: &Config, cal_name: &str) -> CliResult {
    let Some(cal) = config.calendars.get(cal_name) else {
        return Err(CliError::Stderr(format!(
            "error: calendar '{cal_name}' not found in config\navailable calendars: {}",
            config
                .calendars
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        )));
    };
    if cfg!(not(target_os = "macos")) && cal.is_eventkit() {
        return Err(CliError::msg(
            "EventKit calendars are only supported on macOS",
        ));
    }
    maintenance::purge_calendar(config, cal_name, maintenance::no_progress())
        .await
        .map_err(|e| match e {
            Error::SinkEvents { .. } => CliError::Stderr(e.to_string()),
            e => CliError::msg(e),
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::tests::{caldav_config, state_dir};
    use crate::sync::maintenance::tests::{event, expect_deletes, mount_home};
    use std::path::Path;
    use wiremock::MockServer;

    #[test]
    fn test_purge_all_needs_the_calendar_name_typed() {
        let cfg = super::super::tests::config();
        let confirm = |answer: &str, interactive| {
            confirm_purge_all(&cfg, "home", answer.as_bytes(), interactive)
        };
        assert!(confirm("home\n", true).is_ok());
        let Err(CliError::Stderr(msg)) = confirm("y\n", true) else {
            panic!("expected an abort");
        };
        assert_eq!(msg, "aborted, nothing was deleted");
        let Err(CliError::Stderr(msg)) = confirm("home\n", false) else {
            panic!("expected a refusal without a terminal");
        };
        assert!(msg.contains("add --yes"), "{msg}");
        // An unknown calendar is reported by purge_all itself
        assert!(confirm_purge_all(&cfg, "nope", "".as_bytes(), false).is_ok());
    }

    #[tokio::test]
    async fn test_purge_all_refuses_a_feed_without_asking() {
        let mut cfg = super::super::tests::config();
        cfg.calendars.insert(
            "feed".to_string(),
            toml::from_str("ics_url = \"https://example.com/basic.ics\"").unwrap(),
        );
        assert!(confirm_purge_all(&cfg, "feed", "".as_bytes(), false).is_ok());
        let err = purge_all(&cfg, "feed").await.unwrap_err();
        assert!(format!("{err:?}").contains("read-only feed"), "{err:?}");
    }

    #[tokio::test]
    async fn test_purge_unknown_sync_lists_sync_names() {
        let cfg = super::super::tests::config();
        let CliError::Stderr(msg) = purge(&cfg, "nope").await.unwrap_err() else {
            panic!("expected stderr error");
        };
        assert_eq!(
            msg,
            "error: sync 'nope' not found in config\navailable syncs: work-to-home, home-to-work"
        );
    }

    fn write_states(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        for name in ["work-to-home", "home-to-work"] {
            std::fs::write(dir.join(format!("{name}.json")), r#"{"uid_mapping":{}}"#).unwrap();
        }
    }

    #[tokio::test]
    async fn test_purge_deletes_only_marked_events_and_forgets_state() {
        let server = MockServer::start().await;
        mount_home(
            &server,
            &["/home/a.ics", "/home/b.ics", "/home/c.ics"],
            &[
                event("/home/a.ics", "X-CAPYCAL-SYNC:work-to-home\n"),
                event("/home/b.ics", ""),
                event("/home/c.ics", "X-CAPYCAL-SYNC:home-to-work\n"),
            ],
        )
        .await;
        expect_deletes(&server, &["/home/a.ics"], 1).await;
        expect_deletes(&server, &["/home/b.ics", "/home/c.ics"], 0).await;
        let dir = state_dir("purge");
        write_states(&dir);

        purge(&caldav_config(&server.uri(), &dir), "work-to-home")
            .await
            .unwrap();
        server.verify().await;
        assert!(!dir.join("work-to-home.json").exists());
        assert!(dir.join("home-to-work.json").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_purge_ignores_syncs_with_longer_names() {
        let server = MockServer::start().await;
        mount_home(
            &server,
            &["/home/old.ics"],
            &[event("/home/old.ics", "X-CAPYCAL-SYNC:work-to-home-old\n")],
        )
        .await;
        expect_deletes(&server, &["/home/old.ics"], 0).await;
        let dir = state_dir("purge-prefix");

        purge(&caldav_config(&server.uri(), &dir), "work-to-home")
            .await
            .unwrap();
        server.verify().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_purge_empty_sink_still_forgets_state() {
        let server = MockServer::start().await;
        mount_home(&server, &[], &[]).await;
        let dir = state_dir("purge-empty");
        write_states(&dir);

        purge(&caldav_config(&server.uri(), &dir), "work-to-home")
            .await
            .unwrap();
        assert!(!dir.join("work-to-home.json").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_purge_reports_listing_error() {
        let server = MockServer::start().await;
        let dir = state_dir("purge-list-error");
        write_states(&dir);

        let CliError::Stderr(msg) = purge(&caldav_config(&server.uri(), &dir), "work-to-home")
            .await
            .unwrap_err()
        else {
            panic!("expected stderr error");
        };
        assert!(msg.starts_with("error listing target events: "), "{msg}");
        assert!(dir.join("work-to-home.json").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(not(target_os = "macos"))]
    #[tokio::test]
    async fn test_purge_eventkit_sink_unsupported_off_macos() {
        let mut cfg = caldav_config("http://localhost", &state_dir("purge-ek"));
        cfg.calendars.get_mut("home").unwrap().eventkit_calendar = Some("Home".to_string());

        let CliError::Stderr(msg) = purge(&cfg, "work-to-home").await.unwrap_err() else {
            panic!("expected stderr error");
        };
        assert_eq!(msg, "error: EventKit calendars are only supported on macOS");
    }

    #[tokio::test]
    async fn test_purge_all_deletes_everything_and_forgets_sink_syncs() {
        let server = MockServer::start().await;
        mount_home(&server, &["/home/a.ics", "/home/b.ics"], &[]).await;
        expect_deletes(&server, &["/home/a.ics", "/home/b.ics"], 1).await;
        let dir = state_dir("purge-all");
        write_states(&dir);

        purge_all(&caldav_config(&server.uri(), &dir), "home")
            .await
            .unwrap();
        server.verify().await;
        assert!(!dir.join("work-to-home.json").exists());
        assert!(dir.join("home-to-work.json").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_purge_all_reports_listing_error() {
        let server = MockServer::start().await;
        let dir = state_dir("purge-all-list-error");
        write_states(&dir);

        let CliError::Stderr(msg) = purge_all(&caldav_config(&server.uri(), &dir), "home")
            .await
            .unwrap_err()
        else {
            panic!("expected stderr error");
        };
        assert!(msg.starts_with("error listing events: "), "{msg}");
        assert!(dir.join("work-to-home.json").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_purge_all_unknown_calendar_lists_calendars() {
        let cfg = super::super::tests::config();
        let CliError::Stderr(msg) = purge_all(&cfg, "nope").await.unwrap_err() else {
            panic!("expected stderr error");
        };
        assert!(
            msg.starts_with("error: calendar 'nope' not found in config\navailable calendars: "),
            "{msg}"
        );
        assert!(msg.contains("work") && msg.contains("home"), "{msg}");
    }

    #[cfg(not(target_os = "macos"))]
    #[tokio::test]
    async fn test_purge_all_eventkit_unsupported_off_macos() {
        let dir = state_dir("purge-all-ek");
        let mut cfg = caldav_config("http://localhost", &dir);
        cfg.calendars.get_mut("home").unwrap().eventkit_calendar = Some("Home".to_string());

        let CliError::Stderr(msg) = purge_all(&cfg, "home").await.unwrap_err() else {
            panic!("expected stderr error");
        };
        assert_eq!(msg, "error: EventKit calendars are only supported on macOS");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
