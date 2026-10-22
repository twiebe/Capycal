# UI and UX guidelines

These rules hold for the desktop apps: the macOS menu bar app (`capycal-ui`) and the Linux app (`capycal-gtk`). Every change to a window follows them; a change that needs a new pattern adds it here first. `/ui-review` checks a change against this page, and the tests in `settings_view::tests::ui_rules` catch the rules a machine can check.

## Principles

- **The model decides, the renderers draw.** Logic, texts and states live in the display-free models (`settings_view.rs`, `sync_form.rs`, `transform_form.rs`, `menu.rs`, `dialog.rs`, ...) with unit tests. `settings_window_macos.rs` and the GTK windows only draw a view and report input.
- **Both platforms, always.** A control, a text or a behaviour added to one renderer is added to the other in the same change. Where a platform lacks something (GTK cannot grey out dropdown items), the model copes, not the user.
- **Calm by default.** Nothing blinks, nothing is red. Attention goes to the one thing that needs it: the accent marks what is chosen or active, orange marks a problem.
- **One way to do a thing.** A task has one entry point (a sync is added through the assistant, which has a way out to the editor). Similar tasks start from what exists (Duplicate), not from an empty form.
- **Show the effect.** The preview shows what a sync does to an event, the header says how the sync is doing. Prefer showing over explaining.

## Layout

