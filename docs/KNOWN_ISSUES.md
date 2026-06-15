# Known Issues

Bugs/limitations discovered (often while writing tests) that were **reported but
not fixed** in the change that found them, with enough analysis to fix later. When
you fix one, remove it here and land a regression test.

> Severity: 🔴 data loss / correctness · 🟠 visible-wrong behavior · 🟡 minor /
> divergence-from-vim · ⚪ cleanup.

## Open

### ⚪ `onda-contrast` theme is not in the Phase 5 plan
`runtime/themes/onda-contrast.toml` ships, but `PHASE5_PLAN.md` lists only
`onda-dark`, `onda-light`, `onda-wave`. Decide whether to keep it (and add to the
plan) or remove it. (Plan discrepancy / cleanup, not a code bug.)

### 🟡 Visual-mode `.`-repeat
Dot-repeat now covers immediate operator edits and insert changes, but not visual
selections (intermediate selection-building motions clear the in-progress change).
Vim's visual dot has its own "same-size" semantics; revisit if needed.
- **Where:** `crates/onda/src/main.rs` `finalize_dot`.

## Fixed (kept briefly for reference)

- 🟠 Config merge reset editor settings when a project file omitted `[editor]` —
  fixed (raw-`toml::Value` deep-merge).
- 🟠 `.`-repeat covered nothing (record_change_key was never called) — fixed:
  rebuilt on a per-command change buffer + a Document `rev` counter; now repeats
  `x`/`dw`/`dd`/insert changes.
- 🟠 Linewise operator motions (`dj`/`dk`/`dG`/`dgg`) acted charwise — fixed
  (`Motion::is_linewise` + linewise routing; keymap `dgg` support).
- 🟡 `cw`/`cW`→`ce`/`cE` was unconditional — fixed (only on a non-blank).
- 🟠 LSP percent-decode corrupted multibyte (non-ASCII) paths; `initialize`
  root URI wasn't percent-encoded — both fixed.
- 🔴 Single-line `:s` wiped the whole buffer — fixed (splice result into range).
- 🟠 Cursor misplaced after wide/CJK chars — fixed (`char_to_display_col`).
- 🟠 Wide-char ghosting on redraw — fixed (width-0 continuation cells).
- 🟠 Ctrl-r redo shadowed by `r` replace — fixed (modifier guard).
- 🟠 `x`/`dd` ignored count; `dw`/`db` over-deleted; visual-line delete charwise —
  fixed.
- 🟠 `<Enter>`/`o` insert-mode cursor off-by-one — fixed.
