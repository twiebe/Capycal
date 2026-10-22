//! Sets `CAPYCAL_VERSION`: the crate version, plus the commit when the
//! build is not of the release tag `v<version>`, e.g. `0.2.0 (abc1234)`.

use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn main() {
    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    // Outside a git checkout (a source tarball) the version stays plain
    let full = match git(&["rev-parse", "--short", "HEAD"]) {
        Some(commit) => {
            let tags = git(&["tag", "--points-at", "HEAD"]).unwrap_or_default();
            if tags.lines().any(|tag| tag == format!("v{version}")) {
                version
            } else {
                format!("{version} ({commit})")
            }
        }
        None => version,
    };
    println!("cargo:rustc-env=CAPYCAL_VERSION={full}");

    if let Some(dir) = git(&["rev-parse", "--git-dir"]) {
        // A missing path would rerun the script on every build
        for path in ["HEAD", "refs", "packed-refs"] {
            if std::path::Path::new(&dir).join(path).exists() {
                println!("cargo:rerun-if-changed={dir}/{path}");
            }
        }
    }
}
