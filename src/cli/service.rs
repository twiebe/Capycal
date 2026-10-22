use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::state::lock::lock_holder;

use super::{CliError, CliResult};

const UNIT_NAME: &str = "capycal.service";
/// First line of a unit capycal wrote, so it never overwrites or removes another.
const MARKER: &str = "# Written by capycal --install-service";

/// Runs `systemctl --user` (or `loginctl`) with these arguments.
type Run<'a> = dyn FnMut(&str, &[&str]) -> Result<String, String> + 'a;

/// Install capycal as a systemd user service for the config at `config_path`,
/// enable and start it.
pub fn install(config: &Config, config_path: &Path) -> CliResult {
    let paths = Paths::current(config_path)?;
    if let Some(pid) = lock_holder(config, config_path)
        .map_err(CliError::msg)?
        .map(|h| h.pid)
    {
        return Err(CliError::Stderr(format!(
            "error: config {} is already run by pid {pid}; stop that process first",
            paths.config.display()
        )));
    }
    install_with(&paths, &mut run_command)
}

pub fn uninstall() -> CliResult {
    uninstall_with(&unit_path()?, &mut run_command)
}

struct Paths {
    exe: PathBuf,
    config: PathBuf,
    unit: PathBuf,
}

impl Paths {
    fn current(config_path: &Path) -> Result<Self, CliError> {
        let exe = std::env::current_exe()
            .and_then(std::fs::canonicalize)
            .map_err(|e| CliError::msg(format!("cannot find the capycal binary: {e}")))?;
        let config = std::fs::canonicalize(config_path).map_err(|e| {
            CliError::msg(format!("cannot find config {}: {e}", config_path.display()))
        })?;
        Ok(Self {
            exe,
            config,
            unit: unit_path()?,
        })
    }
}

fn unit_path() -> Result<PathBuf, CliError> {
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .ok_or_else(|| CliError::msg("neither XDG_CONFIG_HOME nor HOME is set"))?;
    Ok(config_home.join("systemd/user").join(UNIT_NAME))
}

