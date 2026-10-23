pub(crate) mod discover;
pub(crate) mod generate;
mod purge;
mod run;
mod service;
mod status;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{CommandFactory, Parser};
use tracing::{error, info};

use tracing_subscriber::fmt;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use crate::config::{self, Config, LogFormat};
use crate::config_migrate::ConfigUpdate;
use crate::logging::{self, LogLevel};
use crate::state::lock::ConfigLock;

#[derive(Parser)]
#[command(
    name = "capycal",
    version = crate::VERSION,
    about = "Calendar sync that knows how to keep a secret"
)]
pub struct Cli {
    /// Config file path (default: platform-specific, e.g. ~/.config/capycal/capycal.toml)
    #[arg(short, long)]
    pub config: Option<PathBuf>,

    /// Run all syncs once and exit
    #[arg(long)]
    pub once: bool,

    /// Validate config and exit
    #[arg(long)]
    pub validate: bool,

    /// Show whether a capycal process runs the config and the state of each
    /// sync, then exit
    #[arg(long)]
    pub status: bool,

    /// Install, enable and start capycal with this config as a systemd user
    /// service (Linux)
    #[arg(long)]
    pub install_service: bool,

    /// Stop and remove the systemd user service of --install-service (Linux)
    #[arg(long)]
    pub uninstall_service: bool,

    /// Generate a starter config and write it to the config path
    #[arg(long)]
    pub generate_config: bool,

    /// Discover calendars on a CalDAV server (provide server URL)
    #[arg(long, value_name = "URL")]
    pub discover: Option<String>,

    /// Start with a full sync: compare the whole source with the sink, create
    /// missing events, update differing ones, remove duplicates and orphans
    #[arg(long)]
    pub full_sync: bool,

    /// List calendars from macOS Calendar.app (EventKit)
    #[arg(long)]
    pub discover_eventkit: bool,

    /// Only run specific syncs (comma-separated names)
    #[arg(long, value_delimiter = ',')]
    pub only: Option<Vec<String>>,

    /// Delete synced events (by marker) from a sync's sink calendar, then exit
    #[arg(long, value_name = "SYNC_NAME")]
    pub purge: Option<String>,

    /// Delete ALL events from a calendar, then exit
    #[arg(long, value_name = "CALENDAR_NAME")]
    pub purge_all: Option<String>,

    /// Don't ask before --purge-all deletes every event (needed when not
    /// run in a terminal)
    #[arg(long, requires = "purge_all")]
    pub yes: bool,

    /// Print shell completions and exit
    #[arg(long, value_name = "SHELL")]
    pub completions: Option<clap_complete::Shell>,

    /// Print the man page and exit
    #[arg(long)]
    pub man: bool,

    /// Increase log verbosity (repeatable: -v, -vv, -vvv)
    #[arg(short, long, action = clap::ArgAction::Count)]
    pub verbose: u8,
}

impl Cli {
    pub fn config_path(&self) -> PathBuf {
        self.config.clone().unwrap_or_else(|| {
            config::default_config_path().unwrap_or_else(|| PathBuf::from("capycal.toml"))
        })
    }

    /// Log level override from `-v` flags, if any.
    pub fn log_level(&self) -> Option<&'static str> {
        match self.verbose {
            0 => None,
            1 => Some("debug"),
            _ => Some("trace"),
        }
    }
}

/// Run the command line, reporting a failure before returning a non-zero code.
pub async fn execute(cli: Cli) -> ExitCode {
    match dispatch(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            e.report();
            ExitCode::FAILURE
        }
    }
}

async fn dispatch(cli: Cli) -> CliResult {
    // Standalone commands that don't need a config
    if let Some(shell) = cli.completions {
        let mut command = Cli::command();
        clap_complete::generate(shell, &mut command, "capycal", &mut std::io::stdout());
        return Ok(());
    }
    if cli.man {
        return clap_mangen::Man::new(Cli::command())
            .render(&mut std::io::stdout())
            .map_err(CliError::msg);
    }
    if cli.discover_eventkit {
        return discover::eventkit();
    }
    if (cli.install_service || cli.uninstall_service) && cfg!(not(target_os = "linux")) {
        return Err(CliError::msg(
            "the service commands need systemd on Linux; on macOS use the app's \
             Start at Login or the LaunchAgent in contrib/launchd",
        ));
    }
    if cli.uninstall_service {
        return service::uninstall();
    }
    if let Some(url) = &cli.discover {
        return discover::caldav(url).await;
    }
    let config_path = cli.config_path();
    if cli.generate_config {
        return generate::write_starter_config(&config_path);
    }

    let (config, update) = load_config(&config_path, cli.config.is_some())?;
    let log_level = init_logging(&config, cli.log_level());
    run_config(&cli, config, update, &config_path, log_level).await
}

