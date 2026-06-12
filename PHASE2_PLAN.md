# onda — Phase 2 Plan: onda as a Go/Rust IDE

**Duration:** 6 weeks | **Milestone demo:** "onda as a Go/Rust IDE"
**Design doc:** `docs/DESIGN.md` v0.4 | **Agent rules:** `AGENTS.md` | **Prereq:** Phase 1 exit criteria all green

## Goal

Phase 1 delivered a complete modal editor. Phase 2 makes onda a genuine IDE for
systems-language projects: Language Server Protocol client with hover, completions,
go-to-definition, diagnostics, and rename/format; an integrated terminal with a real PTY;
auto-session persistence; soft wrap for prose; mouse support; and a Lua plugin foundation
that third-party authors can target.

The hard constraint does not change: **every new feature must clear the existing bench
gates, and new gates are added for LSP and terminal paths.**

---

## Exit criteria

- [ ] Rust and Go LSP (rust-analyzer, gopls) provide hover, completions, go-to-definition,
      inline diagnostics, rename, and format-on-save — all working inside onda on a real
      project
- [ ] Keypress → render p99 < 10ms with an active LSP server (new bench gate: LSP-on path)
- [ ] Cold startup still < 40ms with LSP configured but not yet started (server is lazy)
- [ ] Integrated terminal opens a shell via PTY; `Mode::Terminal` correctly routes all
      key events to the process; VT100 / ANSI escape sequences render without corruption
