# Transforms

Transforms change each event before it is written to the target, or leave it out. They are the `transforms` list of a sync and run in the order they appear, on the whole source event; [`fields`](syncs.md#fields) decides afterwards which properties are copied. Every transform can have [conditions](#conditions), so it only changes the events they match:

> **In the app:** each transform is a step between **Keep** and **Into**. The **+** between two steps adds one there, its menu also offers common steps that come filled in. Each action below names its step.

```toml
[syncs.work-to-home]
source = "work"
target = "home"
interval = "5m"
transforms = [
  { field = "SUMMARY", action = "set_field", value = "Busy" },
  { action = "skip", when = [{ all_day = true }] },
]
```

Each transform is one inline table. The list may span several lines and contain comments (TOML 1.1). The [option reference](../reference/options.md#transforms) also shows the `[[syncs.<name>.transforms]]` table form.

| Action | Effect | Parameters |
|---|---|---|
| [`set_field`](#set_field) | Set a property to a fixed value | `field`, `value` |
| [`replace_field`](#replace_field) | Replace text in a property | `field`, `search`, `replacement`, `count` |
| [`replace_field_regex`](#replace_field_regex) | Find and replace in a property with a regex | `field`, `pattern`, `replacement` |
| [`remove_field`](#remove_field) | Remove a property | `field` |
| [`add_buffer`](#add_buffer) | Start earlier and/or end later | `pre_event`, `post_event` |
| [`add_alarm`](#add_alarm) | Add a reminder | `trigger` |
| [`skip`](#skip) | Leave out the events its conditions match | `when` (required) |

Every action takes `when`, a list of [conditions](#conditions).

The examples on this page show only the `transforms` list; it goes into the table of a sync.

## `set_field`

> **In the app:** **Set field**.

Sets `field` to `value`. An existing property line is replaced as a whole (including parameters such as `LANGUAGE=`); if the event has no such property, it is added. The property doesn't need to be in `fields`.

```toml
transforms = [
  { field = "SUMMARY", action = "set_field", value = "Busy ({{source_name}})" },
]
```

`value` is written as is. For text properties such as `SUMMARY`, escape `,` `;` and `\` as `\,` `\;` `\\` (see [Text escaping](#text-escaping)).

## `replace_field`

> **In the app:** **Replace text**.

Replaces `search` with `replacement` in the value of `field`: every occurrence, or only the first `count` ones. Both are plain text, without regex; `replacement` may contain [placeholders](#placeholders). Like `replace_field_regex`, it needs the property in `fields`, and it sees the escaped value (see [Text escaping](#text-escaping)).

```toml
transforms = [
  # "Zoom: Weekly" -> "Call: Weekly"
  { field = "SUMMARY", action = "replace_field", search = "Zoom", replacement = "Call" },
  # Only the first "-": "2024-01-15 - Review" -> "2024 01-15 - Review"
  { field = "SUMMARY", action = "replace_field", search = "-", replacement = " ", count = 1 },
]
```

## `replace_field_regex`

> **In the app:** **Replace regex**.

Replaces every match of `pattern` in the value of `field` with `replacement`. Only the value is searched, not the property name or parameters. Events without the property are left alone.

The property must be copied, so it has to be in `fields` (or be always kept, or be added by an earlier `set_field`); otherwise the config is rejected, since the transform would do nothing. Keep in mind that events the pattern does not match keep the original value. To hide a property, use `set_field` instead, which replaces it in every event.

```toml
transforms = [
  # "Call with ACME" -> "Call", "Lunch with Bob" -> "Lunch"
  { field = "SUMMARY", action = "replace_field_regex", pattern = ' with .*$', replacement = "" },
]
```

`pattern` uses [Rust regex syntax](https://docs.rs/regex/latest/regex/#syntax); `(?i)` makes it case-insensitive. `replacement` can refer to groups as `$1` or `${name}`; write `$$` for a literal `$`. Use TOML literal strings (single quotes) for patterns, so backslashes need no doubling.

```toml
transforms = [
  # Keep only a ticket number: "Review [OPS-123] rollout" -> "OPS-123"
  { field = "SUMMARY", action = "replace_field_regex", pattern = '^.*\[(?P<ticket>[A-Z]+-\d+)\].*$', replacement = '${ticket}' },
]
```

## `remove_field`

> **In the app:** **Remove field**.

Removes every line of property `field`.

```toml
transforms = [
  { field = "LOCATION", action = "remove_field" },
]
```

Usually it is simpler to leave a property out of `fields`. `remove_field` is useful after a broad `fields` list, to drop something a previous transform added, or to drop an always-kept property such as `TRANSP` (every blocker then shows as busy).

## `add_buffer`

> **In the app:** **Add buffer**.

Moves `DTSTART` earlier by `pre_event` and `DTEND` later by `post_event`. At least one of them is required. All-day events are not changed.

```toml
transforms = [
  { action = "add_buffer", pre_event = "30m", post_event = "15m" },
]
```

For events stored with `DURATION` instead of `DTEND`, the duration grows by `pre_event` plus `post_event`. In a recurring series, the dates that name single occurrences (`EXDATE`, `RDATE`, `RECURRENCE-ID`) move with `DTSTART`, so excluded and changed occurrences still match. Buffers are real time: a 30 minute buffer before an event at 03:00 on the night clocks spring forward starts at 01:30, not at the skipped 02:30, as long as Capycal knows the event's timezone: from its definition in the event (`VTIMEZONE`, which CalDAV servers include) or, for IANA names such as `Europe/Berlin` (Calendar.app sources), from the built-in timezone database.

## `add_alarm`

> **In the app:** **Add alarm**.

Adds a display alarm ("Reminder") `trigger` before the start. Repeat the transform for several alarms.

```toml
transforms = [
  { action = "add_alarm", trigger = "15m" },
  { action = "add_alarm", trigger = "1m" },
]
```

Alarms from the source are only copied with `"VALARM"` in `fields`; `add_alarm` adds to those. See [Travel buffers and reminders](../cookbook/buffers-and-reminders.md).

## `skip`

> **In the app:** **Skip events**. It starts with one condition and keeps at least one.

Leaves out the events that match all its conditions. Later transforms don't run for them, and a copy synced earlier is deleted from the target. `skip` without `when` is a config error, since it would skip every event.

```toml
transforms = [
  # Leave out weekend events
  { action = "skip", when = [{ days = ["sat", "sun"] }] },
  # Leave out all-day events
  { action = "skip", when = [{ all_day = true }] },
  # Leave out events with "private" in the title, in any case
  { action = "skip", when = [{ field = "SUMMARY", matches = "(?i)private" }] },
  # Leave out events without a location
  { action = "skip", when = [{ field = "LOCATION", empty = true }] },
]
```

Since transforms see the whole event, a condition can look at a property the sync does not copy: a busy blocker without `fields` can still leave out events by their title.

Recipes: [Weekends, nights and business hours](../cookbook/time-filters.md).

## Conditions

> **In the app:** open a step and press **Add Condition**. The first condition reads **Only if** (**If** for **Skip events**), the others **if**, after a switch between **and** and **or** that joins them to the one above. Each has a popup for what it looks at (**Title**, **Location**, **Notes**, **Other field**, **Start**, **Event**), one for the test, and its text, days or times; drag it by its handle (≡) to another place, or use the arrows (**Move Up**, **Move Down**) to swap it with its neighbour; **Remove** takes it away.

`when` is a list of conditions. A transform with conditions only changes an event when all of them match; without `when` it changes every event.

```toml
transforms = [
  # 45 minutes of travel time, only for events at a place that is not a video call
  { action = "add_buffer", pre_event = "45m", post_event = "30m", when = [
      { field = "LOCATION", empty = false },
      { field = "LOCATION", contains = "zoom.us", not = true },
  ] },
]
```

### One of several

> **In the app:** switch the **and** between two conditions to **or**. Conditions joined by **or** share a box, **One of these**; switching back to **and** splits it. To join conditions that are not next to each other, drag one into the box: dropped inside it, at its top, between two of its conditions or at its bottom, it joins them with **or**. Dropped outside a box, right above or below one included, it comes with **and**.

`{ any = [...] }` in `when` is a group of conditions of which one is enough. It stands for one condition among the others, which still all have to match:

```toml
transforms = [
  # An earlier alarm for events at the airport or at the station
  { action = "add_alarm", trigger = "2h", when = [{ any = [
      { field = "LOCATION", contains = "Airport" },
      { field = "LOCATION", contains = "Station" },
  ] }] },
  # Leave out weekday events that are all-day or marked as out of office
  { action = "skip", when = [
      { any = [{ all_day = true }, { field = "SUMMARY", contains = "OOO" }] },
      { days = ["mon", "tue", "wed", "thu", "fri"] },
  ] },
]
```

- `any` holds at least two conditions, takes no other keys and cannot hold another `any`.
- For "this and that, or something else", use two transforms: two `skip`s leave out what either matches. Steps that would add twice, such as `add_alarm` or `add_buffer`, need `any` instead.

### Tests

Each condition has exactly one test:

| Test | Keys | Matches when | In the app |
|---|---|---|---|
| contains | `field`, `contains` | the value contains the text, case matters | **contains**, **does not contain** |
| matches | `field`, `matches` | the value has a match of the regex | **matches regex**, **does not match regex** |
| empty | `field`, `empty = true` | the property is missing or empty; `empty = false`: it has a value | **is empty**, **is not empty** |
| time | `days`, optional `start_time`, `end_time` | the event starts on one of the days, within the times | **Start** with **is on**, **is not on**, the days, **From** and **Until** |
| all-day | `all_day = true` | the event is all-day (`DTSTART` with `VALUE=DATE`); `all_day = false`: it is not | **Event** with **is all-day**, **is not all-day** |

- `not = true` turns contains, matches and time around. `empty` and `all_day` say it with their value, so `not` is an error there.
- `field` is an iCal property name in upper case such as `"SUMMARY"` (**Title**), `"LOCATION"` (**Location**) or `"DESCRIPTION"` (**Notes**); any other is **Other field**. Only contains, matches and empty take a `field`.
- A property that appears several times, such as `ATTENDEE` or `CATEGORIES`, matches when any of its lines does; "does not contain" means none of them does, and "is empty" that all are empty.
- A missing property counts as an empty value: `contains` doesn't match it, `matches = "^$"` does.
- The value is unescaped first (see [Text escaping](#text-escaping)), so `Lunch, Room 5` matches as written.
- `matches` uses [Rust regex syntax](https://docs.rs/regex/latest/regex/#syntax) and looks for a match anywhere in the value; use `^` and `$` for the whole value and `(?i)` to ignore case.
- Conditions look at the series itself (the `VEVENT` without `RECURRENCE-ID`). A transform whose conditions match changes, or skips, the whole resource, so a single occurrence moved into or out of a time range follows its series. The same holds for its text: a single occurrence renamed to "Doctor" is not checked against a condition on `SUMMARY`, only the series title is.

### Time conditions

`days` are `"mon"`, `"tue"`, `"wed"`, `"thu"`, `"fri"`, `"sat"`, `"sun"` (lowercase). `start_time` and `end_time` are `HH:MM` and both inclusive at minute precision; they default to `"00:00"` and `"23:59"`, so without times the whole day matches.

```toml
transforms = [
  # Leave out events starting after 18:00 on weekdays
  { action = "skip", when = [{ days = ["mon", "tue", "wed", "thu", "fri"], start_time = "18:00" }] },
  # Leave out events outside working hours: before 08:00 or from 18:00 on weekdays
  { action = "skip", when = [{ days = ["mon", "tue", "wed", "thu", "fri"], start_time = "18:00", end_time = "07:59" }] },
]
```

- Times are in the timezone of the machine running Capycal. The event's start is converted from UTC or its `TZID` first; floating times are taken as local.
- Only the start counts. An event from 17:00 to 19:00 does not match `start_time = "18:00"`.
- A `start_time` after the `end_time` wraps around midnight: `start_time = "22:00"`, `end_time = "05:59"` matches starts from 22:00 to midnight and from midnight to 05:59 on each of `days`. Both parts belong to the day the event starts on: with `days = ["fri"]`, Friday 23:00 matches, Saturday 01:00 doesn't.
- All-day events have no start time: they match a time condition without times on their weekday, and never one with `start_time` or `end_time`, not even with `not = true`. To match them, add a condition with `all_day`.
- The desktop app's common step **Skip outside working hours** fills in Mon-Fri, `18:00` to `07:59`; **Skip weekends** fills in Saturday and Sunday without times.

## Placeholders

`value` (of `set_field`) and `replacement` (of `replace_field` and `replace_field_regex`) may contain:

| Placeholder | Value |
|---|---|
| `{{source_name}}` | Key of the source calendar, e.g. `work` for `[calendars.work]` |
| `{{source_url}}` | URL of a CalDAV source, `eventkit://local` for Calendar.app, `ics://feed` for a feed (its address is a secret) |
| `{{sync_name}}` | Name of the sync (its table key) |

An unknown placeholder is a config error. Placeholder values are inserted without escaping.

## Order

Transforms run top to bottom on the whole source event, each on the result of the previous one. `fields` is applied after the last one:

- A `skip` leaves the event out as soon as its conditions match; later transforms don't run.
- Conditions see the event as the transforms above them left it: a time condition sees the start moved by an earlier `add_buffer`, a condition on `SUMMARY` the title set by an earlier `set_field`. Put a transform above the others to look at the original values. A condition on a property that an earlier `set_field` or `remove_field` without conditions already fixed would always give the same answer, so it is a config error that names the transform to move.
- A property a `set_field` sets is copied, even if it is not in `fields`, but only in the events its conditions matched. In the others the property stays out unless it is in `fields`, so a title that should have become "Busy" is not copied from the source either.
- `replace_field` and `replace_field_regex` change only properties that are copied, so their property has to be in `fields` (or be set by an earlier `set_field`).
- `add_alarm` checks its conditions at its place but adds its alarm after `fields`, so the source event's own alarms are only copied with `"VALARM"` in `fields`.

## Scope

- `set_field`, `replace_field`, `replace_field_regex`, `remove_field` and `add_buffer` change the event's own properties in every `VEVENT` of the resource. A recurring CalDAV series stores modified occurrences as extra `VEVENT`s, so a `set_field` of `SUMMARY` covers them too. Alarms (`VALARM`) and timezone definitions (`VTIMEZONE`) are never touched; `add_alarm` adds an alarm to every `VEVENT`.
- Conditions look at the series itself (the `VEVENT` without `RECURRENCE-ID`); a `skip` keeps or leaves out the whole resource.
- An event that was synced earlier and is skipped later (for example, moved to a weekend) is deleted from the target, the same way as an event deleted in the source.

## Text escaping

Transforms work on the raw iCal values, where text is escaped as in RFC 5545 (except [conditions](#conditions), which match the unescaped value). This also applies to Calendar.app sources: their `SUMMARY`, `LOCATION` and `DESCRIPTION` are escaped before transforms run. A regex or a `search` of `replace_field` therefore has to match `\,`, `\;` and `\\` instead of `,`, `;` and `\`, and a `value` or `replacement` should be escaped the same way. A line break in a description is `\n`.

To rewrite `Lunch, Room 5` (stored as `Lunch\, Room 5`) to `Lunch`:

```toml
transforms = [
  # pattern is a TOML literal string; as a basic string: "\\\\, Room \\d+"
  { field = "SUMMARY", action = "replace_field_regex", pattern = '\\, Room \d+', replacement = "" },
]
```
