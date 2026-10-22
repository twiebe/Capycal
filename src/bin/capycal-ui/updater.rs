//! Fetching the latest release and replacing the running app with it: the
//! DMG and its checksum from the release, copied out of the mounted image
//! beside the installed bundle, then swapped in and started once this
//! process is gone. The steps that need macOS tools are macOS only.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::updates::{Release, parse_release};

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent(concat!("capycal/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(30))
        .read_timeout(Duration::from_secs(60))
        .build()
        .map_err(|e| e.to_string())
}

/// The latest stable release (GitHub leaves out drafts and pre-releases).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub async fn check(client: &reqwest::Client) -> Result<Release, String> {
    let repo = REPOSITORY.trim_start_matches("https://github.com/");
    let response = client
        .get(format!(
            "https://api.github.com/repos/{repo}/releases/latest"
        ))
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(unreachable)?;
    match response.status().as_u16() {
        200 => {}
        403 | 429 => return Err("GitHub asks to try again later".to_string()),
        status => return Err(format!("GitHub answered with status {status}")),
    }
    parse_release(&response.text().await.map_err(unreachable)?)
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn unreachable(e: reqwest::Error) -> String {
    if e.is_connect() || e.is_timeout() {
        "GitHub cannot be reached".to_string()
    } else {
        e.to_string()
    }
}

/// The checksum for `file` in the text of a `.sha256` file (`shasum`
/// output: the hex digest, two spaces or a space and `*`, the name).
pub fn expected_sha(text: &str, file: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let (sum, name) = line.split_once(' ')?;
        let name = name.trim_start_matches([' ', '*']);
        (name == file && sum.len() == 64 && sum.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| sum.to_ascii_lowercase())
    })
}

/// The `.app` bundle the executable `exe` belongs to.
pub fn bundle_of(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    (macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && bundle.extension()? == "app")
        .then(|| bundle.to_path_buf())
}

/// A path beside `bundle`, hidden: `.Capycal.app.<suffix>`.
fn beside(bundle: &Path, suffix: &str) -> PathBuf {
    let name = bundle.file_name().unwrap_or_default().to_string_lossy();
    bundle.with_file_name(format!(".{name}.{suffix}"))
}

/// Put the bundle `new` in place of `installed`, which is removed. If the
/// swap fails, `installed` stays as it was.
pub fn replace_bundle(installed: &Path, new: &Path) -> Result<(), String> {
    let old = beside(installed, "old");
    let _ = std::fs::remove_dir_all(&old);
    std::fs::rename(installed, &old)
        .map_err(|e| format!("Cannot move {} aside: {e}", installed.display()))?;
    if let Err(e) = std::fs::rename(new, installed) {
        let _ = std::fs::rename(&old, installed);
        return Err(format!("Cannot put the new version in place: {e}"));
    }
    if let Err(e) = std::fs::remove_dir_all(&old) {
        tracing::warn!(path = %old.display(), error = %e, "cannot remove the old version");
    }
    Ok(())
}

/// Download `release` and replace `bundle` with it, reporting the steps.
/// `cancelled` is looked at between them; once the swap starts it is too
/// late.
#[cfg(target_os = "macos")]
pub async fn install(
    client: &reqwest::Client,
    release: &Release,
    bundle: &Path,
    cancelled: &std::sync::atomic::AtomicBool,
    progress: impl Fn(crate::updates::Step),
) -> Result<(), String> {
    use crate::updates::Step;
    use std::io::Write;
    use std::sync::atomic::Ordering;

    const CANCELLED: &str = "Cancelled";
    let check_cancel = || {
        if cancelled.load(Ordering::Relaxed) {
            Err(CANCELLED.to_string())
        } else {
            Ok(())
        }
    };
    if bundle
        .components()
        .any(|c| c.as_os_str() == "AppTranslocation")
    {
        return Err(
            "macOS runs Capycal from a temporary place. Move it to the Applications \
                    folder, start it from there and update again."
                .to_string(),
        );
    }
    // Fails early, before the download, without write access
    let staging = beside(bundle, "new");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir(&staging).map_err(|e| {
        format!(
            "Capycal cannot replace itself in {}: {e}",
            bundle.parent().unwrap_or(bundle).display()
        )
    })?;
    let temp = TempDir(std::env::temp_dir().join(format!("capycal-update-{}", std::process::id())));
    let result = async {
        std::fs::create_dir_all(&temp.0).map_err(|e| e.to_string())?;
        let file = format!("capycal-v{0}-macos.dmg", release.version);
        let url = format!("{REPOSITORY}/releases/download/v{}/{file}", release.version);

        progress(Step::Downloading(0, None));
        let sums = fetch(client, &format!("{url}.sha256")).await?.text().await;
        let expected = expected_sha(&sums.map_err(unreachable)?, &file)
            .ok_or("The release has no checksum for its disk image.")?;
        let mut response = fetch(client, &url).await?;
        let total = response.content_length();
        let dmg = temp.0.join(&file);
        let mut out = std::fs::File::create(&dmg).map_err(|e| e.to_string())?;
        let (mut done, mut shown) = (0u64, 0u64);
        while let Some(chunk) = response.chunk().await.map_err(unreachable)? {
            check_cancel()?;
            out.write_all(&chunk).map_err(|e| e.to_string())?;
            done += chunk.len() as u64;
            // About every percent
            if done - shown >= total.unwrap_or(u64::MAX) / 100 {
                shown = done;
                progress(Step::Downloading(done, total));
            }
        }
        drop(out);

        progress(Step::Checking);
        let sum = run(
            "/usr/bin/shasum",
            &["-a".as_ref(), "256".as_ref(), dmg.as_os_str()],
        )?;
        if sum.split_whitespace().next() != Some(expected.as_str()) {
            return Err("The download does not match the release's checksum.".to_string());
        }
        check_cancel()?;

        progress(Step::Installing);
        let mount = temp.0.join("mount");
        run(
            "/usr/bin/hdiutil",
            &[
                "attach".as_ref(),
                "-nobrowse".as_ref(),
                "-readonly".as_ref(),
                "-noautoopen".as_ref(),
                "-mountpoint".as_ref(),
                mount.as_os_str(),
                dmg.as_os_str(),
            ],
        )?;
        let copied = run(
            "/usr/bin/ditto",
            &[mount.join("Capycal.app").as_os_str(), staging.as_os_str()],
        );
        let _ = run(
            "/usr/bin/hdiutil",
            &["detach".as_ref(), "-force".as_ref(), mount.as_os_str()],
        );
        copied?;
        if !staging.join("Contents/MacOS/capycal-ui").is_file() {
            return Err("The disk image holds no Capycal app.".to_string());
        }
        check_cancel()?;
        replace_bundle(bundle, &staging)
    }
    .await;
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    result
}

