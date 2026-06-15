# Known Issues

Bugs/limitations discovered (often while writing tests) that were **reported but
not fixed** in the change that found them, with enough analysis to fix later. When
you fix one, remove it here and land a regression test.

> Severity: 🔴 data loss / correctness · 🟠 visible-wrong behavior · 🟡 minor /
> divergence-from-vim · ⚪ cleanup.

## Open

### 🟠 Config merge resets editor settings when a project file omits `[editor]`
`onda-config::merge` takes `overlay.editor` and `overlay.theme` wholesale. Because
both `Config`s are already parsed with `#[serde(default)]`, an *absent* `[editor]`
section is indistinguishable from one full of default values — so a project
`.onda/config.toml` that sets only `theme` silently resets the user's home
`[editor]` settings (tab_width, etc.) to defaults.
- **Fix:** merge at the raw `toml::Value` level (parse both files to `Value`,
  deep-merge the tables, then deserialize once), instead of merging already-defaulted
  structs.
- **Where:** `crates/onda-config/src/lib.rs` `merge()`.

### 🟠 `.`-repeat only covers insert-mode changes
Dot-repeat replays the last *insert* change span (bracketed by
`macros.begin_change()`/`end_change()`). Immediate normal-mode edits — `x`, `dd`,
`dw`, `p`, `r`, `J`, `~`, etc. — are not recorded as changes, so `.` does nothing
after them.
- **Fix:** bracket every mutating normal-mode action as a recorded change (begin on
  entry, end after apply) so the key sequence is captured for replay.
- **Where:** `crates/onda/src/main.rs` `execute_action` (the immediate-edit arms) +
  the macros/dot recording in `handle_key`.

### 🟠 Linewise motions act charwise under an operator (`dj`, `dk`, `dG`, `dgg`)
`ApplyOperatorMotion` always builds a charwise range. Vim treats line motions
(`j`/`k`/`G`/`gg`/`{`/`}` are line-oriented) as **linewise** when combined with an
operator, so `dj` should delete two whole lines, not a charwise span across the line
break.
- **Fix:** classify motions as linewise vs charwise (like the inclusive/exclusive
  split added in `Motion::is_inclusive`) and route linewise operator-motions through
  the linewise delete path.
- **Where:** `crates/onda/src/main.rs` `Action::ApplyOperatorMotion`;
  `crates/onda-modal/src/motion.rs`.

### 🟡 `cw`/`cW` → `ce`/`cE` remap is unconditional
vim only makes `cw` behave like `ce` when the cursor is on a non-blank; on
whitespace `cw` should behave like `dw`. The current remap is unconditional.
- **Where:** `crates/onda/src/main.rs` `Action::ApplyOperatorMotion`.

### ⚪ `onda-contrast` theme is not in the Phase 5 plan
`runtime/themes/onda-contrast.toml` ships, but `PHASE5_PLAN.md` lists only
`onda-dark`, `onda-light`, `onda-wave`. Decide whether to keep it (and add to the
plan) or remove it.

## Fixed (kept briefly for reference)

- 🟠 LSP percent-decode corrupted multibyte (non-ASCII) paths; `initialize`
  root URI wasn't percent-encoded — both fixed.
- 🔴 Single-line `:s` wiped the whole buffer — fixed (splice result into range).
- 🟠 Cursor misplaced after wide/CJK chars — fixed (`char_to_display_col`).
- 🟠 Wide-char ghosting on redraw — fixed (width-0 continuation cells).
- 🟠 Ctrl-r redo shadowed by `r` replace — fixed (modifier guard).
- 🟠 `x`/`dd` ignored count; `dw`/`db` over-deleted; visual-line delete charwise —
  fixed.
- 🟠 `<Enter>`/`o` insert-mode cursor off-by-one — fixed.
