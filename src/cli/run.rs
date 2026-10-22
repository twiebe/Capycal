use std::path::Path;

use tokio::signal::unix::{SignalKind, signal};
use tracing::{error, info};

use crate::config::Config;
use crate::config_watch::ConfigWatcher;
use crate::logging::LogLevel;
use crate::state::lock::ConfigLock;
use crate::sync::coordinator::Coordinator;

use super::{CliError, CliResult};

/// How the syncs run.
#[derive(Debug, Clone, Copy, Default)]
pub struct Mode {
    /// Run one cycle of each sync and exit.
    pub once: bool,
    /// Make the first cycle of each sync a full sync.
    pub full_sync: bool,
}

/// Run all syncs once, or as a daemon until SIGINT/SIGTERM. SIGHUP and, with
/// `watch_config`, a change of the config file reload the config, keeping
/// the `--only` selection.
pub async fn run(
    config: Config,
    config_path: &Path,
    mode: Mode,
    mut log_level: LogLevel,
    only: Option<&[String]>,
    lock: Option<&mut ConfigLock>,
) -> CliResult {
    info!(
        syncs = config.syncs.len(),
        calendars = config.calendars.len(),
        "Capycal started"
    );

    if mode.once {
        return match Coordinator::run_once(&config, mode.full_sync).await {
            Ok(true) => {
                info!("all syncs done");
                Ok(())
            }
            Ok(false) => Err(CliError::Logged {
                message: "one or more syncs failed".to_string(),
                error: None,
            }),
            Err(e) => Err(CliError::logged("fatal error", e)),
        };
    }

    let signal_err = |e| CliError::logged("failed to install signal handler", e);
    let mut sigterm = signal(SignalKind::terminate()).map_err(signal_err)?;
    let mut sighup = signal(SignalKind::hangup()).map_err(signal_err)?;
    let (_watcher, mut changes) = if config.daemon.watch_config {
        let (watcher, changes) = ConfigWatcher::new(config_path)
            .map_err(|e| CliError::logged("failed to watch the config", e))?;
        info!(config = %config_path.display(), "watching the config for changes");
        (Some(watcher), Some(changes))
    } else {
        (None, None)
    };

    daemon(
        &config,
        config_path,
        mode.full_sync,
        &mut log_level,
        only,
        lock,
        async || {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    info!("received SIGINT");
                    Signal::Stop
                }
                _ = sigterm.recv() => {
                    info!("received SIGTERM");
                    Signal::Stop
                }
                _ = sighup.recv() => {
                    info!("received SIGHUP, reloading config");
                    Signal::Reload
                }
                Some(()) = async {
                    match changes.as_mut() {
                        Some(changes) => changes.recv().await,
                        None => None,
                    }
                } => {
                    info!("config changed, reloading");
                    Signal::Reload
                }
            }
        },
    )
    .await
}

enum Signal {
    Stop,
    Reload,
}

/// Run the syncs until `next_signal` says stop, reloading the config file on
/// request. A reloaded config updates `lock` (its sync names and state
/// directory) before it is applied.
#[allow(clippy::too_many_arguments)]
async fn daemon(
    config: &Config,
    config_path: &Path,
    full_sync: bool,
    log_level: &mut LogLevel,
    only: Option<&[String]>,
    mut lock: Option<&mut ConfigLock>,
    mut next_signal: impl AsyncFnMut() -> Signal,
) -> CliResult {
    let mut coordinator = Coordinator::new();
    coordinator
        .start(config)
        .map_err(|e| CliError::logged("failed to start syncs", e))?;
    if full_sync {
        coordinator.full_sync(None);
    }

    while let Signal::Reload = next_signal().await {
        match Config::load(config_path)
            .map_err(|e| e.to_string())
            .and_then(|mut new_config| {
                if let Some(only) = only {
                    reselect_syncs(&mut new_config, only)?;
                }
                Ok(new_config)
            }) {
            Ok(new_config) if new_config.syncs.is_empty() => {
                error!(
                    "{}, continuing with previous config",
                    super::no_syncs_message(config_path)
                );
            }
            Ok(new_config) => {
                if let Some(lock) = lock.as_deref_mut()
                    && let Err(e) = lock.update(&new_config, config_path)
                {
                    error!(error = %e, "cannot lock the reloaded config, continuing with previous");
                    continue;
                }
                info!(
                    syncs = new_config.syncs.len(),
                    calendars = new_config.calendars.len(),
                    "config reloaded successfully"
                );
                match coordinator.reload(&new_config).await {
                    Ok(()) => log_level.set(&new_config.daemon.log_level),
                    Err(e) => {
                        error!(error = %e, "failed to apply reloaded config, continuing with previous");
                    }
                }
            }
            Err(e) => {
                error!(error = %e, "failed to reload config, continuing with previous");
            }
        }
    }

    coordinator.shutdown().await;
    info!("Capycal stopped");
    Ok(())
}

