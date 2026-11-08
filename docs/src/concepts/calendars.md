# Calendars

Each `[calendars.<name>]` table defines one calendar. The type follows from the keys: a table with `eventkit_calendar` is a Calendar.app calendar, one with `ics_url` a [calendar feed](#calendar-feeds-ical-address), one with `jmap_calendar` a [JMAP calendar](#jmap), any other table is a CalDAV calendar. A calendar can be the source of some syncs and the target of others; a feed can only be a source.

The `<name>` is yours to choose. It is used in `source` / `target` of syncs, in the `{{source_name}}` placeholder and in the markers on created events.

> **In the app:** you don't write calendar entries yourself. Picking a calendar in a sync's **From** or **Into** adds its `[calendars.<name>]` entry when you save, and a calendar no sync uses any more is removed after asking.

## Accounts

> **In the app:** **Add Account…** in Settings asks for the protocol (**CalDAV** or **JMAP**), a name, the server URL, the user name and the password; **Test Connection** looks up the calendars with them. A saved account keeps its protocol.

A CalDAV or JMAP calendar belongs to an `[accounts.<name>]` table, which holds the server and the credentials. Several calendars on one server share one account, so the password is set once.

```toml
[accounts.nextcloud]
url = "https://cloud.example.org/remote.php/dav/"
username = "alice"
password = { env = "CAPYCAL_NEXTCLOUD_PASSWORD" }

[calendars.personal]
account = "nextcloud"
url = "https://cloud.example.org/remote.php/dav/calendars/alice/personal/"

[calendars.blockers]
account = "nextcloud"
url = "https://cloud.example.org/remote.php/dav/calendars/alice/work-blockers/"
```

| Key | Required | Description |
|---|---|---|
| `url` | yes | Server address, the one you would give a calendar app; `capycal --discover` starts from it |
| `username` | yes | User name for HTTP Basic authentication |
| `password` | yes | A [credential](#passwords) |
| `protocol` | no, default `"caldav"` | `"caldav"`, or `"jmap"` for a [JMAP](#jmap) server |

## CalDAV

> **In the app:** the calendar menus of a sync list the calendars every account finds on the server. `concurrency` is **Parallel writes** in the Advanced settings of a sync that writes into the calendar.

```toml
[accounts.work]
url = "https://caldav.example.com/"
username = "alice"
password = { env = "CAPYCAL_WORK_PASSWORD" }

[calendars.work]
account = "work"
url = "https://caldav.example.com/calendars/alice/work/"
concurrency = 4
```

| Key | Required | Description |
|---|---|---|
| `account` | yes | Name of the [account](#accounts) whose credentials the calendar uses |
| `url` | yes | URL of the calendar collection (not the server root) |
| `concurrency` | no, default `2` | Maximum events written (PUT) to this calendar at the same time as a target, and parallel deletes of a purge |

`capycal --discover <server>` lists the collection URLs of an account. Some examples of collection URLs:

| Server | Collection URL |
|---|---|
| Nextcloud | `https://cloud.example.org/remote.php/dav/calendars/<user>/<calendar>/` |
| Radicale | `https://radicale.example.org/<user>/<calendar-id>/` |
| Fastmail | `https://caldav.fastmail.com/dav/calendars/user/<email>/<calendar-id>/` |
| iCloud | found with `--discover https://caldav.icloud.com` and an app-specific password |

Only HTTP Basic authentication is supported. Providers that require OAuth (Google, Microsoft 365) can't be used over CalDAV directly. To copy from them, use their [calendar feed](#calendar-feeds-ical-address), or on macOS add the account to Calendar.app and use it through EventKit.

`concurrency` is shared by every sync that writes to the calendar and also sets the parallel deletes of `--purge` / `--purge-all`. Raise it for large initial syncs to a server that handles parallel requests well, set it to `1` for one that answers parallel writes with errors. The desktop app sets it as **Parallel writes** in a sync's Advanced settings when the target is a CalDAV calendar. Reads are not limited by it, and the deletes of a sync cycle run one at a time per sync. Capycal retries on HTTP 408, 429 and 5xx with exponential backoff and honours `Retry-After`.

## Calendar.app (EventKit, macOS only)

> **In the app:** the calendar menus list Calendar.app's calendars under their account; Settings writes the identifier.

```toml
[calendars.work_mac]
eventkit_calendar = "Exchange/Calendar"
```

`eventkit_calendar` accepts three forms, tried in this order:

1. The calendar identifier, for example `"A1B2C3D4-..."`. Stable across renames.
2. `"Account/Name"`, for example `"iCloud/Family"` or `"Exchange/Calendar"`. Use this when several accounts have a calendar with the same name.
3. The calendar name alone, for example `"Work"`. The first calendar with that name wins.

`capycal --discover-eventkit` prints every calendar with its account and identifier, and a snippet with both the identifier and the `Account/Name` form:

```toml
[calendars.work]
eventkit_calendar = "A1B2C3D4-0000-0000-0000-000000000000"
# eventkit_calendar = "Exchange/Work"
```

A config with an `eventkit_calendar` fails validation on Linux. How long the source is queried is set per sync with the [EventKit window options](syncs.md#calendarapp-query-window). See [macOS and Calendar.app](calendar-app.md) for what is read and written.

## Calendar feeds (iCal address)

> **In the app:** **Add Calendar Feed…** at the end of a sync's **From** menu, or below the calendars of the assistant's first step, asks for the address. Feeds you added are listed under **Feeds**; as a target they are greyed out.

Many calendar services publish a calendar as one iCalendar file at a private address. Capycal reads such a feed as a source: no account, no password, and it works with Google Calendar and Outlook.com, which don't offer CalDAV with a password.

```toml
[calendars.google]
ics_url = "https://calendar.google.com/calendar/ical/alice%40gmail.com/private-0123456789abcdef/basic.ics"
```

| Key | Required | Description |
|---|---|---|
| `ics_url` | yes | The feed's address, `https://`, `http://` or `webcal://`. A [credential](#passwords) like a password: a plain string or `{ env = ... }`, `{ file = ... }`, `{ command = ... }`, `{ keychain = ... }` |

Where to find the address:

| Service | Address |
|---|---|
| Google Calendar | Settings, **Settings for my calendars**, the calendar, **Integrate calendar**: **Secret address in iCal format** |
| Outlook.com, Microsoft 365 | Settings, **Calendar**, **Shared calendars**, **Publish a calendar**: the calendar with **Can view all details**, then the **ICS** link |
| Others | Look for "publish", "share link", "subscribe" or "iCal". Capycal reads any address that returns an `.ics` file |

A feed is read-only: a sync with a feed as `target` fails validation, and `--purge-all` refuses it. Every check downloads the whole calendar, unless the server answers that it did not change (`ETag` / `Last-Modified`). Capycal finds the changed events by their content, so a feed that sets every event's `DTSTAMP` to the time of the request doesn't rewrite all copies. Some feeds (waste collection calendars, for example) are generated on each request and give every event a new UID: Capycal recognizes the events by their content and keeps their copies. For such a feed, an event whose time or title changes gets a new copy in place of the old one, and a full sync replaces all its copies once.

The service decides how fresh its feed is. Google updates the secret address only every few hours, so a change in Google Calendar can take that long to reach the target, whatever `interval` says. An `interval` of `"30m"` or `"1h"` is plenty.

Treat the address like a password: anyone who has it can read the calendar. It never appears in Capycal's logs, and `{{source_url}}` gives `ics://feed` for a feed. To keep it out of the config file, use another credential form, for example `ics_url = { keychain = "google-feed" }`; the desktop app then shows the address as set in the config file. A feed behind HTTP Basic authentication takes the user name and password in the address: `https://user:password@example.com/calendar.ics`. If the address leaks, reset it with the service (Google: **Reset** next to the secret address) and enter the new one.

## JMAP

> **In the app:** choose **JMAP** under **Protocol** when you add the account; the calendar menus of a sync list its calendars like those of a CalDAV account. `concurrency` is **Parallel writes** in the Advanced settings of a sync that writes into the calendar.

Servers with JMAP for Calendars (`urn:ietf:params:jmap:calendars`), such as [Stalwart](https://stalw.art/), work as source and as target, also together with any other kind of calendar.

```toml
[accounts.mail]
url = "https://mail.example.org/"
username = "alice@example.org"
password = { env = "CAPYCAL_MAIL_PASSWORD" }
protocol = "jmap"

[calendars.team]
account = "mail"
jmap_calendar = "Team"
```

| Key | Required | Description |
|---|---|---|
| `account` | yes | Name of an [account](#accounts) with `protocol = "jmap"` |
| `jmap_calendar` | yes | Id or name of the calendar. The id stays the same when the calendar is renamed; a name must be unique in the account |
| `concurrency` | no, default `2` | Maximum events written to this calendar at the same time as a target, and parallel deletes of a purge |

The account's `url` is the server (Capycal asks for `/.well-known/jmap`) or the address of its JMAP session. `capycal --discover-jmap <server>` lists the calendars with their ids.

What is different from CalDAV:

- **Only the account's own calendars.** Capycal reads the primary calendar account of the JMAP session; calendars another user shares as an account of their own are not listed.
- **Password sign-in.** Capycal signs in with HTTP Basic authentication, like for CalDAV. Servers that accept only tokens through a web sign-in cannot be used.
- **Requests stay on the server of the session.** If the session names another address for its API, as a server behind a proxy or a port mapping may do, Capycal keeps the path but stays on the address it reached the session at, so the password never goes to another host. For the same reason a `/.well-known/jmap` that redirects to another host fails with "... sends the session to ..., use that as the server URL": put that address into `url`.
- **Invitations.** Capycal does not ask the server to send any: JMAP's `sendSchedulingMessages` stays off, also for a copy that keeps `ATTENDEE` or `ORGANIZER`.
- **Other properties.** Events travel as JSCalendar and are translated. Times, recurrence with its exceptions, `SUMMARY`, `DESCRIPTION`, `LOCATION`, `URL`, `CATEGORIES`, `CLASS`, `COLOR`, `PRIORITY`, `CONFERENCE`, `ORGANIZER`, `ATTENDEE`, `STATUS`, `TRANSP` and `VALARM` arrive; other properties a `fields` entry or a `set_field` names, like `X-...`, are not written to a JMAP target. A reminder read from a JMAP source is a display alarm, whatever action it had.
- **The markers** are the properties `capycal.app:source` and `capycal.app:sync`. Over CalDAV the same server shows them under other names, so before you move a target from JMAP to CalDAV, remove its copies with `capycal --purge <sync>`. The other way round needs nothing: copies written over CalDAV are found by their `X-CAPYCAL-*` properties and taken over.

## Passwords

> **In the app:** a password typed into an account is stored in the Keychain (macOS) or the Secret Service (Linux) and written as `password = { keychain = "<account>" }`. A password from `env`, `file` or `command` stays as it is until you type a new one.

The `password` of an account (`[accounts.<name>]`) can come from five places:

```toml
password = { env = "CAPYCAL_WORK_PASSWORD" }   # environment variable
password = { file = "secrets/work.txt" }      # file content
password = { command = "pass show capycal/work" }  # output of a command
password = { keychain = "work" }              # macOS Keychain or Secret Service
password = "literal"                          # plain text in the config
```

- **`env`**: the value of the environment variable. Works well with systemd `EnvironmentFile=`, Docker `env_file` and `.env` files.
- **`file`**: the content of the file, with leading and trailing whitespace (including the final newline) removed. A relative path is resolved against the directory of the config file. Works well with Docker or systemd secrets (`/run/secrets/...`).
- **`command`**: the output of a shell command (`sh -c`, run in the config file's directory), with leading and trailing whitespace removed. Use it for password managers and keyrings: `secret-tool lookup service capycal account work` (GNOME Keyring, KWallet and other Secret Service keyrings), `pass show capycal/work | head -n1`, `op read op://Private/work/password` (1Password) or `security find-generic-password -s capycal-work -w` (macOS Keychain). If the command fails or prints nothing, the sync does not start; its error output goes to Capycal's standard error (the journal under systemd), its regular output (the password) is never shown. A keyring that needs an unlocked desktop session doesn't work in a system service.
- **`keychain`**: an entry in the macOS Keychain or, on Linux, the Secret Service (GNOME Keyring, KWallet). The entry has the service `app.capycal` and the name given here as its account (`username` attribute). To set one by hand: `security add-generic-password -s app.capycal -a work -w` on macOS, `secret-tool store --label "capycal work" service app.capycal username work` on Linux. A server or a system service without a desktop session has no Secret Service; use `env` or `file` there.
- **Plain string**: kept in the config file. Not recommended; if you use it, make the config readable only by you (`chmod 600`).

Credentials are resolved when the syncs start and on every config reload, so a missing variable or file shows up on `capycal --once`, not on `--validate`. Capycal never logs passwords.

Use an app-specific password where the provider offers one (iCloud, Fastmail, Nextcloud).