- [ ] `:session save` / ``:session restore` round-trips buffer list, split layout, and
      cursor positions; session auto-saved on quit
- [ ] Soft wrap renders long lines correctly; no horizontal scroll needed for prose files
- [ ] Mouse: click to move cursor, scroll wheel scrolls viewport, click in picker selects
- [ ] Lua plugin API (`~/.config/onda/plugins/*.lua`): a third-party plugin can define a
      keybinding, open a floating window, and read/write buffer text — all via documented
      API with no unsafe surface
- [ ] `docs/PLUGIN_API.md` exists and covers the Phase 2 API surface
- [ ] **Dogfooding gate:** onda is used as the primary editor for at least one full week of
      Phase 3 planning; friction list captured in `docs/DOGFOOD.md` (Phase 2 session)

---

## Workstreams & dependency order

```
T10.0 harness update ─► W10 LSP client ──────────────┐
                     ─► W11 Terminal (PTY) ───────────┤
                     ─► W12 Sessions + UX ────────────┼─► W14 Hardening & dogfooding
                     ─► W13 Lua plugins ──────────────┘
```

W10–W13 are largely parallel (different crates). W14 is the final week.

---

## T10.0 — Harness update (day 1, before anything else)

- Extend `AGENTS.md` pre-approved deps: `lsp-types`, `async-lsp` (or `tower-lsp-client`),
  `portable-pty`, `vt100` (terminal emulation), `mlua` (Lua 5.4 via feature flag),
  `serde_json`, `url`, `tempfile`
- New bench fixtures: a real mid-size Rust workspace (~20k lines) for LSP smoke tests;
  a long-line prose file for soft-wrap rendering
- New gates in `xtask bench --check`:
  - LSP-on keypress latency (p99 < 10ms)
  - Terminal frame render cost (< 4ms per frame for a 80x24 PTY)
  - Lua hook overhead per keypress (< 0.5ms total)
- **Accept:** gates run in CI; synchronous LSP call on the main thread fails CI

---

## W10 — LSP client (`onda-lsp`, weeks 1–4)

### T10.1 — Transport & lifecycle
- New crate `onda-lsp`: JSON-RPC 2.0 over stdin/stdout; LSP server process managed by
  tokio (spawn, kill on quit, restart on crash with backoff)
- `LspManager` in the background worker pool: one server per workspace root, servers
  shared across splits on the same file
- Capability negotiation on `initialize`; `textDocument/didOpen` / `didChange` (full sync
  for Phase 2, incremental in Phase 3) / `didClose` consume the existing `ChangeSet` stream
- **Accept:** rust-analyzer starts and reports `initialized` for a real Rust workspace;
  log shows capability handshake; main thread never awaits LSP responses

### T10.2 — Diagnostics
- `textDocument/publishDiagnostics` pushed to the main loop via `BgMessage::Diagnostics`
- Inline rendering: underline severity-coloured spans in the buffer view; gutter signs
  (E/W/I/H icons) in the sign column; statusline shows `E:2 W:1` count
- Diagnostics float on `<space>d` (or cursor-hover after delay); `:lnext`/`:lprev` navigate
- **Accept:** introduce a deliberate type error in a Rust file → diagnostic appears within
  2s (server-dependent); fixing it clears within one more `didChange` push

### T10.3 — Hover
- `textDocument/hover` triggered on `K` (Normal mode); response rendered in a floating
  window using the picker overlay component from T7.3
- Request is async: `K` queues the request; the float appears when the response arrives
  (frame budget not blocked); `<Esc>` or cursor move dismisses
- Markdown content in hover responses: render bold/italic/code spans with theme scopes;
  no full Markdown parser needed — handle the four common patterns (code block, bold,
  italic, plain)
- **Accept:** `K` on a Rust `struct` field shows the doc comment from rust-analyzer;
  latency from `K` to visible float < 500ms on a warm server

### T10.4 — Go-to-definition / references
- `textDocument/definition` on `gd`; `textDocument/references` on `gr` (results in picker)
- Multi-result definition uses the picker (consistent with T7.3 patterns)
- Jump integrates with the existing jumplist (T6.5): `Ctrl-o` returns from definition
- `textDocument/typeDefinition` on `gD`; `textDocument/implementation` on `gi`
- **Accept:** `gd` on a function call in a real Rust workspace navigates to the definition;
  `Ctrl-o` returns; `gr` shows all references in the picker

### T10.5 — Completions
- `textDocument/completion` triggered in Insert mode: character trigger (`.`, `::`, `:`)
  and explicit `Ctrl-n`/`Ctrl-p`
- Completion menu as a floating widget (10 items visible, scrollable): item text,
  kind icon (fn/struct/var/…), detail column
- Confirmed with `<Tab>` or `<Enter>`; `<Esc>` dismisses without inserting; completion
  runs as a cancelable async request — a new character cancels the in-flight request
- `textDocument/completionItem/resolve` for detail/documentation on the selected item
- **Accept:** typing `std::` in a Rust file shows stdlib completions; selecting one inserts
  correctly including snippet placeholders (tab-stop navigation with `<Tab>`)

### T10.6 — Rename & format
- `textDocument/rename` on `<space>r`: prompt for new name in the command line; apply
  the returned `WorkspaceEdit` as a set of Transactions (one per file, one undo step each)
- `textDocument/formatting` on `:Format` and optionally format-on-save (config opt-in);
  response applied as a single Transaction
- **Accept:** rename a Rust struct field across a 5-file workspace; all references updated
  in one undo-able step; format on a deliberately unformatted file produces `rustfmt` output

---

## W11 — Integrated terminal (`onda-terminal`, weeks 2–4)

### T11.1 — PTY backend
- New crate `onda-terminal`: spawn a shell via `portable-pty`; `PtyProcess` owns the
  master side; a dedicated tokio task reads PTY output and pushes `BgMessage::PtyData`
  frames to the main loop
- PTY resize propagates on window resize (SIGWINCH equivalent via `portable-pty`)
- Shell determined by `$SHELL`, fallback to `/bin/sh`
- **Accept:** `:terminal` opens a pane running the user's shell; `echo hello` produces
  visible output; resize works

### T11.2 — VT100 emulation
- Embed `vt100` crate (or equivalent): maintain an in-memory `Screen` of `(rows, cols)`
  cells updated by parsing PTY output bytes; cells carry SGR attributes (fg, bg, bold,
  italic, underline)
- Map `vt100::Screen` to the `onda-render` `Grid` segment each frame (damage region =
  changed cells from the previous `Screen` snapshot)
- Handle common sequences: cursor movement, SGR colors (16-color + 256 + true color),
  erase-in-line/display, alternate screen, window title
- **Accept:** `htop` and `git log --oneline --graph` render correctly without corruption;
  `vim` inside the terminal pane opens and closes without breaking onda's outer terminal

### T11.3 — Mode::Terminal & key routing
- New mode `Mode::Terminal`; when a terminal pane is focused all key events are forwarded
  as raw bytes to the PTY writer (no keymap processing)
- `Ctrl-\ Ctrl-n` escapes back to `Mode::Normal` (helix convention; also configurable)
- Terminal pane participates in the split layout (T8.1); multiple terminal panes are
  independent PTY processes
- **Accept:** run `fish` shell with custom prompt in a terminal pane; navigate to another
  split with `Ctrl-w l`; return; shell session is still alive and interactive

### T11.4 — Terminal UX polish
- Scrollback buffer (configurable, default 10 000 lines); `Mode::TerminalScroll` lets
  the user scroll the history with normal vim motions (`Ctrl-u/d`, `gg/G`), `i` or
  `<Esc>` returns to insert
- Copy from terminal: visual selection in `Mode::TerminalScroll` yanks to the unnamed
  register
- **Accept:** run a command that produces 500 lines; scroll up in scrollback; copy a
  region with `V`; paste it into a normal buffer with `p`

---

## W12 — Sessions, soft wrap & mouse (`onda-session` + UX, weeks 2–4)

### T12.1 — Auto-session
- Session = buffer list (paths + unsaved content), split layout tree, per-window cursor
  and viewport, current working directory
- `:session save [name]` serializes to `~/.local/share/onda/sessions/<name>.toml`;
  `:session restore [name]` loads it; auto-save on `:wqa` and on `SIGTERM`
- Project-local session: if a `.onda/session.toml` exists in the cwd, onda loads it
  automatically (opt-in via config)
- **Accept:** open 3 files in splits, position cursors, quit, reopen — same layout and
  positions restored; round-trip is < 50ms added to startup

### T12.2 — Soft wrap
- Viewport renders logical lines wrapped at the window boundary; visual lines are
  numbered optionally (`+` prefix for continuation lines)
- Motions (`j/k`, `gj/gk` for visual-line variants), scrolling, and the damage
  compositor all operate on logical lines — no architectural regressions
- Toggle: `:set wrap` / `:set nowrap`; default off (consistent with Phase 0 horizontal-scroll)
- **Accept:** open a 200-character-per-line Markdown file; enable wrap; `j/k` moves
  logical lines; `gj/gk` moves visual lines; bench regression gate passes

### T12.3 — Mouse support
- crossterm mouse events: `EnableMouseCapture` on startup, `DisableMouseCapture` on quit
- Left-click: move cursor to clicked buffer position; click in picker selects item
- Scroll wheel: scroll viewport (3 lines per notch, configurable)
- Click in statusline / tab bar: switch window focus
- Mouse in terminal pane: forward raw mouse sequences to the PTY
- **Accept:** click to move cursor in a split; scroll a buffer; click a completion item;
  all with bench gate passing

### T12.4 — UX polish sweep (Phase 1 friction list)
- Resolve F-04 (Insert-mode `Ctrl-w`), F-15 (`Ctrl-w =` equalize), F-17 (`:ls`), F-20
  (register prefix echo) from `docs/DOGFOOD.md`
- `:messages` command to review startup and notification history (resolves F-19)
- Command-line completion improvements: file path completion for `:e`, command-name
  fuzzy completion — these were scoped to T8.3 but did not land
- Grammar auto-fetch on first use (resolves F-11): `:GrammarFetch` auto-triggered when a
  supported filetype is opened and grammars are absent; non-blocking, progress in statusline
- **Accept:** all F-0x blockers/annoyances from the Phase 1 dogfood list resolved or
  explicitly deferred with documented reasoning

---

## W13 — Lua plugin foundation (`onda-lua`, weeks 3–5)

### T13.1 — Runtime sandbox
- Embed Lua 5.4 via `mlua` (feature `lua54`); one `Lua` VM per session; sandboxed: no
  `io.open`, `os.execute`, `require` of arbitrary paths (whitelist: `onda.*`, stdlib
  non-IO modules)
- Plugin loader: scan `~/.config/onda/plugins/*.lua` and `<project>/.onda/plugins/*.lua`
  at startup (after editor init, non-blocking); errors in plugins printed to message line,
  never panic the editor
- **Accept:** a plugin that calls `onda.log("hello")` produces a message; a plugin that
  calls `os.execute("rm -rf /")` gets a sandbox error, not actual deletion

### T13.2 — Core API (`onda.*`)
- Documented API surface (see `docs/PLUGIN_API.md`):
  - `onda.buf.get_lines(buf, start, end)` / `onda.buf.set_lines(...)`
  - `onda.buf.get_text(buf, start_row, start_col, end_row, end_col)` / `set_text`
  - `onda.win.get_cursor(win)` / `onda.win.set_cursor(win, {row, col})`
  - `onda.keymap.set(mode, lhs, rhs_or_fn, opts)` — register keybindings from Lua
  - `onda.cmd.create(name, fn, opts)` — register custom commands
  - `onda.ui.float(opts)` — open a floating window with content
  - `onda.notify(msg, level)` — message line
  - `onda.autocmd.create(event, pattern, fn)` — event hooks (BufEnter, InsertLeave, etc.)
- All Lua→Rust calls go through a queue drained on the main loop (rule 2: Lua runs on
  the main thread but only between frames, bounded by a per-frame Lua budget)
- **Accept:** a plugin implements a custom `:Timestamp` command that inserts the current
  datetime at the cursor; verified with table tests against the Lua API

### T13.3 — Plugin docs & example plugins
- `docs/PLUGIN_API.md`: full API reference with types, examples, and caveats
- Three bundled example plugins under `runtime/plugins/`:
  - `rainbow_brackets.lua`: colorize bracket pairs with cycle colors
  - `word_count.lua`: show word count in statusline segment
  - `project_todos.lua`: picker listing `TODO`/`FIXME` comments across the project
- **Accept:** all three example plugins load and function on a fresh checkout

### T13.4 — LSP-Lua bridge (optional, if time allows)
- Expose `onda.lsp.get_diagnostics(buf)`, `onda.lsp.request(method, params, callback)`
  so plugins can extend LSP behavior
- Defer if W10 is not fully settled — note in BACKLOG if not reached
- **Accept:** a plugin that adds a custom `<space>H` hover formatted with extra context
  using `onda.lsp.request`

---

## W14 — Hardening & dogfooding (week 6)

### T14.1 — Perf re-verification
- Full bench suite: LSP-on, terminal active, Lua plugins loaded; update `baseline.json`;
  run `bench-compare` vs nvim/helix *with LSP enabled* and update `BENCH_REPORT.md`
- **Accept:** all Phase 0 + Phase 1 + Phase 2 gates green on macOS + Linux

### T14.2 — Dogfooding sprint (Phase 2)
- Use onda as the primary editor for at least 5 consecutive working days (Phase 3 planning
  work happens inside onda); log every friction point in `docs/DOGFOOD.md` Phase 2 section
- Crash triage: any panic during dogfooding → regression test + fix before phase close
- **Accept:** `DOGFOOD.md` Phase 2 section populated; zero known panics

### T14.3 — Fuzzing & edge case hardening
- `cargo-fuzz` targets for: PTY output parser (malformed VT sequences), LSP response
  parser (malformed JSON-RPC), Lua plugin loader (malformed Lua)
- At least one fuzz corpus commit with seeds from real LSP/terminal traffic
- **Accept:** 24h fuzz run finds zero panics (or all found panics fixed)

### T14.4 — Retro & Phase 3 prep
- Sweep `BACKLOG.md`; draft `PHASE3_PLAN.md` (DAP debugger, git integration, remote
  editing, Phase 2 polish) in this format; tag `v0.0.3-phase2`
- **Accept:** Phase 3 plan drafted and reviewed against `docs/DESIGN.md`

---

## Suggested implementation order

```
T10.0 →
  T10.1 → T10.2 → T11.1 → T10.3 → T11.2 →
  T12.1 → T10.4 → T11.3 → T10.5 → T12.2 →
  T13.1 → T10.6 → T11.4 → T12.3 → T13.2 →
  T12.4 → T13.3 → T13.4 →
  T14.1 → T14.2 → T14.3 → T14.4
```

Rationale: LSP transport (T10.1) first because T10.2–T10.6 all depend on it and it
has the most external-tooling risk; PTY backend (T11.1) early because terminal
emulation bugs are hard to retrofit; session (T12.1) before soft-wrap and mouse because
it has no dependencies; Lua sandbox (T13.1) after LSP is stable so the bridge (T13.4)
can be validated.

---

## Phase 2 risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| rust-analyzer JSON-RPC protocol edge cases (cancelRequest timing, partial results) | High | T10.1 acceptance includes a protocol conformance test against the actual binary; log all traffic at DEBUG level |
| VT100 emulation incomplete for complex TUI apps (tmux, neovim-in-terminal) | High | Scope is explicitly common sequences; complex apps documented as "unsupported in Phase 2"; defer to Phase 3 libvterm integration if needed |
| mlua compile time / binary size regression | Medium | Gate binary size in CI; evaluate Lua 5.4 vs LuaJIT vs Wren if size is a problem |
| LSP-on latency regression past 10ms gate | Medium | Request/response runs entirely on tokio; main thread only handles `BgMessage` delivery — enforce this with the CI gate from T10.0 |
| Lua plugins accessing unsafe internals via FFI | Medium | Sandbox explicitly blocks `ffi`, `package.loadlib`; fuzz the plugin loader (T14.3) |
| Soft wrap breaking damage-tracking assumptions | Low | T12.2 designs logical vs visual line tracking from scratch, not a retrofit; bench gate added before merging |
| Session restore startup latency regression | Low | Session load is async; T12.1 acceptance gate: < 50ms added startup on a 20-buffer session |

---

## New crates introduced in Phase 2

| Crate | Purpose | Pre-approved |
|---|---|---|
| `onda-lsp` | LSP client, JSON-RPC transport | — (new, listed in T10.0) |
| `onda-terminal` | PTY management, VT100 emulation | — (new, listed in T10.0) |
| `onda-session` | Session serialization | — (new; uses `toml` already pre-approved) |
| `onda-lua` | Lua plugin runtime | — (new, listed in T10.0) |

Crate dependency rules from `docs/DESIGN.md` §6 still apply: new crates depend on
`onda-core`, never on each other, and the binary wires them together.
