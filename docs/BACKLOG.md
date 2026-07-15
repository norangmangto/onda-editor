# Backlog

Agent follow-up notes — items deferred from the current phase.

## Plan reconciliation (2026-06-15) — current source of truth

The codebase was realigned to the approved plans (PHASE0–5 + DESIGN v0.3 + AGENTS.md).
Where older sections below still describe removed features as "done", **this section
wins**:

- **DAP debugger (`onda-dap`) — removed for v0.1, restored for v0.2.** It was cut from
  the v0.1 scope, then reinstated as a **core feature crate** in Phase 6 (W40) after the
  Croft benchmarking — the LSP twin (external adapter + framed protocol + editor
  integration). Crate, wiring, `dap.toml`, `docs/DAP.md`, and the dap bench gate are back.
- **Built-in git (`onda-git`) — removed from core.** Returns in Phase 3 as the
  `git-blame-inline` WASM reference plugin (ADR-002). diff/blame logic salvaged to
  `docs/SALVAGE/onda-git/`. `git2` remains only for `onda plugin install` (repo clone).
- **Lua plugins (`onda-lua`) — removed.** Replaced by the WASM Component Model system
  (`onda-plugin`), per ADR-002. Host API surface salvaged to
  `docs/SALVAGE/lua-api-surface.md`; old `docs/PLUGIN_API.md` deleted (see `wit/onda/`).
- **Text objects — narrowed to Rust + Python.** The feature (PHASE1 T6.4) stays; the
  Go/C/TypeScript grammars and their `textobjects.scm` were dropped to match the bundled
  language set (rust/python/json/toml).
- **Command-line completion — kept** (PHASE1 T8.3), unchanged.
- **`onda-agent` / `onda-data` — kept** (Phase 4 / Phase 5), fully implemented.

ADRs (`docs/DESIGN.md`) are user-owned: product ADR-001..009 + implementation ADR-101+;
agents must not change them without explicit approval (AGENTS.md rule 3).

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
- **LSP integration**: `onda-lsp` (W10 core: transport/lifecycle/document-sync/
  request-manager/server-registry) landed in Phase 2. Binary wiring (spawn on startup,
  `ensure_server`+`did_open` on file open, debounced `did_change`, and
  hover/definition/references/rename/format/document-symbol/code-action bound to keys
  and commands) landed later (Phase 6 W36) — see `docs/KNOWN_ISSUES.md` Fixed section.
  Remaining gaps (incremental sync, command-only code actions, config-driven server
  list, signature help/rename-preview/breadcrumb) are tracked in `docs/KNOWN_ISSUES.md`.
- **Plugin system**: deferred to Phase 2 (W13, T13.x) for Lua foundation.

## Phase 1 deferrals

Items identified during Phase 1 that were explicitly deferred:

- **Soft/word wrap**: implemented (`:set wrap`/`:set nowrap`) — character-boundary
  wrapping via `onda_render::{build_row_layout, locate_in_layout}`, wired through
  rendering, diagnostics overlay, and cursor placement. Word-boundary (greedy) wrap,
  and wrap-awareness for plugin decorations/debugger gutter markers, are tracked in
  `docs/KNOWN_ISSUES.md`.
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

## Phase 3 — WASM plugin migration (design-first, ADR-002)

DESIGN.md v0.3 mandates **WASM Component Model** plugins (ADR-002 explicitly
rejects Lua). The shipped `onda-lua` (mlua) system is a divergence being reversed.

### Done — W17 (WIT API v0)
- `wit/onda/{world,host,guest,types}.wit` — host API v0 (`@unstable`), mirroring
  the old Lua surface (notify/buffer/selection/keymap/command/decoration/picker/
  config) + capability-gated `fs`/`http`. Design-review doc: `wit/README.md`.
- `onda-plugin` crate scaffold: `manifest` (`onda-plugin.toml` schema + API-version
  gating), `permission` (capability model, request ∩ grant, `..`-escape rejection),
  `api` (`PluginApiCall` host-call queue). 11 tests, fmt+clippy green. No `wasmtime`
  dep yet (kept out of W17 so the heavy dep + bench gates land with W18).
- AGENTS.md: pre-approved deps swapped mlua → wasmtime/wit-bindgen/wasmparser/
  cap-std; "commonly wrong" entries added (no blocking host calls, no raw buffer).

### Done — W18 (wasmtime host)
- `onda-plugin::host` — `wasmtime::component::bindgen!` host bindings for the full
  WIT surface; effectful calls routed to the `PluginApiCall` queue, reads from a
  pre-frame buffer snapshot (rule 2). WASI satisfied via `wasmtime-wasi` (wasip2
  std pulls it in).
- `onda-plugin::engine` — `PluginEngine` (component model + epoch interruption +
  watchdog), per-instance `Store` with memory limit, lazy instantiate + `init`
  under the 5ms epoch budget, capability interfaces (`fs`/`http`) linked **only
  when granted** (link-time enforcement, T17.3).
- Integration tests against **real components** (built from `plugins/`, committed
  under `tests/fixtures/`): todo-highlighter emits a decoration batch; a busy-loop
  plugin is trapped by the epoch budget; git-blame reads `.git/HEAD` through a
  granted fs cap; an ungranted capability fails to link. 20 tests, fmt+clippy green.

