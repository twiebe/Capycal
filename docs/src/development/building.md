# Building and testing

Capycal is a Rust crate (edition 2024): a library with the `capycal` CLI and the `capycal-ui` menu bar app (feature `ui`). Contributor conventions are in [`CLAUDE.md`](https://github.com/twiebe/capycal/blob/main/CLAUDE.md).

## Build and test

```bash
make build        # cargo build
make build-ui     # with the menu bar app (feature ui)
make run-ui       # run the menu bar app as a plain binary (macOS)
make run-app      # build Capycal.app and open it (macOS)
make test         # cargo test
make test-ui      # tests of the menu bar app
make test-caldav  # end-to-end tests against a throwaway Radicale server
make test-caldav SERVER=stalwart  # the same against Stalwart, or SERVER=nextcloud; needs Docker
make test-caldav-docker # the same in a Linux container, needs only Docker
make test-jmap    # JMAP end-to-end tests against a throwaway Stalwart; needs Docker
make test-eventkit # tests against Calendar.app (macOS)
make lint         # cargo fmt --check + clippy -D warnings, without and with the app
make fmt          # format the code
```

`make lint test test-ui` checks what CI checks, except the GTK app. `make help` lists every target with a short description.

EventKit code is compiled on macOS only. Its conversion logic (`eventkit/convert.rs`) and the app's display-free modules are compiled and tested on Linux too, so `make test test-ui` works on both platforms.

`make test-caldav` (`scripts/test-caldav.sh`) installs [Radicale](https://radicale.org/) into `target/radicale`, starts it on 127.0.0.1:5299 with a temporary storage directory and runs `tests/caldav_e2e.rs`: the `capycal` binary syncs between two calendars named `capycal-test-...` and the tests check what arrives in the target (busy blocker fields, updates, series with exceptions, buffers, cancelled events, full sync after lost state, purge). The tests are `#[ignore]`d, so a plain `cargo test` skips them. It needs `python3` and `curl`.

`make test-caldav SERVER=stalwart` and `SERVER=nextcloud` run the same tests against these servers in Docker containers with pinned images, set up in the script. They run one test at a time: Stalwart allows an account 4 requests at once by default, and Nextcloud in the container uses SQLite. Nextcloud gets an app password, since a real password costs each request about 200 ms of hashing. In CI, `.github/workflows/e2e-servers.yml` runs them for changes near a server and on a release; the Radicale run stays in every CI job.

`make test-eventkit` runs the `#[ignore]`d `live_tests` in `src/eventkit/client.rs` against the real Calendar.app database: Calendar.app as a source (all-day, multi-day, timed, alarms, series as occurrences) copied into a second calendar with nothing left to change for a full sync, series with excluded, moved and cancelled occurrences, UNTIL, monthly and all-day rules, rewriting a series, a single event round trip with the full sync comparison, unsupported rules, delete and purge. Every test creates its own calendar named `capycal-test-<pid>-<test>` ("On My Mac" if available, else in the account of the default calendar) and removes it afterwards, also when the test fails. Calendars with that prefix left over by an aborted run are removed at the next run; no other calendar is touched. Run it from Terminal: macOS asks once whether Terminal may access your calendars.

The first line of the output names the source the calendars go to. Prefer "On My Mac" (type 0): in an account that syncs (iCloud and CalDAV report type 2), a removed calendar can come back when the account syncs its creation afterwards, so the tests remove it again until it stays away, and other devices briefly see it. macOS hides "On My Mac" while an account syncs calendars. To get it back, turn off Calendars for every account in System Settings > Internet Accounts, create a calendar under "On My Mac" in Calendar.app (not named `capycal-test-...`, the tests would remove it), and turn the accounts back on. "On My Mac" stays as long as it holds a calendar.

The Linux desktop app needs the GTK 4 and libadwaita development files (`sudo apt install libgtk-4-dev libadwaita-1-dev`, or `sudo dnf install gtk4-devel libadwaita-devel`): `make build-gtk`, `make run-gtk`, `make test-gtk`, `make clippy-gtk`. It is not part of `make lint`, so working on the rest needs no GTK. On macOS, `brew install gtk4 libadwaita pkgconf` lets `make run-gtk` show its windows for development; the tray exists only on Linux, so there the app opens its menu as a window.

For manual tests against a real server, `make up-dev` starts a local [Radicale](https://radicale.org/) CalDAV server on port 5232 without authentication (`docker-compose.dev.yml`); `make down-dev` stops it.

## Release builds

The distribution targets write to `dist/`:

| Target | Output in `dist/` | Host |
|---|---|---|
| `make dist-macos` | `aarch64-apple-darwin/capycal` (CLI) | macOS, Apple silicon |
| `make app` | `Capycal.app` (app with the CLI inside, signed ad hoc, see below) | macOS, Apple silicon |
| `make dmg` | `capycal-v<version>-macos.dmg` | macOS, Apple silicon |
| `make dist-linux-x86_64` | `x86_64-unknown-linux-musl/capycal` (static) | any, needs Docker buildx |
| `make dist-linux-arm64` | `aarch64-unknown-linux-musl/capycal` (static) | any, needs Docker buildx |
| `make dist-linux-app-x86_64` | `capycal-app_<version>-1_amd64.deb`, `capycal-app-<version>-1.x86_64.rpm` | any, needs Docker buildx |
| `make dist-linux-app-arm64` | `capycal-app_<version>-1_arm64.deb`, `capycal-app-<version>-1.aarch64.rpm` | any, needs Docker buildx |
| `make dist-flatpak` | `capycal-app-v<version>-<arch>.flatpak` for this machine | Linux, needs flatpak-builder and the `flathub` remote (`--user`) |
| `make dist` | everything the host can build | |

The Linux builds run in Docker (`packaging/linux/Dockerfile.musl`, for the desktop app `packaging/linux/Dockerfile.app` on Debian 13 like the release); a foreign architecture is emulated and therefore slow. The Linux release packages are made by `packaging/linux/build-packages.sh <target>` from a native release build: it generates man page, completions and `/usr/bin` units into `dist/pkg`, then writes the tarball, a `.deb` (cargo-deb) and an `.rpm` (cargo-generate-rpm) according to `[package.metadata.deb]` and `[package.metadata.generate-rpm]` in `Cargo.toml`. The desktop app is packaged separately by `packaging/linux/build-app-packages.sh <target>` from a glibc build with `--features gtk` (the `gtk` variants in the same metadata: desktop entry, icons, GTK dependencies); the release builds it in a Debian 13 container, whose GTK 4.18 and libadwaita 1.7 are the oldest it supports (its glibc is the oldest among the systems that have them). The Flatpak (`app.capycal.Capycal`) is built by `packaging/linux/build-flatpak.sh <arch>` from `packaging/linux/flatpak/app.capycal.Capycal.yml` on the GNOME 51 runtime; cargo fetches the crates during the build, which Flathub would not allow, but the app is not on Flathub. A wrapper (`capycal-app.sh`) points `XDG_CONFIG_HOME` and `XDG_STATE_HOME` back to the host's, so the Flatpak shares config and state with the packages; inside, `FLATPAK_ID` names the exported tray icons (`icons::tray_name`) and switches Start at Login to the Background portal (`autostart::request_background`). The release job writes the AUR PKGBUILDs `capycal-bin.PKGBUILD` (from the musl tarballs) and `capycal-app-bin.PKGBUILD` (from the desktop app's `.deb`) from the templates in `packaging/linux/aur/` and attaches them to the release; with the secret `AUR_SSH_KEY` (the private SSH key of the AUR account) `packaging/linux/aur/publish.sh` pushes them with their `.SRCINFO`, made by `makepkg` in an Arch container. It also writes the Homebrew formula `capycal.rb` and cask `capycal-app.rb` from the templates in `packaging/homebrew/` and attaches them to the release; with the secret `HOMEBREW_TAP_TOKEN` (write access to `twiebe/homebrew-capycal`) it commits them to the tap. The image `ghcr.io/twiebe/capycal` is built by `.github/workflows/image.yml` from the two musl tarballs of a published release with `packaging/docker/Dockerfile` (no compilation, distroless base) and pushed with the workflow's own token. The release workflow calls it after publishing; `gh workflow run image.yml -f tag=v0.2.0` builds the image of an existing release (with `-f latest=false` for an older one, so `latest` stays). The app is assembled by `packaging/macos/build-app.sh` and packed by `build-dmg.sh`. The release builds it on `macos-26`: macOS draws an app in the current style (Liquid Glass) only when it was linked against the current SDK, an older Xcode gives it the look of macOS 15.

Every package and the app's About > Acknowledgements include `THIRD-PARTY-NOTICES.txt`, the licenses of all crates compiled into the binaries. After adding or updating a dependency, regenerate it with `make notices` (needs `cargo install --locked cargo-about@0.9.2 --features cli`: CI pins that version, since 0.9.3 replaces the ISC text of aws-lc with a blank template); CI fails with `make check-notices` when it is out of date or a crate uses a license `about.toml` does not accept.

`build-app.sh` signs the bundle ad hoc. macOS ties **Always Allow** of a Keychain item to the code hash of the build unless the app is signed with a Developer ID (an Apple team), so every changed build, including `make run-ui`, asks again for each Keychain item. A config whose passwords come from files or environment variables avoids the prompts during development. Its icon comes from `packaging/macos/icon/`: `make-icon.py` draws `capycal.svg`, the PNGs in `AppIcon.iconset/` and the book's favicons in `docs/theme/` (all checked in; rerun it after changing the drawing, see its header), and `build-app.sh` turns the iconset into `AppIcon.icns` with `iconutil`.

## Documentation

This book is built with [mdBook](https://rust-lang.github.io/mdBook/) from `docs/`. Install mdBook (`cargo install mdbook` or a [release binary](https://github.com/rust-lang/mdBook/releases)), then:

```bash
make docs         # build into docs/book/
make docs-serve   # serve on http://localhost:3000 with live reload
```

Without mdBook, `make docs-docker` and `make docs-serve-docker` do the same in a container and need only Docker.

The book is published at [capycal.app/docs](https://capycal.app/docs/) by the Pages workflow of the website repository `twiebe/capycal-web`, which builds it from `main`. Pushes to `main` that change `docs/` check the build and trigger that workflow (`.github/workflows/docs.yml`, needs the secret `WEBSITE_DISPATCH_TOKEN`).
