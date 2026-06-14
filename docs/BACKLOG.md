# Backlog

Agent follow-up notes — items deferred from the current phase.

## Completed in Phase 1

Items that were Phase 0 deferrals and landed during Phase 1:

- **Visual-block mode** (`Ctrl-v`): implemented in T6.5 (block selection as multicursor,
  `I`/`A` block insert, `d`/`y`/`p` blockwise).
- **Named registers** (`"a`..`"z`, `"0`..`"9`): implemented in T6.2.
- **Tree-sitter syntax highlighting**: implemented in W5 (T5.1–T5.5).
- **Config file loading** (TOML): implemented in T8.2 (`~/.config/onda/config.toml` +
  project overlay).
- **Undo tree** (non-linear): implemented in T6.1 (branch-on-diverge, `g-`/`g+`).
- **`/` search** and substitute: implemented in T7.1 (incremental regex search) and
  T7.2 (`:[range]s/pat/rep/`).
- **Marks** (`m`, `` ` ``, `'`): implemented in T6.5.
- **Macros** (`q`, `@`): implemented in T6.3.
- **Fuzzy file picker**: implemented in T7.3 (`<space>f`, `<space>b`, `nucleo-matcher`).
- **Window splits**: implemented in T8.1 (`:sp`/`:vsp`, `Ctrl-w` navigation).
- **Jumplist** (`Ctrl-o`/`Ctrl-i`): implemented in T6.5.

## Phase 0 deferrals still open

Items from Phase 0 not yet addressed:

- **Mouse support**: not implemented in Phase 0 or Phase 1. Scheduled for T12.3 (Phase 2).
- **`:e` with completion**: bare path only through Phase 1. T8.3 command completion did
  not land. Scheduled for T12.4 (Phase 2) as a quick-win polish item.
- **Kitty keyboard protocol** full suite: basic crossterm support only. Deferred
  indefinitely — revisit when crossterm adds first-class support.
- **Async progressive file loading** (true streaming): T4.2 has basic async loading;
  true progressive streaming deferred to Phase 3.
- **LSP integration**: deferred to Phase 2 (W10, T10.x).
- **Plugin system**: deferred to Phase 2 (W13, T13.x) for Lua foundation.

## Phase 1 deferrals

Items identified during Phase 1 that were explicitly deferred:

- **Soft/word wrap**: no soft wrap through Phase 1. Horizontal scroll only (T8.3 decision
  recorded in `docs/DESIGN.md` changelog). Scheduled for T12.2 (Phase 2).
- **dylib grammar loading hardening**: T5.1 loads grammars via `libloading` but the
  fresh-machine build experience is rough (no auto-fetch on first open). Partial fix in
  T12.4 (Phase 2 grammar auto-fetch); full prebuilt artifact bundling deferred to Phase 5.
- **Clipboard on Linux/Wayland without a clipboard manager** (`arboard` silent failure):
  identified as friction item F-18 in `docs/DOGFOOD.md`. Threading workaround deferred;
  fix targeted in T12.4 hardening.
- **tree-sitter `textobjects.scm` queries**: T6.4 supports `if/af`, `ic/ac`, `ia/aa` when
  a grammar is loaded but the `.scm` query files were not populated for all bundled
  languages. Remaining query files deferred — add per language as they are validated.
- **File tree (T7.4) full feature set**: T7.4 delivered create/delete/open; rename from
  the tree, multi-select operations, and git status badges deferred to Phase 3.
- **Undo-tree visualization picker**: T6.1 implements the undo tree data structure and
  `g-`/`g+` navigation. A visual picker overlay (like `undotree.vim`) is deferred to
  Phase 3 as a BACKLOG item.
- **`:e` command completion and command-name completion** (T8.3 partial): the picker
  matcher is wired but file-path `<Tab>` completion in the command line did not land.
  Scheduled for T12.4 (Phase 2).
- **Hot-reload config on file change**: `:config-reload` works; automatic watch-and-reload
  via `notify` explicitly deferred. Revisit in Phase 3.

## Notes from Phase 0

<!-- Agents: append here as you work. Format: `- [T0.x] Note about friction/decision.` -->

## Notes from Phase 1

<!-- Agents: append here as you work. Format: `- [T5.x–T9.x] Note about friction/decision.` -->

## Phase 3–5 status audit (post-foundational-implementation)

This records what is **done** vs **outstanding** after the foundational engines and the
audit-gap fixes were implemented. "Engine done" = pure, unit-tested logic crate;
"wired" = usable in the editor.

### Done (this pass)
- **Phase 1 gap fixed**: tree-sitter syntax highlighting now actually paints (the editor
  had no tokio runtime, so the syntax worker never spawned; render discarded highlights).
  Runtime created+entered in `run_editor`; `HlSpan`/`HlCursor` render path; reparse on edit.
- **Git** (T16.1–16.4): status gutter signs, `:GitStatus` picker, `:GitDiff` (scratch
  buffer), `:GitBlame` (current-line float), `:GitStageHunk`/`:GitResetHunk` — all wired.
- **Themes** (T18.1/T30.1): 4 built-ins incl. `onda-wave`, `:theme`, hot-reload,
  `inherits`, Lua `onda.highlight.set`.
- **Tree-sitter text objects** (T18.2), **command-line completion** (T18.3).
- **Data views** (W27/W28): `onda-data` CSV + JSONL engines, wired as `:table` (aligned
  virtual table) and `:fields` (JSONL schema overlay).
- **ACP agent engine** (W22, T24.1/24.3, T25.1): `onda-agent` — protocol/transport/
  session, staging+rebase, permission model, mention assembly; mock-agent E2E.
- **`onda doctor`** (T30.2); **`onda data` engines**; **`cargo xtask install`/`bundle`**
  (T31.1/T19.4); **`BENCH_REPORT.md` v1.0** with real numbers (Phase 0/5).
- **DAP debugger** (Phase 3 W15): `onda-dap` (protocol/transport/session/client + mock
  adapter, 19 tests) wired into the editor — `<F9>` breakpoints + gutter markers,
  `:DapRun`, F5/F10/F11/F12 control, stop marker, `:DapStack`/`:DapVars`/`:DapEval`,
  `dap.toml` + `docs/DAP.md`. Conformance via `onda-mock-dap`; lldb-dap/debugpy are the
  documented real targets (not run in CI). Conditional breakpoints + side panel deferred.

### Outstanding (not implemented — each is large and/or needs external infra)
- **Remote editing `scp://`** (Phase 3 W17): no `russh` transport; needs a live SSH host.
- **libvterm** (Phase 3 W17): terminal still uses `vt100`; vendoring + nvim/tmux/htop
  regression is a large FFI effort.
- **Agent diff review** (Phase 4 T24.2): the agent panel (W23) is wired — streaming
  thread, tool cards, input box, permission prompt (persisted), `@`-mention resolution,
  fs/read from live buffers, `:agent-export`. **Still outstanding:** the hunk-level
  diff-review screen for agent-proposed *writes* (the `StagingArea` engine exists but
  file writes are currently rejected pending review), and live Claude Code (needs the
  `claude-code acp` binary; conformance is via `onda-mock-agent`).
- **Persistent undo** (Phase 5 T29.1): needs serde on core transaction types; default-off
  for v0.1, lowest priority.
- **Release/launch** (W31/W32): clean-machine install matrix, Homebrew tap, docs site,
  signed multi-platform artifacts, `v0.0.3`/`v0.1.0` tags + announcement — not codeable/
  verifiable in a sandbox.
