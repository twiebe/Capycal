//! "Start at Login": `SMAppService.mainApp` when running from an app bundle,
//! otherwise (or if that fails) a LaunchAgent plist in ~/Library/LaunchAgents.

use std::path::Path;

/// Label and file name of the fallback LaunchAgent. Differs from the CLI's
/// `app.capycal` agent in contrib/.
pub const LAUNCH_AGENT_LABEL: &str = "app.capycal-ui";

/// Whether `exe` is the executable inside an `.app` bundle, which
/// `SMAppService.mainApp` needs.
pub fn in_app_bundle(exe: &Path) -> bool {
    exe.parent()
        .is_some_and(|dir| dir.ends_with("Contents/MacOS"))
        && exe
            .ancestors()
            .nth(3)
            .is_some_and(|app| app.extension().is_some_and(|ext| ext == "app"))
}

/// LaunchAgent that starts `exe` once at login.
pub fn launch_agent_plist(exe: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LAUNCH_AGENT_LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>ProcessType</key>
    <string>Interactive</string>
</dict>
</plist>
"#,
        xml_escape(&exe.to_string_lossy())
    )
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(target_os = "macos")]
pub use mac::{is_enabled, set_enabled};

#[cfg(target_os = "macos")]
mod mac {
    use std::path::PathBuf;

    use objc2_service_management::{SMAppService, SMAppServiceStatus};
    use tracing::{info, warn};

    use super::{LAUNCH_AGENT_LABEL, in_app_bundle, launch_agent_plist};

    fn bundled() -> bool {
        std::env::current_exe().is_ok_and(|exe| in_app_bundle(&exe))
    }

    fn launch_agent_path() -> Option<PathBuf> {
        directories::BaseDirs::new().map(|d| {
            d.home_dir()
                .join("Library/LaunchAgents")
                .join(format!("{LAUNCH_AGENT_LABEL}.plist"))
        })
    }

    fn service_status() -> SMAppServiceStatus {
        unsafe { SMAppService::mainAppService().status() }
    }

    fn set_service(enabled: bool) -> Result<(), String> {
        let service = unsafe { SMAppService::mainAppService() };
        let result = if enabled {
            unsafe { service.registerAndReturnError() }
        } else {
            unsafe { service.unregisterAndReturnError() }
        };
        result.map_err(|e| e.localizedDescription().to_string())
    }

    pub fn is_enabled() -> bool {
        let registered = bundled()
            && matches!(
                service_status(),
                SMAppServiceStatus::Enabled | SMAppServiceStatus::RequiresApproval
            );
        registered || launch_agent_path().is_some_and(|p| p.exists())
    }

    pub fn set_enabled(enabled: bool) -> Result<(), String> {
        if enabled { enable() } else { disable() }
    }

    fn enable() -> Result<(), String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        if in_app_bundle(&exe) {
            match set_service(true) {
                Ok(()) => {
                    info!(method = "SMAppService", "start at login enabled");
                    if service_status() == SMAppServiceStatus::RequiresApproval {
                        warn!("start at login needs approval in System Settings > Login Items");
                        unsafe { SMAppService::openSystemSettingsLoginItems() };
                    }
                    return Ok(());
                }
                Err(e) => warn!(error = %e, "SMAppService failed, using a LaunchAgent instead"),
            }
        }
        let path = launch_agent_path().ok_or("no home directory")?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        std::fs::write(&path, launch_agent_plist(&exe)).map_err(|e| e.to_string())?;
        info!(method = "LaunchAgent", path = %path.display(), "start at login enabled");
        Ok(())
    }

    fn disable() -> Result<(), String> {
        if bundled() && service_status() != SMAppServiceStatus::NotRegistered {
            set_service(false)?;
            info!(method = "SMAppService", "start at login disabled");
        }
        if let Some(path) = launch_agent_path().filter(|p| p.exists()) {
            std::fs::remove_file(&path).map_err(|e| e.to_string())?;
            info!(method = "LaunchAgent", path = %path.display(), "start at login disabled");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_in_app_bundle() {
        assert!(in_app_bundle(Path::new(
            "/Applications/Capycal.app/Contents/MacOS/capycal-ui"
        )));
        assert!(!in_app_bundle(Path::new("/usr/local/bin/capycal-ui")));
        assert!(!in_app_bundle(Path::new(
            "/tmp/build/Contents/MacOS/capycal-ui"
        )));
    }

    #[test]
    fn test_launch_agent_plist() {
        let plist = launch_agent_plist(Path::new("/Users/a&b/capycal <x>/capycal-ui"));
        assert!(plist.contains("<string>app.capycal-ui</string>"));
        assert!(plist.contains("<string>/Users/a&amp;b/capycal &lt;x&gt;/capycal-ui</string>"));
        assert!(plist.contains("<key>RunAtLoad</key>\n    <true/>"));
    }
}
