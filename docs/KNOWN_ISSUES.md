# Known Issues

Bugs/limitations discovered (often while writing tests) that were **reported but
not fixed** in the change that found them, with enough analysis to fix later. When
you fix one, remove it here and land a regression test.

> Severity: 🔴 data loss / correctness · 🟠 visible-wrong behavior · 🟡 minor /
> divergence-from-vim · ⚪ cleanup.

## Open

### 🟠 LSP is not wired into the editor binary
`onda-lsp` (the crate) is complete and tested, but the `onda` binary never spawns a
server: `App.lsp_manager` is always `None`, there is no `ensure_server`/`did_open`/
`did_change` on file open/edit, and no interactive request dispatch (hover, definition,
format, rename, code action, symbols). Diagnostics/format/rename handlers exist but are
dormant because no events ever arrive. This blocks the full W36 LSP UX.
- **Done so far (W36):** a tested, UTF-16-aware edit applier (`lsp_edit`) and wiring so
  `FormattingResult`/`RenameResult` actually apply edits *when* events flow.
- **Needed:** spawn `LspManager` in `run_editor`; on file open call
  `ensure_server` + `did_open`; on edit `did_change` (debounced, with versions); a
  sync→async request-dispatch + `request_id` correlation; bind hover/definition/
  references/format/rename/code-action/document-symbol to keys/commands; then build the
  remaining W36 UX (code-actions menu, document-symbol picker, signature help,
  rename preview, breadcrumb). Needs a live server (rust-analyzer) to validate, so it
  won't be E2E-tested in CI.
- **Where:** `crates/onda/src/main.rs` (LSP lifecycle + dispatch).

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
