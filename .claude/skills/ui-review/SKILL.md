---
name: ui-review
description: Check a change to the Capycal desktop apps (capycal-ui on macOS, capycal-gtk on Linux) against the UI and UX guidelines before it is committed. Use after any change to a window, a dialog, the menu or the texts they show, and when asked to review UI or UX.
---

# UI review

Review the current change (`git diff` against the last commit, or the branch against `main` when asked) against `docs/src/development/ui-guidelines.md`. Read that page first, every time: it is the rule book and it changes.

## 1. Find what the change touches

- Models: `src/bin/capycal-ui/{settings_view,settings,sync_form,transform_form,account_form,picker,preview,menu,dialog,problems,purge,sponsor,logview}.rs`
- macOS renderers: `src/bin/capycal-ui/*_macos.rs`, `app.rs`
- GTK renderers: `src/bin/capycal-gtk/*.rs`
- User docs: `docs/src/app/*.md`, the `> **In the app:**` lines in `docs/src/concepts/` and `docs/src/cookbook/`

## 2. Go through the checklist

For each item, look at the diff and the code around it. Note every miss with file and line.

- [ ] **Model first**: new logic, texts and states are in a display-free model with unit tests, not in a renderer.
- [ ] **Both platforms**: every control, text or behaviour added to one renderer is in the other too.
- [ ] **Layout**: gaps only from the spacing scale (`GAP_S` 4, `GAP_M` 8, `GAP_L` 12, `GAP_XL` 20); labels in the label column; new content in a `Stage` or `Group`, lined up with the steps; nothing shifts when a state changes.
- [ ] **Color**: accent for chosen and active, orange with a warning sign for problems, secondary text for notes; no red anywhere, Delete included; meaning never only by color.
- [ ] **Text**: buttons and menu items in title case and saying what happens, "…" when they open a window or ask; everything else in sentence case; "…" not "...", no em or en dashes, no "--"; errors say what to do and name the field by its label; user words ("target", "Field"), not code words.
- [ ] **Feedback**: a field's mistake below the field once the user leaves it; what keeps Save off in the footer; status of a saved sync under its name.
- [ ] **Interaction**: one way to do a thing; undo instead of a question for small reversible changes, a question only before data is lost; shortcuts only while their button is enabled; everything reachable without a mouse (a context menu for what a drag does); presets and filled-in defaults before "Custom".
- [ ] **Motion**: only in answer to an action, about 0.2 s, none with Reduce motion.
- [ ] **Accessibility**: every control without a title has an accessibility label (macOS `accessibility_label`, GTK `update_property(&[Property::Label(..)])`); Tab order follows the screen.
- [ ] **Docs**: the user docs describe the change; a new pattern is added to the guidelines.

## 3. Run the checks

```sh
make test-ui                      # includes settings_view::tests::ui_rules
make clippy-ui
make test-gtk && make clippy-gtk  # where the GTK libraries are available
cargo clippy --target aarch64-apple-darwin --all-targets --features ui -- -D warnings  # macOS type-check, see CLAUDE.md
make docs
```

A rule the tests can check but do not yet (a new banned word, a new kind of text): add it to `ui_rules` in `src/bin/capycal-ui/settings_view.rs`.

## 4. Look at it

Screenshots, not only code: the macOS app (on a Mac or the test VM) and the GTK app (under Xvfb), each in light and dark, in the normal state, with a mistake in a field, while saving, and through the keyboard path. Compare with the guidelines, especially alignment, spacing and that nothing moves when a state changes.

## 5. Report

List the misses as `path:line: rule: what to change`, most important first, then what you checked and found fine in one line. Fix the misses if asked to; re-run steps 3 and 4 after fixing.
