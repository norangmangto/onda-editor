# Known Issues

Bugs/limitations discovered (often while writing tests) that were **reported but
not fixed** in the change that found them, with enough analysis to fix later. When
you fix one, remove it here and land a regression test.

> Severity: 🔴 data loss / correctness · 🟠 visible-wrong behavior · 🟡 minor /
> divergence-from-vim · ⚪ cleanup.

## Open

### ⚪ Syntax highlighting uses node-kind→scope heuristics, not highlight queries
The highlighter maps tree-sitter node **kinds** to `Scope` via hand-written per-language
`*_scope()` functions (`crates/onda-syntax/src/highlight.rs`), not `.scm` highlight
queries. Coverage is therefore approximate per language — it colors keywords/strings/
numbers/comments/types well, but misses query-only distinctions (e.g. function *call* vs
definition, injected languages, contextual captures). Migrating to compiled
`highlights.scm` per language (`HighlightConfig` is the placeholder) would improve fidelity.

### ⚪ CSV/TSV tinting is naive (no quoted-field handling)
`csv_highlights` splits on the raw delimiter and does not honour RFC-4180 quoted fields —
a `,` inside `"..."` still starts a new column tint. Adequate for visual column
separation; structured CSV work uses the `:table` view.
- **Where:** `crates/onda-syntax/src/highlight.rs` `csv_highlights`.

### ⚪ Dockerfile highlighting not yet bundled
Shell, Makefile, Go, JS, TS, HTML, CSS were added alongside HCL/Markdown, but the
`tree-sitter-dockerfile` crate lags the stable-ABI layer (latest 0.2.0, pre-`LanguageFn`);
deferred until a compatible release. Registry detection (filename-based, like Makefile)
can be added when the grammar is wired.

### 🟡 LSP: full-document `didChange` sync only (no incremental)
`LspClient::did_change` sends the whole buffer text on every debounced flush, not an
LSP-incremental range edit. DESIGN.md's "디바운스된 증분 didChange" calls for incremental
sync; onda has the debounce (250ms quiet-period, `App::maybe_flush_lsp_change`) but not
the incremental part — that needs `ChangeSet` → LSP-range translation. Fine for typical
file sizes; would matter for very large open buffers with a slow server round-trip.
- **Where:** `crates/onda-lsp/src/client.rs` `did_change`; `crates/onda/src/main.rs`
  `send_lsp_did_change`.

### 🟡 LSP: command-only code actions are dropped
`CodeActionOrCommand::Command` variants (a server-side command with no `edit`) are
filtered out of the `<space>ca` picker — only edit-based actions apply. Executing
arbitrary `workspace/executeCommand` requests is unimplemented.
- **Where:** `crates/onda-lsp/src/client.rs` `parse_code_actions`.

### ⚪ LSP: server list is hardcoded (rust-analyzer + gopls only)
`LspManager::new` hardcodes its two configs; there's no `languages.toml`-driven,
per-language server command/args/root-marker config (PHASE7_PLAN.md T41.1/T42.2 already
plans this — basedpyright/ruff, typescript-language-server, clangd, taplo, etc.).
- **Where:** `crates/onda-lsp/src/manager.rs`.

### ⚪ LSP: signature help, rename preview, and breadcrumb are unimplemented
Explicitly called out as "remaining W36 UX" beyond the base wiring; hover/definition/
references/rename/format/document-symbol/code-action are all wired (W36 core), but
these three UI affordances aren't started.

### ⚪ Soft wrap: character-boundary only, and some overlays aren't wrap-aware
`:set wrap` wraps at the display-width boundary, not word boundaries (no greedy
word-wrap). Plugin decorations (highlights/signs/virtual text) and debugger gutter
markers still assume the unwrapped 1:1 doc-line-to-screen-row mapping — they'll
misplace on a wrapped line. The core text/diagnostics/cursor path is wrap-aware
(`onda_render::{build_row_layout, locate_in_layout}`); these overlays are not yet.
- **Where:** `crates/onda/src/main.rs` (`draw_plugin_highlights`/`draw_plugin_signs`/
  `draw_plugin_virt_text` and `draw_dap_markers`, all keyed off
  `viewport.offset_line + row` directly).

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
- 🟠 `<space>f` (file picker) and any multi-key sequence ending in `f`/`t`/`r`/`q`/
  `@`/`"`/`m`/`` ` ``/`'` was unreachable — the single-char pending-key check
  (f/t/F/T-find, r-replace, q-macro, etc.) ran unconditionally on every keystroke,
  hijacking the 2nd+ key of an in-progress trie sequence before the trie ever saw it
  — fixed (guarded on `pending_keys.is_empty()`; also unblocked the new `gr` binding).
- 🟠 LSP not wired into the editor binary — fixed: `LspManager` spawns at startup
  (bridged to `BgMessage::Lsp`), every buffer-open path calls `ensure_server`+
  `did_open`, edits flow through a debounced `did_change`, and
  hover/definition/references/rename/format/document-symbol/code-action are bound to
  keys and commands.
