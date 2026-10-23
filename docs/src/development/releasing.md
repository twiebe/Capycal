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
- writes a `.sha256` file per archive, the AUR PKGBUILDs and the Homebrew formula and cask;
- publishes a GitHub release with generated notes;
- once the release is published, builds the container image for linux/amd64 and linux/arm64 from its musl tarballs (`image.yml`) and pushes it to `ghcr.io/twiebe/capycal` as `<version>`, `<major>.<minor>` and `latest`. The first push creates the package as private: make it public once under the package's settings on GitHub (Package settings > Change visibility);
- with `HOMEBREW_TAP_TOKEN` commits the formula and cask to `twiebe/homebrew-capycal`, with `AUR_SSH_KEY` pushes `capycal-bin` and `capycal-app-bin` to the AUR;
- with `WEBSITE_DISPATCH_TOKEN` asks `twiebe/capycal-web` to rebuild the site, which builds the [package repositories](#package-repositories) from the newest releases.

## Package repositories

capycal.app/packages carries an APT repository (`apt/`) and an RPM repository (`rpm/`) with the `.deb` and `.rpm` files of the newest three releases. The Pages workflow of `twiebe/capycal-web` builds them on every run, from the GitHub releases (`scripts/fetch-releases.sh`, `scripts/build-repos.sh`), so nothing is stored in git; the debs are copied into the site, the RPM metadata points at the release downloads. `make test-repos` there builds them with a throwaway key and installs from them in Debian, Ubuntu and Fedora containers.

Both are signed with a key of their own, kept as the secret `PACKAGES_GPG_KEY` of `twiebe/capycal-web`; without it the site goes out without `packages/`. To create it (again, before it expires):

```bash
export GNUPGHOME=$(mktemp -d)
gpg --batch --passphrase '' --quick-gen-key "Capycal Packages <packages@capycal.app>" rsa4096 sign 3y
gpg --armor --export-secret-keys | gh secret set PACKAGES_GPG_KEY -R twiebe/capycal-web
gpg --armor --export-secret-keys > capycal-packages.key   # keep offline
gpg --fingerprint
```

Users fetch the public key from the site, so a new key reaches them with the next `curl` of the install steps; anyone who installed before has to fetch it again. Extend the expiry of the existing key instead (`gpg --quick-set-expire <fingerprint> 3y`, then set the secret again) to spare them that.

A manual run (`workflow_dispatch`) builds and uploads the artifacts without publishing anything.

## Other workflows

| Workflow | Trigger | Does |
|---|---|---|
| `ci.yml` | push to `main`, pull requests | fmt, clippy and tests on Linux and macOS, with and without `ui` |
| `image.yml` | called by `release.yml`, manual with a release tag | builds and pushes the container image of a published release |
| `docs.yml` | push to `main` or pull request changing `docs/`, manual | builds this book; on `main` it asks `twiebe/capycal-web` to publish it on capycal.app/docs |