fn run_command(program: &str, args: &[&str]) -> Result<String, String> {
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .map_err(|e| format!("cannot run {program}: {e}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(format!(
            "{program} {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

fn install_with(paths: &Paths, run: &mut Run) -> CliResult {
    if let Ok(existing) = std::fs::read_to_string(&paths.unit)
        && !existing.starts_with(MARKER)
    {
        return Err(CliError::Stderr(format!(
            "error: {} exists and was not written by Capycal; remove it first",
            paths.unit.display()
        )));
    }
    let dir = paths.unit.parent().unwrap_or(&paths.unit);
    std::fs::create_dir_all(dir)
        .and_then(|()| std::fs::write(&paths.unit, unit_file(&paths.exe, &paths.config)))
        .map_err(|e| CliError::msg(format!("cannot write {}: {e}", paths.unit.display())))?;
    eprintln!("wrote {}", paths.unit.display());

    let systemctl = |run: &mut Run, args: &[&str]| -> CliResult {
        let mut all = vec!["--user"];
        all.extend_from_slice(args);
        run("systemctl", &all).map(|_| ()).map_err(CliError::msg)
    };
    systemctl(run, &["daemon-reload"])?;
    systemctl(run, &["enable", "--now", UNIT_NAME])?;
    eprintln!(
        "Capycal runs as a user service now.\n  \
         status: systemctl --user status capycal\n  \
         log:    journalctl --user -u capycal -f\n  \
         reload: systemctl --user reload capycal (after editing the config)"
    );
    let user = std::env::var("USER").unwrap_or_default();
    let linger = run("loginctl", &["show-user", &user, "--property=Linger"]);
    if linger.is_ok_and(|out| out.trim() == "Linger=no") {
        eprintln!(
            "It stops when you log out. To keep it running, enable lingering:\n  \
             loginctl enable-linger {user}"
        );
    }
    Ok(())
}

fn uninstall_with(unit: &Path, run: &mut Run) -> CliResult {
    let existing = match std::fs::read_to_string(unit) {
        Ok(existing) => existing,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(CliError::Stderr(format!(
                "error: no Capycal user service installed ({} not found)",
                unit.display()
            )));
        }
        Err(e) => {
            return Err(CliError::msg(format!(
                "cannot read {}: {e}",
                unit.display()
            )));
        }
    };
    if !existing.starts_with(MARKER) {
        return Err(CliError::Stderr(format!(
            "error: {} was not written by Capycal; leaving it alone",
            unit.display()
        )));
    }
    // Already stopped or disabled is fine
    let _ = run("systemctl", &["--user", "disable", "--now", UNIT_NAME]);
    std::fs::remove_file(unit)
        .map_err(|e| CliError::msg(format!("cannot remove {}: {e}", unit.display())))?;
    run("systemctl", &["--user", "daemon-reload"]).map_err(CliError::msg)?;
    eprintln!(
        "removed {}; Capycal no longer runs as a service",
        unit.display()
    );
    Ok(())
}

fn unit_file(exe: &Path, config: &Path) -> String {
    format!(
        "{MARKER}
[Unit]
Description=Capycal Calendar Sync daemon
Documentation=https://capycal.app/docs/
Wants=network-online.target
After=network-online.target

[Service]
Type=simple
ExecStart={} --config {}
ExecReload=/bin/kill -HUP $MAINPID
EnvironmentFile=-%h/.config/capycal/capycal.env
Restart=on-failure
RestartSec=30s
TimeoutStopSec=45s

[Install]
WantedBy=default.target
",
        quote(exe),
        quote(config)
    )
}

/// A path as one systemd command line word: quoted, with `\`, `"`, `%` and
/// `$` escaped.
fn quote(path: &Path) -> String {
    let escaped = path
        .to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%")
        .replace('$', "$$");
    format!("\"{escaped}\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("capycal-{}-service-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn paths(dir: &Path) -> Paths {
        Paths {
            exe: PathBuf::from("/opt/cal cal/capycal"),
            config: PathBuf::from("/home/a/100%/capycal.toml"),
            unit: dir.join("systemd/user/capycal.service"),
        }
    }

    #[test]
    fn test_unit_file_quotes_paths() {
        let unit = unit_file(
            Path::new("/opt/cal cal/capycal"),
            Path::new("/x/$y/100%.toml"),
        );
        assert!(unit.starts_with(MARKER));
        assert!(
            unit.contains("ExecStart=\"/opt/cal cal/capycal\" --config \"/x/$$y/100%%.toml\"\n"),
            "{unit}"
        );
    }

    #[test]
    fn test_install_writes_enables_and_reinstalls() {
        let dir = test_dir("install");
        let paths = paths(&dir);
        let mut calls = Vec::new();
        let mut run = |program: &str, args: &[&str]| {
            calls.push(format!("{program} {}", args.join(" ")));
            Ok("Linger=no\n".to_string())
        };
        install_with(&paths, &mut run).unwrap();
        install_with(&paths, &mut run).unwrap();
        let unit = std::fs::read_to_string(&paths.unit).unwrap();
        assert!(
            unit.contains("--config \"/home/a/100%%/capycal.toml\""),
            "{unit}"
        );
        assert_eq!(
            calls[..3],
            [
                "systemctl --user daemon-reload",
                "systemctl --user enable --now capycal.service",
                &format!(
                    "loginctl show-user {} --property=Linger",
                    std::env::var("USER").unwrap_or_default()
                ),
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_foreign_units_are_left_alone() {
        let dir = test_dir("foreign");
        let paths = paths(&dir);
        std::fs::create_dir_all(paths.unit.parent().unwrap()).unwrap();
        std::fs::write(&paths.unit, "[Unit]\nDescription=mine\n").unwrap();
        let mut run = |_: &str, _: &[&str]| -> Result<String, String> { panic!("no commands") };
        let Err(CliError::Stderr(msg)) = install_with(&paths, &mut run) else {
            panic!("expected a refusal");
        };
        assert!(msg.contains("not written by Capycal"), "{msg}");
        assert!(uninstall_with(&paths.unit, &mut run).is_err());
        assert!(paths.unit.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_uninstall_disables_and_removes() {
        let dir = test_dir("uninstall");
        let paths = paths(&dir);
        install_with(&paths, &mut |_, _| Ok(String::new())).unwrap();
        let mut calls = Vec::new();
        uninstall_with(&paths.unit, &mut |program, args| {
            calls.push(format!("{program} {}", args.join(" ")));
            Ok(String::new())
        })
        .unwrap();
        assert!(!paths.unit.exists());
        assert_eq!(
            calls,
            [
                "systemctl --user disable --now capycal.service",
                "systemctl --user daemon-reload"
            ]
        );
        let Err(CliError::Stderr(msg)) = uninstall_with(&paths.unit, &mut |_, _| Ok(String::new()))
        else {
            panic!("expected an error");
        };
        assert!(msg.contains("no Capycal user service installed"), "{msg}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
