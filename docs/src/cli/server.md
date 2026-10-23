# Running on a server

For a permanent setup, run the `capycal` daemon under a service manager. Ready-made service definitions are in [`contrib/`](https://github.com/twiebe/capycal/tree/main/contrib); they expect the binary at `/usr/local/bin/capycal`. Install steps are in the comment at the top of each file.

Before enabling a service, check the config with `capycal --validate -c <config>` and one manual `capycal --once -v -c <config>`.

## systemd

### System service

[`contrib/systemd/capycal.service`](https://github.com/twiebe/capycal/blob/main/contrib/systemd/capycal.service) runs Capycal as a dynamic, unprivileged user with a hardened sandbox:

- Config: `/etc/capycal/capycal.toml`.
- Passwords: `VAR=value` lines in `/etc/capycal/capycal.env` (mode `0600`), referenced as `password = { env = "VAR" }`.
- State: `/var/lib/capycal`, created by systemd (`StateDirectory=`) and passed to Capycal as `CAPYCAL_STATE_DIR`, since the dynamic user has no home directory. Don't set a different `state_dir` in the config.
- Config updates: the daemon may not write `/etc`, so `ExecStartPre=-+... --migrate-config` brings a config of an older Capycal up to date before each start, outside the sandbox (see [Updates of the config file](config-file.md#updates-of-the-config-file)). A reload does not update the file; restart after an update of Capycal.

```bash
sudo install -m 0755 capycal /usr/local/bin/capycal
sudo install -D -m 0644 capycal.toml /etc/capycal/capycal.toml
sudo install -D -m 0600 capycal.env /etc/capycal/capycal.env
sudo cp contrib/systemd/capycal.service /etc/systemd/system/
sudo systemctl daemon-reload && sudo systemctl enable --now capycal
```

### User service

The quickest way to run Capycal as your user is:

```bash
capycal --install-service            # or: capycal -c /path/to/capycal.toml --install-service
```

It writes `~/.config/systemd/user/capycal.service` with the absolute paths of the Capycal binary and the config, reloads systemd, enables and starts the service, and tells you if lingering is off (without it the service stops when you log out; `loginctl enable-linger $USER` changes that). Run it again after moving the binary or the config. It refuses while another Capycal process runs the config, and it never overwrites a `capycal.service` it didn't write. `capycal --uninstall-service` stops and removes it again. Install the binary to its final place first (for example `/usr/local/bin`), since the unit points to the binary you run it with.

Passwords go into `~/.config/capycal/capycal.env`, files referenced with `{ file = "..." }` or a [password manager command](../concepts/calendars.md#passwords).

To set it up by hand instead, [`contrib/systemd/capycal-user.service`](https://github.com/twiebe/capycal/blob/main/contrib/systemd/capycal-user.service) is the same unit with the default config and state paths:

```bash
mkdir -p ~/.config/systemd/user
cp contrib/systemd/capycal-user.service ~/.config/systemd/user/capycal.service
systemctl --user daemon-reload && systemctl --user enable --now capycal
loginctl enable-linger $USER   # keep it running while you are logged out
```

### Logs and reload

```bash
journalctl -u capycal -f            # system service
journalctl --user -u capycal -f     # user service
sudo systemctl reload capycal       # re-read the config (SIGHUP)
```

## Docker

Each release is published as the multi-architecture image `ghcr.io/twiebe/capycal` (linux/amd64, linux/arm64), tagged with the version (`0.2.0`), the minor version (`0.2`) and `latest`. It holds the static Linux binary on a [distroless](https://github.com/GoogleContainerTools/distroless) base (CA certificates and time zones, no shell) and runs as the unprivileged user `nonroot` (uid 65532). [`contrib/docker/compose.yml`](https://github.com/twiebe/capycal/blob/main/contrib/docker/compose.yml) runs the daemon with the config read-only and the state in a volume:

```bash
mkdir capycal && cd capycal
curl -LO https://raw.githubusercontent.com/twiebe/capycal/main/contrib/docker/compose.yml
curl -L -o capycal.toml https://raw.githubusercontent.com/twiebe/capycal/main/capycal.example.toml
# edit capycal.toml; put passwords into .env
docker compose run --rm capycal --validate
docker compose up -d
```

- The config is `/etc/capycal/capycal.toml`, the default path in the image, so commands such as `--validate`, `--status` or `--once` need no `--config`.
- The image sets `CAPYCAL_STATE_DIR=/var/lib/capycal`, where the compose file mounts the state volume, so the state survives a recreated container. Leave `state_dir` unset in `capycal.toml`, or set it to a path inside that volume.
- Put the passwords into `.env` next to `compose.yml` and reference them as `password = { env = "..." }`. `RUST_LOG` in `.env` overrides `log_level`, also on reload.
- Time conditions of transforms use the zone in `TZ`, `UTC` in the compose file. Set it to yours, such as `Europe/Berlin`.
- Pin a version (`ghcr.io/twiebe/capycal:0.2`) instead of `latest` to update only when you change the tag; `docker compose pull && docker compose up -d` updates.
- The config is mounted read-only, so the container cannot update a config of an older Capycal: it runs with the update in memory and logs a warning. Update the file once with the directory mounted writable, as your user:

  ```bash
  docker run --rm --user "$(id -u):$(id -g)" -v "$PWD":/config \
    ghcr.io/twiebe/capycal --config /config/capycal.toml --migrate-config
  ```

```bash
docker compose logs -f capycal          # logs
docker compose kill -s HUP capycal      # reload the config
docker compose exec capycal capycal --status
```

The image runs the CLI only. Calendar.app (EventKit) is not available in a container, so sync CalDAV accounts and calendar feeds there.

### From source

The repository's `Dockerfile` builds the CLI from source into a Debian slim image, and the `docker-compose.yml` at its top runs that instead of the published image, with the config in `capycal.toml` and the passwords in `.env` (from `.env.example`) in the checkout:

```bash
git clone https://github.com/twiebe/capycal && cd capycal
cp capycal.example.toml capycal.toml
cp .env.example .env
docker compose up -d --build
```

The Makefile has shortcuts for it: `make up`, `make down`, `make logs`, `make reload`.

## launchd (headless macOS)

On a Mac that should sync without the desktop app, run the CLI as a LaunchAgent with [`contrib/launchd/app.capycal.plist`](https://github.com/twiebe/capycal/blob/main/contrib/launchd/app.capycal.plist). It runs in your login session, which Calendar.app (EventKit) access requires; a system LaunchDaemon can't read calendars.

1. Replace `USERNAME` in the plist with your user name.
2. Install and start it:

   ```bash
   cp contrib/launchd/app.capycal.plist ~/Library/LaunchAgents/
   launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/app.capycal.plist
   ```

3. Under launchd the calendar permission belongs to the `capycal` binary itself. Run `launchctl kickstart gui/$(id -u)/app.capycal` once while logged in and confirm the prompt.

```bash
tail -f ~/Library/Logs/capycal.log                          # logs
launchctl kill HUP gui/$(id -u)/app.capycal     # reload the config
launchctl bootout gui/$(id -u)/app.capycal      # stop and remove
```

Use passwords from files (`{ file = "..." }`) rather than environment variables, so they don't end up in the plist.

Don't run the LaunchAgent and the [desktop app](../app/menu.md) with the same config; they share a config lock and the second one does not sync.

## Logs

The CLI logs to stdout. With `log_format = "json"` in `[daemon]`, each line is a JSON object with timestamp, level, message and fields (`sync`, `error`, ...), ready for Loki, Elasticsearch or `jq`:

```bash
journalctl -u capycal -o cat | jq 'select(.level == "ERROR")'
```

Useful messages at `info` level: `capycal started`, `sync started`, `sync cycle done`, `deleted synced event`, `config reloaded successfully`, `sync cycle failed` (error). A block of work starts with `<name> started` and ends with `<name> done` (with `took`, the duration), `<name> done with errors` (warning), `<name> cancelled` or `<name> failed`, for example `full sync started` and `full sync done`. Use `-v` or `log_level = "debug"` for per-event details.

## Reload

A running daemon re-reads its config on `SIGHUP` (all service managers above forward their reload command as `SIGHUP`), or by itself with `watch_config = true`. An invalid new config is rejected and the syncs continue unchanged. See [Reloading the config](config-file.md#reloading-the-config).