- **Spacing scale**, on a 4 point grid: `GAP_S` 4, `GAP_M` 8, `GAP_L` 12, `GAP_XL` 20 (`settings_window_macos.rs`; GTK uses the same values in pixels). No other gaps.
- **Labels** sit left of their control in a column of `LABEL_WIDTH`, left-aligned. A row of short fields (`Control::Row`) puts each field's label after it ("30 days back").
- **Boxes**: a sync is a pipeline of steps (`Stage`: From, Keep, one per transform, Into), each a rounded box without border, joined by a `Joint` whose "+" adds a step there. Other content goes in `Group` boxes. Header, groups and the Advanced pill line up with the steps (the gutter column on the right holds only drag handles).
- **Corners follow where a control stands**: what stands on its own line or ends a part of the window is a pill, like the buttons of a dialog: the footer buttons, a row of buttons (`Control::Buttons`, e.g. Add Condition, Test Connection), the and / or switch, Sample / Next event above the preview, the Advanced pill (macOS `NSControlSize::Large`, GTK `capycal-round` / `capycal-join`). What sits in a form row next to a field keeps the field's corners: popups, text fields, the buttons beside a popup, segments with a label (Check every, Keep). Boxes (steps, groups, cards) keep their own rounded corners.
- **The footer** is the save bar: Delete on the left; on the right a text, Revert and Save (the default button). The text says what is unsaved, what is saving, or what keeps Save off. Its buttons sit centered between the hairline above and the window's edge (`MARGIN` on macOS, 12 px in GTK); the content scrolls up to the hairline.
- **No Liquid Glass of our own.** macOS 27 puts glass only on toolbar controls; sidebars are full height again (the floating glass sidebar was macOS 26), sheet footers are flat with a hairline, and buttons in content are plain bezels with the default one filled in the accent color. Settings has no toolbar, so it uses no `NSGlassEffectView`: no floating sidebar, no glass capsule around the footer, no glass bezels. A control AppKit draws in glass by itself stays as it comes. Check how macOS's own apps draw a pattern before adopting one.
- **The aside** holds the preview, a column of its own.
- **Sidebar** rows are source-list rows: icon, name, a dot for unsaved changes. "Add …" rows have an accent "+". The app's version sits small and in secondary text at the bottom of the sidebar, level with the footer.
- **Settings is the one place to set things.** The menu bar menu holds actions (Sync Now, Settings…, About, Quit), not settings, not even switching a sync off; repairs are in Settings too, next to what they repair (a sync's Advanced settings > Troubleshooting); the app's own settings, the config file and the log are under **General** at the top of the Settings sidebar.

## Color

| Use | macOS | GTK |
|-----|-------|-----|
| Chosen, active, the "+" | `controlAccentColor` | `@accent_bg_color` / `@accent_color` |
| A problem (warning sign, sidebar icon) | `systemOrangeColor` | `#e66100` |
| Secondary text (notes, captions, status) | `secondaryLabelColor` | `dim-label` |
| Inactive (disabled sync, chevrons) | `systemGrayColor`, `tertiaryLabelColor` | `alpha(@window_fg_color, …)` |

- **No red**, not even for Delete: the question before deleting is the warning. Delete still reports a destructive action to accessibility (`setHasDestructiveAction`).
- A problem's text is in the normal text color, after an orange warning sign. Never colored text.
- Account and sync icons use the accent alike.

## Text

- **Buttons and menu items** use title case and say what happens ("Set Up in Editor", "Move Down"). One that opens a window or asks first ends with "…" ("Add Sync…").
- **Everything else** uses sentence case: labels, notes, status lines, errors.
- **A menu of things to add says what each does** in a second, secondary line under its title (`PopupItem::hint`: macOS `NSMenuItem.subtitle`, a tooltip before macOS 14.4; GTK a dim caption label and the accessible description). Keep it to one line, about 50 characters, starting with a verb ("Leaves out events on Saturday and Sunday").
- **Ellipsis** is "…" (U+2026), never three dots, also for progress ("Saving…", "Syncing now…").
- **No dashes as punctuation**: no em or en dash, no "--". Use a colon, a comma or a new sentence.
- **Errors say what to do**: "Set field (step 1): fill in Field and Value", "Use a duration such as 15m or 1h 30m". Name the field by its label. No "invalid", no "error:" prefix, no blame.
- **Name things by what the user sees**: "Field" not "property", "target" in user text (the code says "sink").
- A placeholder in a summary reads as "…" or "a field" ("Set a field to …"), never as empty quotes.

## Feedback and errors

- A field's mistake shows **below the field**, with the warning sign, **once the user leaves the field** (`SettingsInput::Left`), not while typing.
- What keeps Save off shows **in the footer**, next to Save, with the warning sign. Save stays disabled meanwhile.
- **Long work shows its progress** where it was started: what is happening and how far ("Removing the synced events of "work"… 40 of 120") with a bar, counted first so the bar has an end. A save that removes copies shows it in the footer, so does a full sync of the shown sync (after a save that changed what it copies, or Full Sync Now), the Remove Synced Events dialog in its window.
- A saved sync's **status line** under its name says how it is doing ("Synced 2 min ago: 3 new, 1 removed"); a failed sync shows the warning sign and turns its sidebar icon orange.
- Long texts are cut off with "…" and are complete in the tooltip.

## Interaction

- **Undo over questions.** Small, reversible changes (adding, moving, removing a step) happen at once and Cmd+Z takes them back. Ask only before data is lost (deleting a sync, discarding a draft, removing copies).
- **Deleting what Capycal did not create takes two steps**: a warning in an orange box (`dialog::View::warning`, bold, with the warning sign) saying what else goes, then a second warning where the user types the name shown in Settings (`View::entry`) before the destructive button enables (`View::last_disabled`). Remove All Events is the only such action.
- **Keyboard**: Cmd+S saves, Cmd+N adds a sync, Cmd+Delete deletes, Cmd+Z undoes (outside a field), Cmd+Option+Up / Down moves the open step; Ctrl instead of Cmd on Linux. Each only while its button is enabled. Esc or a click on the background ends typing in a field. Tab follows the order on screen.
- **Nothing only by mouse**: what a drag does, a context menu does too (Move Up, Move Down).
- **Suggestions, not restrictions**: a field with common values offers them (field names) and takes any other; field names are upper case as they are stored.
- **An assistant opens over its window**, not in place of it: the new-sync wizard is a sheet on Settings (macOS) or a modal window (GTK), so what was shown stays in view and Cancel returns to it (`SettingsScreen::wizard_view`).
- **Defaults that work**: presets first (interval 1 / 5 / 15 min, what to copy), "Custom" for the rest. Common steps come filled in. An assistant step asks one thing; a page says that everything can be changed later.
- **The way out comes with the problem**: a problem of the whole config shows at the top of General (**Needs attention**, orange General entry) with the one button that fixes it; Settings opens even for a file it cannot read, with only General, so the way to fix it is never closed.
- **Fixes belong to what they fix**: an action on one sync (full sync, removing its copies) sits on its page under Advanced settings > Troubleshooting, not only in a menu with a picker.
- **Something that is not listed is added where it is chosen**: a choice menu ends with an action for what it cannot list ("Add Account…", "Add Calendar Feed…"), and what that needs appears right below it as a field (**Feed address**), not in another window. A secret the config keeps elsewhere (`env`, `keychain`) shows as a disabled field saying so.
- **Conditions read as a sentence down the form**: below a step's own fields, each condition is a popup of what it looks at labelled **Only if** (**If** for Skip events), then **if**, with **Move Up** and **Move Down** (arrows, where there is a neighbour) and **Remove** as its buttons, then the test popup and only the inputs that test needs (text, weekday checkboxes, From / Until); **Add Condition** ends the list. A step that needs a condition starts with one and cannot lose its last.
- **A join is a switch between the parts it joins**: between two conditions an **and** / **or** `Segments` control without a label sits centered on its own line (`centered`). Conditions joined by **or** share a thin outlined box (`Control::AnyOf`, caption **One of these**) inside the step, so the group shows by its frame and its words, not by color. Each condition is a card (`Control::Condition`, a light fill, no border) with a drag handle; the model places a drop mark (`Control::DropMark`) at each place it can land, two at the edge of a box (inside at its top or bottom, and outside), and the renderers snap to the mark nearest the pointer, none where the dragged one would stay as it is. Dropped inside a box it joins it with **or**, outside with **and**; its Move Up / Move Down buttons, small arrows with their titles as tooltip and accessibility label so moving works without a mouse, swap it with its neighbour and leave the joins where they are. A condition drag carries a different value type than a step drag (GTK `u64` vs `u32`), so neither lands in the other's target. The join switch is a pill like the dialog buttons (macOS `NSControlSize::Large`, GTK `capycal-join`). A rebuild keeps the scroll position of the same entry (GTK drops the focus first, else it scrolls to the first field). An update in place leaves a focused field alone, except after a move (`SettingsView::moves` changed): the field then shows another condition and takes its text.
- **Tags** on a list row ("the source", "read-only") are small pills in secondary text, centered on the row.
- **The window remembers** its size and the entry shown.
- **A menu bar app with a window behaves like an app**: on macOS it shows in the Dock, in Cmd+Tab and with its own menu bar (app menu with Settings… and Quit, Edit, Window) while any of its windows is open, and leaves the Dock with the last one. Cmd+W closes, Cmd+Q quits (Ctrl on Linux), asking only about unsaved changes.
- **Ask for permissions in context**: say why before the system asks (Calendar.app access). Before a Keychain prompt (`keychain_notice.rs`), check silently whether one will come and only then explain what Capycal reads, what stays closed and which button keeps the system from asking again.

## Motion

- Motion answers an action and shows what changed: a chevron turns (0.2 s, ease in and out), opened fields fade in, the worked-in step gets a ring. Nothing moves by itself.
- With "Reduce motion" on (macOS), changes happen without animation.
- A change of state never changes the size of what is next to it (fixed boxes for icons that change).

## Accessibility

- Every control without a title has an accessibility label that says what it is and what it is about ("Remove step 2", "Step 2: Add buffer: 15 min before, closed").
- Information is never only color: a problem has a sign and words, a draft a dot and the footer text.

## Checking a change

1. The model change has unit tests; `ui_rules` passes (`make test-ui`, `make test-gtk`).
2. Both renderers draw it: macOS type-checks (`clippy --target aarch64-apple-darwin --features ui`), GTK builds and passes clippy.
3. Look at it: screenshots of the macOS app (in the VM) and the GTK app (under Xvfb), light and dark, including a problem state and the keyboard path.
4. The user docs (`app/`, and the `In the app` lines in `concepts/` and `cookbook/`) describe what changed.