/// The commands that need a loaded config, once logging is set up.
async fn run_config(
    cli: &Cli,
    mut config: Config,
    update: Option<ConfigUpdate>,
    config_path: &Path,
    log_level: LogLevel,
) -> CliResult {
    // Looking at a config never changes it
    if cli.validate {
        return validate(&config, config_path);
    }
    if cli.status {
        return status::status(&config, config_path);
    }
    if let Some(update) = update {
        update.write();
    }
    if cli.install_service {
        require_syncs(&config, config_path)?;
        return service::install(&config, config_path);
    }
    let purging = cli.purge.is_some() || cli.purge_all.is_some();
    if !purging {
        require_syncs(&config, config_path)?;
        if let Some(only) = &cli.only {
            select_syncs(&mut config, only)?;
        }
    }
    // Held until the process exits, so no other process runs this config
    let mut lock = ConfigLock::acquire(&config, config_path)
        .map_err(|e| CliError::logged("cannot run this config", e))?;
    if let Some(sync_name) = &cli.purge {
        return purge::purge(&config, sync_name).await;
    }
    if let Some(cal_name) = &cli.purge_all {
        if !cli.yes {
            let stdin = std::io::stdin();
            let interactive = std::io::IsTerminal::is_terminal(&stdin);
            purge::confirm_purge_all(&config, cal_name, stdin.lock(), interactive)?;
        }
        return purge::purge_all(&config, cal_name).await;
    }
    run::run(
        config,
        config_path,
        run::Mode {
            once: cli.once,
            full_sync: cli.full_sync,
        },
        log_level,
        cli.only.as_deref(),
        Some(&mut lock),
    )
    .await
}

/// Log to stdout. `RUST_LOG` overrides `-v`, which overrides `log_level`;
/// only the latter changes on a config reload.
fn init_logging(config: &Config, level_override: Option<&str>) -> LogLevel {
    let level = level_override.unwrap_or(&config.daemon.log_level);
    let (filter, log_level) = logging::level_filter(level, level_override.is_some());
    let registry = tracing_subscriber::registry().with(filter);
    match config.daemon.log_format {
        LogFormat::Json => registry.with(fmt::layer().json()).init(),
        LogFormat::Pretty => registry.with(fmt::layer().with_target(false)).init(),
    }
    log_level
}

/// A failed command. `main` reports it and exits with a non-zero code.
#[derive(Debug)]
pub(crate) enum CliError {
    /// Printed to stderr as is, for commands that talk to the user directly.
    Stderr(String),
    /// Reported through tracing, for failures once logging is set up.
    Logged {
        message: String,
        error: Option<String>,
    },
}

impl CliError {
    pub fn msg(message: impl std::fmt::Display) -> Self {
        Self::Stderr(format!("error: {message}"))
    }

    pub fn logged(message: impl Into<String>, error: impl std::fmt::Display) -> Self {
        Self::Logged {
            message: message.into(),
            error: Some(error.to_string()),
        }
    }

    pub fn report(&self) {
        match self {
            Self::Stderr(message) => eprintln!("{message}"),
            Self::Logged {
                message,
                error: Some(e),
            } => error!(error = %e, "{message}"),
            Self::Logged {
                message,
                error: None,
            } => error!("{message}"),
        }
    }
}

impl From<crate::error::Error> for CliError {
    fn from(e: crate::error::Error) -> Self {
        Self::msg(e)
    }
}

pub(crate) type CliResult = std::result::Result<(), CliError>;

/// Load the config, adding hints when the default path was used.
fn load_config(
    path: &Path,
    explicit: bool,
) -> std::result::Result<(Config, Option<ConfigUpdate>), CliError> {
    Config::read(path).map_err(|e| {
        let mut message = format!("error: {e}");
        // The hints only help when the default config doesn't exist yet
        if !explicit && !path.exists() {
            message.push_str(&format!(
                "\nhint: no --config specified, looked at: {}\
                 \nhint: run `capycal --generate-config` to create a starter config there",
                path.display()
            ));
        }
        CliError::Stderr(message)
    })
}