### Done — W19 (plugin manager)
- `onda-plugin::manager` — `PluginManager` install/list/remove over a store dir +
  `plugins.lock`; sources: `github:user/repo[@rev]`, git URLs (incl. `file://`),
  local dirs. Staging→promote so a bad manifest can't half-install; entry-component
  presence verified; resolved commit sha recorded. Tested (local + local-git).

### Done — W20 (reference plugins, real WASM components)
- `plugins/{todo-highlighter,git-blame-inline,http-client}` build to wasm32-wasip2
  components via wit-bindgen. Validate: decoration batch / event flow (todo);
  fs capability + virt-text + cursor-hold (git-blame — real per-line blame awaits a
  host `vcs` interface, deferred); network capability + command + picker (http —
  host HTTP is v0-stubbed). `plugins/hostile-test` is the containment fixture.

### Done — final swap (binary rewiring)
- **`onda-lua` removed**: crate deleted, `mlua` workspace dep dropped,
  `runtime/plugins/*.lua` deleted. `ExCommand::LuaCommand` removed (it was dead —
  `parse` never constructed it).
- **Binary on `onda-plugin`** (`crates/onda/src/plugin_host.rs`): `PluginHost`
  discovers + instantiates installed plugins at startup (init registers commands),
  fires editor events (buffer-open at load, cursor-hold/buffer-change on idle),
  applies `PluginApiCall`s between frames (notify, buffer edits, cursor/selection,
  float, highlight-group → theme reapply). `:name` dispatches to plugin commands.
  `onda plugin install|list|remove` CLI wired. fmt+clippy+`cargo test --workspace`
  green; CLI install→list→remove smoke-tested with a real component.

### Done — decoration rendering
- Plugin `SetDecorations`/`ClearDecorations` stored per (doc, namespace) and
  painted in `render_frame` after git signs: highlight ranges (cell-style overlay
  preserving the grapheme), gutter signs, and end-of-line virtual text (inlay
  style). Colors parse `#rrggbb`/basic names. `draw_plugin_{signs,highlights,
  virt_text}` unit-tested for coordinate mapping. todo-highlighter now visibly
  marks TODO/FIXME lines; git-blame shows the branch at end of the cursor line.

### Outstanding — plugin follow-ups
- **Permission approval UI**: `discover` auto-grants declared capabilities; add the
  install-time + first-use prompt (T18.3 / T24.3 pattern). fs is still whitelist- +
  `..`-scoped and ungranted imports still fail to link.
- **Lazy-by-event activation**: plugins currently instantiate eagerly at startup
  (so command tables are known). Switch command-activated plugins to instantiate on
  first `:name` once a manifest pre-scan registers their command names.
- **Plugin keymaps / picker contributions / statusline segments**: wired (W37) —
  statusline segments render; `KeymapSet` registers normal-mode keymaps (matched on
  unbound single keys, ADR-106 keeps static bindings winning); `UiPick` opens a picker
  whose selection invokes the plugin callback; `CmdCreate` (init **and** runtime) feeds
  the command palette + `:`. Callback attribution flows via a packed `(plugin_idx,
  callback_id)` handle through the flat call stream (`plugin_host::pack_handle`).
  **Remaining:** a plugin-contributed **sidebar tree/panel** (needs a new `wit/onda`
  contribution surface + guest binding), multi-key plugin `lhs`, and a compiled sample
  plugin to exercise `KeymapSet`/`UiPick` end-to-end in CI (current fixtures only emit
  decorations/blame).
- **W19 polish**: `update` (re-resolve lockfile), `onda plugin dev --watch`,
  `cargo generate` template, full `docs/plugin-book/` (quickstart drafted).
- **`http` host impl** (currently v0-stub) + a `vcs` host interface for real blame.

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

- **Agent (Phase 4 W23 + T24.2)**: panel wired — streaming thread, tool cards, input
  box, permission prompt (persisted), `@`-mention resolution, fs/read from live buffers,
  `:agent-export`. Diff review (T24.2) done: agent writes stage into the rebase engine;
  `:agent-review` gives per-hunk accept/reject/accept-all applied as one undo step, with
  rejected hunks reported back to the agent.
- **Persistent undo** (Phase 5 T29.1): done — content-hash-keyed `UndoStore`, opt-in,
  persisted on save, lazily restored on first undo, mismatch/corrupt → clean fallback.

### Outstanding (not implemented — each is large and/or needs external infra)
- **Remote editing `scp://`** (Phase 3 W17): no `russh` transport; needs a live SSH host.
- **libvterm** (Phase 3 W17): terminal still uses `vt100`; vendoring + nvim/tmux/htop
  regression is a large FFI effort.
- **Live Claude Code** (Phase 4): the agent path is proven against `onda-mock-agent`;
  driving the real `claude-code acp` binary needs it installed (manual release check).
- **Release/launch** (W31/W32): clean-machine install matrix, Homebrew tap, docs site,
  signed multi-platform artifacts, `v0.0.3`/`v0.1.0` tags + announcement — not codeable/
  verifiable in a sandbox.
