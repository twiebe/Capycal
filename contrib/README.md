# contrib

Service definitions for running Capycal permanently. They assume the binary is at `/usr/local/bin/capycal`; adjust the path otherwise (e.g. `/opt/homebrew/bin/capycal`).

| File | Platform | Runs as |
|---|---|---|
| `systemd/capycal.service` | Linux | system service with a dynamic, unprivileged user; config in `/etc/capycal`, state in `/var/lib/capycal` |
| `systemd/capycal-user.service` | Linux | your user, default config and state paths |
| `launchd/app.capycal.plist` | macOS | LaunchAgent in your login session, required for Calendar.app (EventKit) access |

Installation steps are in the comment at the top of each file. On Linux, `capycal --install-service` sets up the user service for you.

The LaunchAgent is for the headless `capycal` CLI. The macOS menu bar app (`Capycal.app`) starts itself through its "Start at Login" menu item instead. Don't run both with the same config: they share a config lock, and the second one does not sync.

## Notes

- **Config reload**: all three forward a reload to Capycal as `SIGHUP`. An invalid new config is rejected and the running syncs continue. Instead of sending `SIGHUP`, you can set `watch_config = true` in `[daemon]` to reload whenever the config file changes.
- **Passwords**: keep them out of the unit files. Use `{ env = "..." }` with the environment file the systemd units read, or `{ file = "..." }`.
- **Calendar access on macOS**: macOS asks for calendar access the first time Capycal reads Calendar.app. When Capycal runs from a terminal, the permission belongs to the terminal app. Under launchd it belongs to the Capycal binary itself. Run `launchctl kickstart gui/$(id -u)/app.capycal` once while logged in and confirm the prompt; check `~/Library/Logs/capycal.log` and System Settings > Privacy & Security > Calendars if no events arrive.
- **Logs**: systemd sends output to the journal (`journalctl -u capycal`, or `journalctl --user -u capycal`). Set `log_format = "json"` in `[daemon]` for structured logs.
