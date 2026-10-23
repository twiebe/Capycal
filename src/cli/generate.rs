use std::path::Path;

use super::{CliError, CliResult};

/// Starter config written by `--generate-config` and the menu bar app. Every
/// setting is commented out, so it loads as a valid config without syncs.
/// Lines starting with `# ` are settings, lines starting with `##` explain.
pub const STARTER_CONFIG: &str = r###"## Capycal configuration
##
## Capycal copies events from a source calendar to a target calendar and keeps
## only the properties you choose. Documentation:
## https://capycal.app/docs/
##
## Everything below but the version is commented out. Lines starting with
## "##" explain, lines starting with "# " are settings: remove the "# " to
## use one. A config needs at least one calendar pair and one [syncs.<name>]
## table.

## The format of this file. Capycal updates an older file when it loads it
## and keeps the old one next to it as capycal.toml.<date>-<time>.bak.
version = 2

## --- Daemon settings (optional, shown with their defaults) ---------------

# [daemon]
## State files and config locks. Default: $CAPYCAL_STATE_DIR, else
## ~/.local/state/capycal (Linux) or ~/Library/Application Support/capycal/state (macOS)
# state_dir = "/var/lib/capycal"
## trace, debug, info, warn, error or a tracing filter such as "info,capycal=debug"
# log_level = "info"
## "pretty" or "json" (CLI output only)
# log_format = "pretty"
## CLI daemon: reload when this file changes. The desktop app always does.
# watch_config = false

## --- Accounts -------------------------------------------------------------
## Each [accounts.<key>] is a CalDAV account: the server and the credentials
## its calendars use. `url` is the server address you would give a calendar
## app; `capycal --discover <server url>` lists the account's calendars.
## The password takes one of five forms:
##   password = { env = "CAPYCAL_WORK_PASSWORD" }  environment variable
##   password = { file = "secrets/work.txt" }     file content, relative to this file
##   password = { command = "pass show work" }    output of a command (password manager)
##   password = { keychain = "work" }             macOS Keychain or Secret Service entry
##   password = "secret"                          plain text, not recommended
# [accounts.work]
# url = "https://caldav.example.com/"
# username = "alice"
# password = { env = "CAPYCAL_WORK_PASSWORD" }

## --- Calendars ------------------------------------------------------------
## Each [calendars.<key>] is one calendar. The key is your own name for it;
## syncs refer to it as source or target. A calendar can be both.

## A CalDAV calendar of an account. `url` is the calendar collection, not the
## server root.
# [calendars.work]
# account = "work"
# url = "https://caldav.example.com/calendars/alice/work/"
## Events written at the same time when this calendar is a target
# concurrency = 2

## A Calendar.app calendar (macOS only). eventkit_calendar takes one of:
##   "A1B2C3D4-..."   the calendar identifier, stable across renames
##   "iCloud/Family"  "Account/Name", as shown in Calendar.app's sidebar
##   "Family"         the name alone; the first calendar with that name wins
## `capycal --discover-eventkit` or "Discover > Calendar.app" in the menu bar
## app lists them.
# [calendars.family]
# eventkit_calendar = "iCloud/Family"

## A read-only calendar feed, only usable as a source: an address returning
## an .ics file, such as Google Calendar's "Secret address in iCal format".
## The address is a secret: it takes every password form shown above.
# [calendars.google]
# ics_url = "https://calendar.google.com/calendar/ical/.../basic.ics"

## --- Syncs ----------------------------------------------------------------
## Each [syncs.<name>] copies events from `source` to `target`. The name shows
## up in logs and the menu bar app. Two syncs in opposite directions are
## fine: events created by Capycal are never copied back.

