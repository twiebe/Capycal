# Releasing

Releases are built by GitHub Actions (`.github/workflows/release.yml`) when a tag `v*` is pushed.

1. On an up-to-date `main` without uncommitted changes, set the version, commit and tag it in one step:

   ```bash
   make release-tag VERSION=0.2.0
   git push origin main v0.2.0
   ```

   `release-tag` writes the version to `Cargo.toml` and `Cargo.lock`, commits "Release 0.2.0" and adds the annotated tag `v0.2.0`. Its message opens in your editor, filled with the subjects of the commits since the last `v*` tag; closing the editor with an empty message cancels the tag. The message becomes the release notes, followed by GitHub's link to the full changelog, so write it for users. If `Cargo.toml` already has the version (a tag deleted to tag a later commit), it only tags. The tag must be `v` plus the version from `Cargo.toml`; the workflow fails otherwise. Push the tag from the command line and do not create the release in GitHub's web interface: the workflow creates it and fails if one exists.

`Cargo.toml` is the only source of the version. A build of the commit tagged `v<version>` shows it plain (`capycal --version`, About, the Settings sidebar); every other build adds the commit, e.g. `0.2.0 (abc1234)`, set by `build.rs` as `capycal::VERSION`. Outside a git checkout the version stays plain.

The workflow:

- builds and tests the CLI for `aarch64-apple-darwin`, `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl` on native runners, and packs each into `capycal-v<version>-<target>.tar.gz` (binary, `README.md`, `LICENSE`, `CONTRIBUTORS.md`, `THIRD-PARTY-NOTICES.txt`, `capycal.example.toml`) plus a `.deb` and an `.rpm` for Linux;
- builds `Capycal.app` and `capycal-v<version>-macos.dmg`;
- builds the Linux desktop app as `capycal-app` `.deb` and `.rpm` for x86_64 and arm64;
- with `PACKAGES_GPG_KEY` signs the `.rpm` files with the key of the [package repositories](#package-repositories);
- writes a `.sha256` file per archive, the AUR PKGBUILDs and the Homebrew formula and cask;
- publishes a GitHub release with generated notes;
- once the release is published, builds the container image for linux/amd64 and linux/arm64 from its musl tarballs (`image.yml`) and pushes it to `ghcr.io/twiebe/capycal` as `<version>`, `<major>.<minor>` and `latest`. The first push creates the package as private: make it public once under the package's settings on GitHub (Package settings > Change visibility);
- with `HOMEBREW_TAP_TOKEN` commits the formula and cask to `twiebe/homebrew-capycal`, with `AUR_SSH_KEY` pushes `capycal-bin` and `capycal-app-bin` to the AUR;
- with `WEBSITE_DISPATCH_TOKEN` asks `twiebe/capycal-web` to rebuild the site, which builds the [package repositories](#package-repositories) from the newest releases.

## Package repositories

capycal.app/packages carries an APT repository (`apt/`), an RPM repository (`rpm/`) and a Flatpak repository (`flatpak/`, with `capycal.flatpakrepo` and `app.capycal.Capycal.flatpakref` next to it) with the `.deb`, `.rpm` and `.flatpak` files of the newest three releases. The Pages workflow of `twiebe/capycal-web` builds them on every run, from the GitHub releases (`scripts/fetch-releases.sh`, `scripts/build-repos.sh`), so nothing is stored in git; the debs are copied into the site, the RPM metadata points at the release downloads, the Flatpak bundles are imported into an OSTree repository (`flatpak build-import-bundle`, so a rebuild keeps their commits and clients see no false update). Both carry an AppStream catalog of the desktop app, so software centers find it before it is installed. `make test-repos` there builds them with a throwaway key and installs from them in Debian, Ubuntu and Fedora containers, on Fedora also through PackageKit, which software centers use.

APT checks the signed `InRelease`. dnf and PackageKit check each rpm against the key instead (`gpgcheck`): PackageKit cannot import a key for signed RPM metadata and would skip the repository. So the release workflow signs the rpms, and the RPM repository takes only rpms signed with the key; releases before the first signed one stay out of it.

Both are signed with a key of their own, kept as the secret `PACKAGES_GPG_KEY` of `twiebe/capycal-web`, and of `twiebe/capycal` for the rpms; without it the site goes out without `packages/`, and the rpms unsigned. To create it:

```bash
export GNUPGHOME=$(mktemp -d)
gpg --batch --passphrase '' --quick-gen-key "Capycal Packages <packages@capycal.app>" rsa4096 sign never
gpg --armor --export-secret-keys | gh secret set PACKAGES_GPG_KEY -R twiebe/capycal-web
gpg --armor --export-secret-keys | gh secret set PACKAGES_GPG_KEY -R twiebe/capycal
gpg --armor --export-secret-keys > capycal-packages.key   # keep offline
gpg --fingerprint
rm -rf "$GNUPGHOME"
```

The key does not expire: apt and dnf check against the copy users fetched when they set up the repository and never fetch it again, so an expiry date, even one extended later on the server, would break every installation once it passes. Replace the key only if it leaks; everyone then has to fetch the new one.

A manual run (`workflow_dispatch`) builds and uploads the artifacts without publishing anything.

## Other workflows

| Workflow | Trigger | Does |
|---|---|---|
| `ci.yml` | push to `main`, pull requests | fmt, clippy and tests on Linux and macOS, with and without `ui` |
| `image.yml` | called by `release.yml`, manual with a release tag | builds and pushes the container image of a published release |
| `docs.yml` | push to `main` or pull request changing `docs/`, manual | builds this book; on `main` it asks `twiebe/capycal-web` to publish it on capycal.app/docs |