/// Apply the `--only` selection to a reloaded config. Unlike at start, every
/// selected sync must still exist, so a reload never runs fewer syncs than asked.
fn reselect_syncs(config: &mut Config, only: &[String]) -> Result<(), String> {
    if let Some(missing) = only
        .iter()
        .find(|name| !config.syncs.iter().any(|s| &s.name == *name))
    {
        return Err(format!(
            "sync '{missing}' selected with --only is no longer in the config"
        ));
    }
    config.syncs.retain(|s| only.contains(&s.name));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::tests::{caldav_config, caldav_toml, state_dir};
    use wiremock::matchers::{body_string_contains, header, method};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const ONCE: Mode = Mode {
        once: true,
        full_sync: false,
    };

    /// Serve one event to every sync-collection REPORT, an empty listing to
    /// the full sync of a new sync and accept every PUT, expecting `cycles`
    /// sync cycles per sync of the two-sync test config.
    async fn mount_caldav(server: &MockServer, cycles: u64) {
        Mock::given(method("PROPFIND"))
            .and(header("Depth", "1"))
            .respond_with(
                ResponseTemplate::new(207)
                    .set_body_string(r#"<d:multistatus xmlns:d="DAV:"></d:multistatus>"#),
            )
            .mount(server)
            .await;
        // Source reads only; the target's change check gets a 404 and is skipped
        Mock::given(method("REPORT"))
            .and(body_string_contains("calendar-data"))
            .respond_with(ResponseTemplate::new(207).set_body_string(
                r#"<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
<d:response><d:href>/x/e1.ics</d:href><d:propstat><d:prop><d:getetag>"v1"</d:getetag>
<c:calendar-data>BEGIN:VCALENDAR
BEGIN:VEVENT
UID:e1
DTSTART:20260105T090000Z
END:VEVENT
END:VCALENDAR</c:calendar-data></d:prop></d:propstat></d:response>
<d:sync-token>tok-1</d:sync-token></d:multistatus>"#,
            ))
            .expect(2 * cycles)
            .mount(server)
            .await;
        Mock::given(method("PUT"))
            .respond_with(ResponseTemplate::new(201))
            .expect(2 * cycles)
            .mount(server)
            .await;
    }

    /// Wait until the server received `count` PUTs, so the current cycles pushed their events.
    async fn wait_for_puts(server: &MockServer, count: usize) {
        let puts = async || {
            let requests = server.received_requests().await.unwrap();
            requests
                .iter()
                .filter(|r| r.method.as_str() == "PUT")
                .count()
        };
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while puts().await < count {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("syncs did not push their events");
    }

    #[tokio::test]
    async fn test_run_once_succeeds() {
        let server = MockServer::start().await;
        mount_caldav(&server, 1).await;
        let dir = state_dir("run-once");
        let cfg = caldav_config(&server.uri(), &dir);

        run(
            cfg,
            Path::new("unused.toml"),
            ONCE,
            LogLevel::default(),
            None,
            None,
        )
        .await
        .unwrap();
        assert!(dir.join("work-to-home.json").exists());
        assert!(dir.join("home-to-work.json").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_run_once_fails_when_a_sync_fails() {
        // Without mocks every request gets 404, so each sync fails
        let server = MockServer::start().await;
        let dir = state_dir("run-once-fail");
        let cfg = caldav_config(&server.uri(), &dir);

        let err = run(
            cfg,
            Path::new("unused.toml"),
            ONCE,
            LogLevel::default(),
            None,
            None,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(&err, CliError::Logged { message, error: None } if message == "one or more syncs failed"),
            "{err:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_run_once_fatal_error() {
        let file = state_dir("run-once-fatal");
        std::fs::write(&file, "").unwrap();
        let cfg = caldav_config("http://localhost", &file);

        let err = run(
            cfg,
            Path::new("unused.toml"),
            ONCE,
            LogLevel::default(),
            None,
            None,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(&err, CliError::Logged { message, .. } if message == "fatal error"),
            "{err:?}"
        );
        let _ = std::fs::remove_file(&file);
    }

    #[tokio::test]
    async fn test_daemon_reloads_config_until_stopped() {
        let server = MockServer::start().await;
        // Initial start plus one successful reload; the broken reloads keep the syncs
        mount_caldav(&server, 2).await;
        let dir = state_dir("daemon");
        std::fs::create_dir_all(&dir).unwrap();
        let config_path = dir.join("capycal.toml");
        std::fs::write(&config_path, caldav_toml(&server.uri(), &dir)).unwrap();
        let cfg = Config::load(&config_path).unwrap();

        let mut signals = 0;
        daemon(
            &cfg,
            &config_path,
            false,
            &mut LogLevel::default(),
            None,
            None,
            async || {
                signals += 1;
                // Let the running cycles finish, a signal would cancel them
                wait_for_puts(&server, 2.min(signals) * 2).await;
                match signals {
                    1 => Signal::Reload,
                    2 => {
                        std::fs::write(&config_path, "not toml").unwrap();
                        Signal::Reload
                    }
                    3 => {
                        std::fs::write(&config_path, "[daemon]\n").unwrap();
                        Signal::Reload
                    }
                    _ => Signal::Stop,
                }
            },
        )
        .await
        .unwrap();

        assert_eq!(signals, 4);
        server.verify().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_daemon_reload_keeps_only_selection() {
        let server = MockServer::start().await;
        // One selected sync, started and reloaded once: two cycles in total
        mount_caldav(&server, 1).await;
        let dir = state_dir("daemon-only");
        std::fs::create_dir_all(&dir).unwrap();
        let config_path = dir.join("capycal.toml");
        let toml = caldav_toml(&server.uri(), &dir);
        std::fs::write(&config_path, &toml).unwrap();
        let only = ["work-to-home".to_string()];
        let mut cfg = Config::load(&config_path).unwrap();
        cfg.syncs.retain(|s| only.contains(&s.name));

        let mut signals = 0;
        daemon(
            &cfg,
            &config_path,
            false,
            &mut LogLevel::default(),
            Some(&only),
            None,
            async || {
                signals += 1;
                wait_for_puts(&server, 2.min(signals)).await;
                match signals {
                    1 => Signal::Reload,
                    2 => {
                        let renamed = toml.replace("\"work-to-home\"", "\"work-to-home-2\"");
                        std::fs::write(&config_path, renamed).unwrap();
                        Signal::Reload
                    }
                    _ => Signal::Stop,
                }
            },
        )
        .await
        .unwrap();

        assert_eq!(signals, 3);
        server.verify().await;
        assert!(!dir.join("home-to-work.json").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_daemon_reload_rejects_a_sync_another_config_runs() {
        let server = MockServer::start().await;
        // Only the initial cycles: the reload is rejected
        mount_caldav(&server, 1).await;
        let dir = state_dir("daemon-lock");
        std::fs::create_dir_all(&dir).unwrap();
        let config_path = dir.join("capycal.toml");
        let toml = caldav_toml(&server.uri(), &dir);
        std::fs::write(&config_path, &toml).unwrap();
        let cfg = Config::load(&config_path).unwrap();
        let mut lock = ConfigLock::acquire(&cfg, &config_path).unwrap();

        // Another config with the same state directory runs a sync "extra"
        let other_path = dir.join("other.toml");
        let other_toml = toml
            .replace("work-to-home", "extra")
            .replace("home-to-work", "extra-2");
        std::fs::write(&other_path, other_toml).unwrap();
        let _other = ConfigLock::acquire(&Config::load(&other_path).unwrap(), &other_path).unwrap();

        let mut signals = 0;
        daemon(
            &cfg,
            &config_path,
            false,
            &mut LogLevel::default(),
            None,
            Some(&mut lock),
            async || {
                signals += 1;
                wait_for_puts(&server, 2).await;
                if signals == 1 {
                    let renamed = toml.replace("work-to-home", "extra");
                    std::fs::write(&config_path, renamed).unwrap();
                    Signal::Reload
                } else {
                    // Time for the renamed sync to push, had the reload been applied
                    let _ = tokio::time::timeout(
                        std::time::Duration::from_secs(1),
                        wait_for_puts(&server, 3),
                    )
                    .await;
                    Signal::Stop
                }
            },
        )
        .await
        .unwrap();

        assert_eq!(signals, 2);
        server.verify().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_reselect_syncs_needs_every_selected_sync() {
        let mut cfg = crate::cli::tests::config();
        reselect_syncs(&mut cfg, &["home-to-work".to_string()]).unwrap();
        assert_eq!(cfg.syncs.len(), 1);
        assert_eq!(cfg.syncs[0].name, "home-to-work");

        let mut cfg = crate::cli::tests::config();
        let only = ["work-to-home".to_string(), "gone".to_string()];
        assert_eq!(
            reselect_syncs(&mut cfg, &only).unwrap_err(),
            "sync 'gone' selected with --only is no longer in the config"
        );
        assert_eq!(cfg.syncs.len(), 2);
    }

    #[tokio::test]
    async fn test_daemon_fails_to_start_with_unusable_state_dir() {
        let file = state_dir("daemon-fail");
        std::fs::write(&file, "").unwrap();
        let cfg = caldav_config("http://localhost", &file);

        let err = daemon(
            &cfg,
            Path::new("unused.toml"),
            false,
            &mut LogLevel::default(),
            None,
            None,
            async || Signal::Stop,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(&err, CliError::Logged { message, .. } if message == "failed to start syncs"),
            "{err:?}"
        );
        let _ = std::fs::remove_file(&file);
    }
}