# [syncs.work-to-family]
# source = "work"
# target = "family"
# interval = "5m"
## Times, recurrence, status and free/busy (DTSTART, DTEND, DURATION, RRULE,
## RDATE, EXDATE, STATUS, TRANSP) are always copied; cancelled events are
## skipped. `fields` lists the content properties to copy on top, such as
## "SUMMARY", "LOCATION", "DESCRIPTION" or "VALARM" for the source's
## reminders. Leave it out for busy blockers without any details.
# fields = ["SUMMARY", "LOCATION"]
## Keep the sync in the config but stop it
# enabled = true
## Copy only the source events of these days around today. all_events is on
## by default, but for Calendar.app sources, which have a window of 30 days
## back and 365 ahead. A recurring event is copied as a whole series if it
## might have an occurrence in the window. For Calendar.app, past plus future
## must be at most 1460 days; all_events reads 20 years back and 4 ahead.
# all_events = false
# days_past = 30
# days_future = 365
## Transforms change the copied events and run in order. Placeholders in
## value and replacement: {{source_name}}, {{source_url}}, {{sync_name}}.
# transforms = [
##  Leave out events. A transform with conditions in `when` runs only if all
##  of them match the event as the transforms above it left it: contains,
##  matches (a regex) or empty test a property, days with start_time and
##  end_time (local time) or all_day test the time. not = true turns a test
##  around, { any = [...] } needs only one of its conditions. Here weekends:
#   { action = "skip", when = [{ days = ["sat", "sun"] }] },
##  A start time after the end time wraps around midnight, here outside working hours
#   { action = "skip", when = [{ days = ["mon", "tue", "wed", "thu", "fri"], start_time = "18:00", end_time = "07:59" }] },
##  All-day events
#   { action = "skip", when = [{ all_day = true }] },
##  Events whose title matches, even if fields drops it
#   { action = "skip", when = [{ field = "SUMMARY", matches = "(?i)private" }] },
##  Travel time only for events at a place, not for video calls
#   { action = "add_buffer", pre_event = "45m", post_event = "30m", when = [{ field = "LOCATION", empty = false }, { field = "LOCATION", contains = "zoom.us", not = true }] },
##  An earlier alarm for events at the airport or at the station
#   { action = "add_alarm", trigger = "2h", when = [{ any = [{ field = "LOCATION", contains = "Airport" }, { field = "LOCATION", contains = "Station" }] }] },
##  Set a property
#   { field = "SUMMARY", action = "set_field", value = "Busy ({{source_name}})" },
##  Find and replace in a property that is in fields ("Lunch with Bob" -> "Lunch (work)")
#   { field = "SUMMARY", action = "replace_field_regex", pattern = ' with .*$', replacement = " ({{source_name}})" },
##  Replace text in a property that is in fields ("Zoom: Weekly" -> "Call: Weekly")
#   { field = "SUMMARY", action = "replace_field", search = "Zoom", replacement = "Call" },
##  Remove a property
#   { field = "LOCATION", action = "remove_field" },
##  Start earlier and end later
#   { action = "add_buffer", pre_event = "15m" },
##  Add a reminder before the start
#   { action = "add_alarm", trigger = "15m" },
# ]
"###;

/// Write the starter config to `path`, refusing to overwrite an existing file.
pub fn write_starter_config(path: &Path) -> CliResult {
    if path.exists() {
        return Err(CliError::Stderr(format!(
            "error: config file already exists at {}\
             \nhint: remove it first or use --config to specify a different path",
            path.display()
        )));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            CliError::msg(format!(
                "could not create directory {}: {e}",
                parent.display()
            ))
        })?;
    }
    std::fs::write(path, STARTER_CONFIG)
        .map_err(|e| CliError::msg(format!("could not write config to {}: {e}", path.display())))?;
    eprintln!("config written to {}", path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn test_starter_config_is_valid_without_syncs() {
        let config: Config = toml::from_str(STARTER_CONFIG).unwrap();
        config.validate().unwrap();
        assert!(config.syncs.is_empty() && config.calendars.is_empty());
    }

    #[test]
    fn test_starter_config_examples_are_valid_when_uncommented() {
        let text: String = STARTER_CONFIG
            .lines()
            .map(|line| format!("{}\n", line.strip_prefix("# ").unwrap_or(line)))
            .collect();
        // EventKit calendars only validate on macOS
        #[cfg(not(target_os = "macos"))]
        let text = text.replace(
            r#"eventkit_calendar = "iCloud/Family""#,
            "account = \"work\"\nurl = \"https://example.com/family/\"",
        );
        let config: Config = toml::from_str(&text).unwrap();
        config.validate().unwrap();
        assert_eq!(config.calendars.len(), 3);
        assert_eq!(config.syncs.len(), 1);
        assert_eq!(config.syncs[0].transforms.len(), 12);
    }

    #[test]
    fn test_write_starter_config_refuses_overwrite() {
        let dir = std::env::temp_dir().join(format!("capycal-{}-generate", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("nested/capycal.toml");

        write_starter_config(&path).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), STARTER_CONFIG);

        let CliError::Stderr(msg) = write_starter_config(&path).unwrap_err() else {
            panic!("expected stderr error");
        };
        assert!(msg.starts_with("error: config file already exists"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