#[cfg(target_os = "macos")]
async fn fetch(client: &reqwest::Client, url: &str) -> Result<reqwest::Response, String> {
    let response = client.get(url).send().await.map_err(unreachable)?;
    if !response.status().is_success() {
        return Err(format!(
            "The download failed with status {}.",
            response.status().as_u16()
        ));
    }
    Ok(response)
}

/// Run a tool and return its output, or its error output as the error.
#[cfg(target_os = "macos")]
fn run(program: &str, args: &[&std::ffi::OsStr]) -> Result<String, String> {
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .map_err(|e| format!("Cannot run {program}: {e}"))?;
    if !output.status.success() {
        let name = Path::new(program).file_name().unwrap_or_default();
        return Err(format!(
            "{} failed: {}",
            name.to_string_lossy(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Removed on drop.
#[cfg(target_os = "macos")]
struct TempDir(PathBuf);

#[cfg(target_os = "macos")]
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Start `bundle` once this process is gone. A process that has not quit
/// after 60 seconds is killed. The waiting shell has a process group of its
/// own, so it outlives the app.
#[cfg(target_os = "macos")]
pub fn relaunch(bundle: &Path) -> Result<(), String> {
    use std::os::unix::process::CommandExt;
    const WAIT: &str = r#"i=0
while kill -0 "$1" 2>/dev/null; do
  if [ "$i" -ge 600 ]; then kill -9 "$1"; sleep 1; break; fi
  sleep 0.1; i=$((i + 1))
done
exec /usr/bin/open "$2""#;
    std::process::Command::new("/bin/sh")
        .args(["-c", WAIT, "sh", &std::process::id().to_string()])
        .arg(bundle)
        .process_group(0)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(drop)
        .map_err(|e| format!("Cannot start the new version: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("capycal-updater-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn bundle(dir: &Path, name: &str, marker: &str) -> PathBuf {
        let app = dir.join(name);
        std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        std::fs::write(app.join("Contents/MacOS/capycal-ui"), marker).unwrap();
        app
    }

    fn marker(app: &Path) -> String {
        std::fs::read_to_string(app.join("Contents/MacOS/capycal-ui")).unwrap()
    }

    #[test]
    fn test_expected_sha() {
        let sum = "ab".repeat(32);
        let file = "capycal-v0.2.1-macos.dmg";
        assert_eq!(
            expected_sha(&format!("{sum}  {file}\n"), file),
            Some(sum.clone())
        );
        assert_eq!(
            expected_sha(&format!("{} *{file}", sum.to_uppercase()), file),
            Some(sum.clone())
        );
        assert_eq!(expected_sha(&format!("{sum}  other.dmg"), file), None);
        assert_eq!(expected_sha(&format!("abc  {file}"), file), None);
        assert_eq!(expected_sha("Not Found", file), None);
    }

    #[test]
    fn test_bundle_of() {
        assert_eq!(
            bundle_of(Path::new(
                "/Applications/Capycal.app/Contents/MacOS/capycal-ui"
            )),
            Some(PathBuf::from("/Applications/Capycal.app"))
        );
        assert_eq!(bundle_of(Path::new("/opt/homebrew/bin/capycal-ui")), None);
        assert_eq!(
            bundle_of(Path::new("/x/Capycal/Contents/MacOS/capycal-ui")),
            None
        );
    }

    #[test]
    fn test_replace_bundle_swaps_and_removes_the_old() {
        let dir = temp("swap");
        let installed = bundle(&dir, "Capycal.app", "old");
        let new = bundle(&dir, ".Capycal.app.new", "new");
        replace_bundle(&installed, &new).unwrap();
        assert_eq!(marker(&installed), "new");
        assert!(!new.exists());
        assert!(!dir.join(".Capycal.app.old").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn test_replace_bundle_restores_on_failure() {
        let dir = temp("restore");
        let installed = bundle(&dir, "Capycal.app", "old");
        let error = replace_bundle(&installed, &dir.join("missing.app")).unwrap_err();
        assert!(
            error.starts_with("Cannot put the new version in place"),
            "{error}"
        );
        assert_eq!(marker(&installed), "old");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn test_replace_bundle_in_a_read_only_folder_fails_untouched() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp("readonly");
        let folder = dir.join("Applications");
        std::fs::create_dir(&folder).unwrap();
        let installed = bundle(&folder, "Capycal.app", "old");
        let new = bundle(&dir, "new.app", "new");
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o555)).unwrap();
        let result = replace_bundle(&installed, &new);
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o755)).unwrap();
        // Root may write anyway; the swap then simply works
        if let Err(error) = result {
            assert!(error.starts_with("Cannot move"), "{error}");
            assert_eq!(marker(&installed), "old");
            assert_eq!(marker(&new), "new");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