/// Log a summary of the valid config. Fails if it has no syncs, as it
/// could not run.
fn validate(config: &Config, config_path: &Path) -> CliResult {
    let state_dir = config
        .resolve_state_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|e| format!("<error: {e}>"));
    info!(
        config = %config_path.display(),
        state_dir = %state_dir,
        syncs = config.syncs.len(),
        calendars = config.calendars.len(),
        "configuration is valid"
    );
    require_syncs(config, config_path)
}

/// Running needs at least one sync. The library accepts a config without
/// syncs, so the desktop app can show a config that is not set up yet.
fn require_syncs(config: &Config, config_path: &Path) -> CliResult {
    if config.syncs.is_empty() {
        return Err(CliError::Logged {
            message: no_syncs_message(config_path),
            error: None,
        });
    }
    Ok(())
}

fn no_syncs_message(config_path: &Path) -> String {
    format!(
        "no syncs defined in {} - add a [syncs.<name>] table",
        config_path.display()
    )
}

/// Keep only the syncs named in `--only`.
fn select_syncs(config: &mut Config, only: &[String]) -> CliResult {
    let available = sync_names(config);
    let before = config.syncs.len();
    config.syncs.retain(|s| only.contains(&s.name));
    if config.syncs.is_empty() {
        return Err(CliError::Logged {
            message: format!("no syncs matched --only filter. available: {available}"),
            error: None,
        });
    }
    info!(
        total = before,
        selected = config.syncs.len(),
        "filtered syncs via --only"
    );
    Ok(())
}

