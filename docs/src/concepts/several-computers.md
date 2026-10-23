# One sync on several computers

You can run the same syncs on two or more computers, for example a Mac and a Linux laptop you switch between, so the copies stay current whichever one is on. Give every computer the same config, for example through Syncthing, a cloud folder or Git:

- Keep the sync names the same. A copy's address and marker are derived from the sync name and the source event, so every computer writes the same copy instead of a second one.
- Keep the state local. Leave `state_dir` out (each computer uses its OS default) or set `CAPYCAL_STATE_DIR` per computer; never share the state directory.
- On a Mac the default state directory is inside the config's folder (`~/Library/Application Support/capycal/state/`), so share only the file: keep it in the shared folder and link it, for example `ln -s ~/Sync/capycal.toml ~/Library/Application\ Support/capycal/capycal.toml`, or start the app with `-c` (see [Another config file](../app/system.md#another-config-file)). Settings writes through the link.
- Credentials from the Keychain or the Secret Service are stored per computer, so set the password on each one. `{ env = "..." }` and `{ command = "..." }` work if each computer provides them.

Each computer keeps its own state, so it does not see what another one wrote. Writes of the same source event go to the same address, so normal cycles converge on their own. What a computer has to notice is a change in the target without a change in the source: the other computer removed the copies (`--purge`, **Remove Synced Events**), wrote them with other settings, or you edited or deleted a copy by hand. Capycal asks a CalDAV target at the start of each cycle what changed since its own last write (RFC 6578 sync-collection, without downloading events). If one of this sync's own events changed, the cycle is a full sync, which restores the copies; the log says "synced events changed in the target, running a full sync". Changes to other events in the calendar, including the copies of other syncs, do not count.

Limits:

- Only CalDAV targets. A Calendar.app target is not checked, a full sync there runs only on the other triggers above.
- The target server must support sync-collection; Nextcloud, Radicale, Baikal, SabreDAV, Fastmail, iCloud and most others do. Without it Capycal logs this at debug level and relies on the daily full sync.
- A change made elsewhere while a cycle writes to the target counts as the cycle's own and is picked up by the next full sync.
- Switching a sync off (`enabled = false`) or changing its transforms reaches the other computer with the config. A purge with the desktop app writes `enabled = false`, so the other computer stops too; a purge with `--purge` does not, and the other computer restores the copies at its next cycle.