fn sync_names(config: &Config) -> String {
    config
        .syncs
        .iter()
        .map(|s| s.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn config() -> Config {
        toml::from_str(
            r#"
[daemon]

[accounts.acc]
url = "https://caldav.example.com/"
username = "u"
password = "p"

[calendars.work]
url = "https://example.com/work/"
account = "acc"

[calendars.home]
url = "https://example.com/home/"
account = "acc"

[syncs.work-to-home]
source = "work"
target = "home"
interval = "1h"
fields = ["DTSTART"]

[syncs.home-to-work]
source = "home"
target = "work"
interval = "1h"
fields = ["DTSTART"]
"#,
        )
        .unwrap()
    }

    pub fn state_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("capycal-{}-cli-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// Config TOML with calendars `work` and `home` served by `uri`, syncing both ways.
    pub fn caldav_toml(uri: &str, state_dir: &Path) -> String {
        format!(
            r#"
[daemon]
state_dir = "{}"

[accounts.acc]
url = "https://caldav.example.com/"
username = "u"
password = "p"

[calendars.work]
url = "{uri}/work/"
account = "acc"

[calendars.home]
url = "{uri}/home/"
account = "acc"
concurrency = 2

[syncs.work-to-home]
source = "work"
target = "home"
interval = "1h"
fields = ["DTSTART"]

[syncs.home-to-work]
source = "home"
target = "work"
interval = "1h"
fields = ["DTSTART"]
"#,
            state_dir.display()
        )
    }

    pub fn caldav_config(uri: &str, state_dir: &Path) -> Config {
        toml::from_str(&caldav_toml(uri, state_dir)).unwrap()
    }

    #[test]
    fn test_cli_flags() {
        let cli = Cli::parse_from(["capycal", "-c", "/etc/c.toml", "-vv", "--only", "a,b"]);
        assert_eq!(cli.config_path(), PathBuf::from("/etc/c.toml"));
        assert_eq!(cli.log_level(), Some("trace"));
        assert_eq!(cli.only, Some(vec!["a".to_string(), "b".to_string()]));

        let cli = Cli::parse_from(["capycal", "-v"]);
        assert_eq!(cli.log_level(), Some("debug"));
        assert!(cli.config_path().ends_with("capycal.toml"));
        assert_eq!(Cli::parse_from(["capycal"]).log_level(), None);
    }

    #[derive(Clone, Default)]
    struct LogBuf(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for LogBuf {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().write(data)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn capture_logs(f: impl FnOnce()) -> String {
        let buf = LogBuf::default();
        let writer = buf.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || writer.clone())
            .with_ansi(false)
            .finish();
        tracing::subscriber::with_default(subscriber, f);
        String::from_utf8(buf.0.lock().unwrap().clone()).unwrap()
    }

    #[test]
    fn test_cli_error_from_error() {
        let err = CliError::from(crate::error::Error::Config("bad".to_string()));
        assert!(
            matches!(&err, CliError::Stderr(m) if m == "error: configuration error: bad"),
            "{err:?}"
        );
    }

    #[test]
    fn test_cli_error_report_logs_message_and_cause() {
        let logs = capture_logs(|| CliError::logged("failed to load", "cause").report());
        assert!(logs.contains("ERROR"), "{logs}");
        assert!(logs.contains("failed to load"), "{logs}");
        assert!(logs.contains("error=cause"), "{logs}");

        let logs = capture_logs(|| {
            CliError::Logged {
                message: "no cause".to_string(),
                error: None,
            }
            .report()
        });
        assert!(
            logs.contains("no cause") && !logs.contains("error="),
            "{logs}"
        );
    }

    #[test]
    fn test_validate_logs_summary() {
        let mut cfg = config();
        cfg.daemon.state_dir = Some(PathBuf::from("/tmp/capycal"));
        let logs = capture_logs(|| validate(&cfg, Path::new("capycal.toml")).unwrap());
        assert!(logs.contains("configuration is valid"), "{logs}");
        assert!(logs.contains("config=capycal.toml"), "{logs}");
        assert!(logs.contains("state_dir=/tmp/capycal"), "{logs}");
        assert!(logs.contains("syncs=2"), "{logs}");
        assert!(logs.contains("calendars=2"), "{logs}");
    }

    #[test]
    fn test_validate_fails_without_syncs() {
        let mut cfg = config();
        cfg.syncs.clear();
        let mut result = Ok(());
        let logs = capture_logs(|| result = validate(&cfg, Path::new("capycal.toml")));
        assert!(logs.contains("configuration is valid"), "{logs}");
        assert!(logs.contains("syncs=0"), "{logs}");
        let Err(CliError::Logged { message, .. }) = result else {
            panic!("expected logged error");
        };
        assert_eq!(
            message,
            "no syncs defined in capycal.toml - add a [syncs.<name>] table"
        );
    }

    #[tokio::test]
    async fn test_only_running_commands_update_an_old_config() {
        let dir = state_dir("old-config");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("capycal.toml");
        let old = format!(
            "[daemon]\nstate_dir = \"{}\"\n",
            dir.join("state").display()
        );
        std::fs::write(&path, &old).unwrap();
        let run = |args: &'static [&'static str]| {
            let path = path.clone();
            async move {
                let (cfg, update) = Config::read(&path).unwrap();
                let cli = Cli::parse_from(args);
                let _ = run_config(&cli, cfg, update, &path, LogLevel::default()).await;
            }
        };
        run(&["capycal", "--validate"]).await;
        run(&["capycal", "--status"]).await;
        assert_eq!(std::fs::read_to_string(&path).unwrap(), old);
        // Fails for want of syncs, after updating the file
        run(&["capycal", "--once"]).await;
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .starts_with("version = 2\n")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_running_needs_syncs() {
        let dir = state_dir("no-syncs");
        let path = Path::new("/etc/capycal.toml");
        for args in [
            &["capycal"][..],
            &["capycal", "--once"],
            &["capycal", "--full-sync", "--once"],
        ] {
            let cli = Cli::parse_from(args);
            let cfg =
                toml::from_str(&format!("[daemon]\nstate_dir = \"{}\"\n", dir.display())).unwrap();
            let err = run_config(&cli, cfg, None, path, LogLevel::default())
                .await
                .unwrap_err();
            assert!(
                matches!(&err, CliError::Logged { message, .. }
                    if message == "no syncs defined in /etc/capycal.toml - add a [syncs.<name>] table"),
                "{args:?}: {err:?}"
            );
        }
        // Nothing was locked or written
        assert!(!dir.exists());
    }

    #[test]
    fn test_cli_definition_completions_and_man_page() {
        Cli::command().debug_assert();
        for shell in [
            clap_complete::Shell::Bash,
            clap_complete::Shell::Zsh,
            clap_complete::Shell::Fish,
        ] {
            let mut out = Vec::new();
            clap_complete::generate(shell, &mut Cli::command(), "capycal", &mut out);
            assert!(
                String::from_utf8(out)
                    .unwrap()
                    .contains("--install-service")
            );
        }
        let mut man = Vec::new();
        clap_mangen::Man::new(Cli::command())
            .render(&mut man)
            .unwrap();
        let man = String::from_utf8(man).unwrap();
        assert!(man.contains(env!("CARGO_PKG_VERSION")), "{man}");
    }

    #[tokio::test]
    async fn test_purge_needs_the_config_lock() {
        let dir = state_dir("purge-lock");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("capycal.toml");
        std::fs::write(&path, caldav_toml("http://localhost:1", &dir)).unwrap();
        let _held = ConfigLock::acquire(&Config::load(&path).unwrap(), &path).unwrap();

        for args in [
            &["capycal", "--purge", "work-to-home"][..],
            &["capycal", "--purge-all", "home"],
            &["capycal", "--full-sync", "--once"],
        ] {
            let cli = Cli::parse_from(args);
            let err = run_config(
                &cli,
                Config::load(&path).unwrap(),
                None,
                &path,
                LogLevel::default(),
            )
            .await
            .unwrap_err();
            assert!(
                matches!(&err, CliError::Logged { message, error: Some(e) }
                    if message == "cannot run this config" && e.contains("already in use")),
                "{args:?}: {err:?}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_full_sync_once_respects_only() {
        use wiremock::matchers::{body_string_contains, header, method, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("REPORT"))
            .and(body_string_contains("calendar-data"))
            .respond_with(ResponseTemplate::new(207).set_body_string(
                r#"<d:multistatus xmlns:d="DAV:"><d:sync-token>t</d:sync-token></d:multistatus>"#,
            ))
            .expect(1)
            .mount(&server)
            .await;
        // Only the sink of the selected sync is listed
        Mock::given(method("PROPFIND"))
            .and(path_regex("^/home/?$"))
            .and(header("Depth", "1"))
            .respond_with(
                ResponseTemplate::new(207)
                    .set_body_string(r#"<d:multistatus xmlns:d="DAV:"></d:multistatus>"#),
            )
            .expect(1)
            .mount(&server)
            .await;
        let dir = state_dir("full-sync-only");
        std::fs::create_dir_all(&dir).unwrap();
        // A state from before full syncs: only --full-sync makes this a full sync
        std::fs::write(
            dir.join("work-to-home.json"),
            r#"{"uid_mapping":{"entries":{}}}"#,
        )
        .unwrap();
        let path = dir.join("capycal.toml");
        std::fs::write(&path, caldav_toml(&server.uri(), &dir)).unwrap();

        let cli = Cli::parse_from(["capycal", "--full-sync", "--once", "--only", "work-to-home"]);
        assert!(cli.full_sync);
        run_config(
            &cli,
            Config::load(&path).unwrap(),
            None,
            &path,
            LogLevel::default(),
        )
        .await
        .unwrap();
        server.verify().await;
        let state = std::fs::read_to_string(dir.join("work-to-home.json")).unwrap();
        assert!(state.contains("last_full_sync"), "{state}");
        assert!(!dir.join("home-to-work.json").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_select_syncs_keeps_named() {
        let mut cfg = config();
        select_syncs(&mut cfg, &["home-to-work".to_string()]).unwrap();
        assert_eq!(cfg.syncs.len(), 1);
        assert_eq!(cfg.syncs[0].name, "home-to-work");
    }

    #[test]
    fn test_select_syncs_no_match_lists_sync_names() {
        let mut cfg = config();
        let err = select_syncs(&mut cfg, &["nope".to_string()]).unwrap_err();
        let CliError::Logged { message, .. } = err else {
            panic!("expected logged error");
        };
        assert_eq!(
            message,
            "no syncs matched --only filter. available: work-to-home, home-to-work"
        );
    }

    #[test]
    fn test_load_config_hints_only_for_default_path() {
        let missing = Path::new("/nonexistent/capycal.toml");
        let CliError::Stderr(implicit) = load_config(missing, false).unwrap_err() else {
            panic!("expected stderr error");
        };
        assert!(implicit.starts_with("error: "));
        assert!(implicit.contains("hint: no --config specified"));

        let CliError::Stderr(explicit) = load_config(missing, true).unwrap_err() else {
            panic!("expected stderr error");
        };
        assert!(!explicit.contains("hint:"));
    }

    #[test]
    fn test_load_config_no_hints_for_an_existing_broken_file() {
        let dir = std::env::temp_dir().join(format!("capycal-hint-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("capycal.toml");
        std::fs::write(&path, "[calendars.a]\neventkit_all_events = true\n").unwrap();
        let CliError::Stderr(message) = load_config(&path, false).unwrap_err() else {
            panic!("expected stderr error");
        };
        assert!(message.contains("unknown field `eventkit_all_events`"));
        assert!(!message.contains("hint:"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
