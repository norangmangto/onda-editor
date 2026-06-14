# onda — Phase 1 Plan: Editor Completeness

**Duration:** 5 weeks | **Milestone demo:** "onda is developed in onda" (dogfooding starts)
**Design doc:** `docs/DESIGN.md` v0.3 | **Agent rules:** `AGENTS.md` | **Prereq:** Phase 0 exit criteria all green

## Goal

Phase 0 proved the engine is fast. Phase 1 makes onda a *complete* daily-driver modal
editor: syntax highlighting with incremental parsing, the full vim editing vocabulary
(undo tree, registers, macros, text objects, `.` repeat), search, splits, and a fuzzy
picker. No LSP yet (Phase 2) — but everything a vim user's fingers expect must work.

The hard constraint: **syntax intelligence must not cost a single frame.** All new
features land behind the existing bench gates, and new gates are added for the
highlight-on path.

## Exit criteria

- [ ] Rust / Python / JSON / TOML files auto-detected and highlighted via tree-sitter,
      with tree-sitter ERROR nodes rendered as inline warnings
- [ ] Typing in a 100k-line Rust file with highlighting on: keypress→render p99 < 10ms
      (new bench gate, added to `bench/baseline.json`)
- [ ] Cold startup still < 40ms with grammars *available* (lazy-loaded, not eager)
- [ ] Undo tree, named registers, macros, `.` repeat, text objects, visual-block work
      per the test tables
- [ ] Regex search/substitute, split windows, fuzzy file picker functional
- [ ] `config.toml` loads options + keymap overrides without measurable startup cost
- [ ] **Dogfooding gate:** at least one real onda feature is developed *using onda*
      during week 5, and the friction list is captured in `docs/DOGFOOD.md`

## Workstreams & dependency order

```
T5.0 harness update ─► W5 Syntax ───────────────┐
                    ─► W6 Editing vocabulary ───┼─► W9 Dogfooding & hardening
                    ─► W7 Search & navigation ──┤
                    ─► W8 Windows, config, UI ──┘
```

W5–W8 are largely parallel (different crates). W9 is week 5.

---

## T5.0 — Harness update (day 1, before anything else)

- Extend `AGENTS.md` pre-approved deps: `tree-sitter`, `libloading`, `regex`,
  `nucleo-matcher`, `serde`, `toml`, `notify` (file watcher), `arboard` (clipboard)
- New bench fixtures: 100k-line Rust file with realistic structure, deeply-nested JSON,
  malformed TOML (for error-node rendering)
- New gates in `xtask bench --check`: highlight-on typing latency, highlight-on
  startup, parse time budget for fixture set
- **Accept:** gates run in CI; intentionally synchronous parse in the render path fails CI

---

## W5 — Syntax layer (`onda-syntax`, weeks 1–3)

### T5.1 — Grammar infrastructure
- Grammar registry: `runtime/grammars.toml` listing tree-sitter grammar sources
  (rust, python, json, toml) pinned to revisions
- `onda grammar fetch` / `onda grammar build` (xtask-backed): clone, compile to dylib
  into the runtime dir; prebuilt grammars bundled in release artifacts later
- Load grammars at runtime via `libloading`, **lazily on first buffer of that type**
  (protects the 40ms startup budget)
- **Accept:** fresh machine: `onda grammar fetch && onda grammar build` → opening
  `main.rs` highlights; startup bench unchanged when no file is opened

### T5.2 — Filetype detection
- Detection chain: extension map → shebang → content sniffing (JSON/CSV delimiter
  heuristics per DESIGN §5.4.2) → user override (`:set filetype=`)
- Declared in `runtime/languages.toml` (extensions, comment tokens, indent defaults,
  grammar name) — data, not code
- **Accept:** table-driven tests incl. extensionless `#!/usr/bin/env python3` script,
  `.jsonl`, `Cargo.toml`

### T5.3 — Incremental parsing worker
- Syntax worker on the tokio runtime owns parse trees; consumes the same `ChangeSet`
  stream as everything else (`tree.edit()` + incremental reparse)
- Main loop renders with the **last completed** highlight spans; a parse that misses
  the frame deadline never blocks input (AGENTS.md rule 2)
- Debounce + cancellation for rapid typing; full reparse fallback on desync (checksum)
- **Accept:** typing burst test on 100k-line fixture passes the new latency gate;
  kill-the-worker chaos test → editor keeps working, unhighlighted

### T5.4 — Highlighting & queries
- `runtime/queries/<lang>/highlights.scm` (start from upstream/helix queries, trimmed);
  span resolution maps capture names → theme scopes
- Minimal built-in theme (one dark, one light) with the scope table from DESIGN —
  full theme system stays in Phase 5, but the scope *names* are fixed now (API surface)
- Viewport-only span materialization: only visible lines get styled (1GB rule applies)
- **Accept:** golden-grid snapshot tests per language; scrolling a highlighted 1GB-ish
  file stays within damage budget

### T5.5 — Error nodes & structural niceties
- tree-sitter `ERROR`/`MISSING` nodes rendered as undercurl + gutter sign → instant
  syntax-error feedback for JSON/TOML before any LSP exists (DESIGN §5.3)
- Auto-indent on newline using `indents.scm` where available, fallback to
  keep-previous-indent
- Bracket-pair awareness: `%` motion via the tree when available, text fallback
- **Accept:** broken JSON fixture shows error exactly at the offending token; `o` in
  a Rust block indents correctly

---

## W6 — Editing vocabulary (`onda-modal` / `onda-core`, weeks 1–3)

### T6.1 — Undo tree
- Replace the Phase 0 linear stack behind the `UndoHistory` trait with a tree:
  branches on divergent edits, `u`/`Ctrl-r` walk current branch, `g-`/`g+` walk
  chronologically across branches
- Timestamped nodes; tree visualizer picker deferred (BACKLOG)
- **Accept:** vim-semantics test: edit A, undo, edit B, `g-` reaches A's state

### T6.2 — Registers & clipboard
- Named registers `"a`–`"z` (append with `"A`), numbered delete history `"1`–`"9`,
  yank register `"0`, black hole `"_`, system clipboard `"+` via `arboard`
  (clipboard I/O happens off the main thread — rule 2)
- Registers store charwise/linewise/blockwise kind
- **Accept:** table tests for register routing on `d`/`y`/`p` combinations

### T6.3 — Macros & repeat
- `q{reg}` record / `@{reg}` replay / `@@`; recording captures resolved key events
- `.` repeat: last change (operator+motion or insert-session) replayable with count
- Replay runs through the normal key pipeline (no special-cased mutation path) —
  guarantees macros stay correct as features are added
- **Accept:** macro that edits 3 lines replays with `3@q` correctly across multicursor;
  `.` after `ciwfoo<Esc>` repeats on a new word

### T6.4 — Text objects
- Pair objects `i(/a(`, `i[/a[`, `i{/a{`, `i"/a"`, `i'/a'`, `` i`/a` `` (text-scan based,
  multiline-aware); word/paragraph `iw aw iW aW ip ap`
- Tree-sitter objects `if/af` (function), `ic/ac` (class), `ia/aa` (argument) via
  `textobjects.scm` when a grammar is loaded — graceful absence otherwise
- **Accept:** `ci(`, `daf` test tables; objects operate per-range under multicursor

### T6.5 — Visual-block + marks + jumplist (Phase 0 backlog sweep)
- `Ctrl-v` visual-block: block selection as N ranges (it's just multicursor — ADR-006
  pays off), `I`/`A` block insert, `d`/`y`/`p` blockwise
- Marks `m{a-z}` buffer-local, `` ` ``/`'` jumps; jumplist with `Ctrl-o`/`Ctrl-i`
- **Accept:** classic block-edit scenario (prepend `// ` to 10 lines) matches vim

---

## W7 — Search & navigation (weeks 2–4)

### T7.1 — Regex search
- `/` `?` incremental search (highlight matches while typing), `n/N`, `*`/`#` word
  search, `hlsearch` + `:noh`; rust `regex` crate with vim-pattern translation for
  the common subset (`\<`, `\>`, case-smart: smartcase default)
- Search runs on a worker for large buffers; viewport matches render first
- **Accept:** incremental search on the 1GB fixture stays responsive; smartcase tests

### T7.2 — Substitute
- `:[range]s/pat/rep/[g][c][i]` with capture groups; `c` confirm mode steps through
  matches (y/n/a/q); whole-file `%s` executed as a single Transaction (one undo step)
- **Accept:** `%s` over 100k lines completes within frame-budget rules (chunked apply,
  progress in statusline); undo restores in one step

### T7.3 — Fuzzy pickers (`nucleo-matcher`)
- Picker UI component (floating overlay, reusable for Phase 2 symbol/diagnostic
  pickers — design the component API, not a one-off)
- File picker (`<space>f`): respects `.gitignore` (use the `ignore` crate walker, on a
  worker, streaming results into the matcher); buffer picker (`<space>b`)
- **Accept:** picker opens instantly on a 100k-file synthetic tree, results stream in;
  zero main-thread walking

### T7.4 — File tree (minimal)
- Toggleable sidebar: expand/collapse, open file, create/rename/delete with confirm;
  refresh on focus via `notify` watcher events
- Deliberately minimal — the picker is the primary navigation; the tree is for
  orientation (keep scope locked, extras go to BACKLOG)
- **Accept:** basic CRUD scenario; no watcher events processed on main thread

---

## W8 — Windows, config & UI polish (weeks 2–4)

### T8.1 — Window splits
- `:sp`/`:vsp`, `Ctrl-w h/j/k/l` focus moves, `Ctrl-w c/o`, resize commands; layout as
  a tree of splits; per-window viewport/cursor over shared buffers (two windows on one
  buffer must stay consistent — flows naturally from ChangeSet)
- Compositor renders window borders; damage tracking stays per-window
- **Accept:** edit in one split reflects live in the other; bench: splits don't
  regress render budget

### T8.2 — Config loading (`onda-config`)
- `~/.config/onda/config.toml` + project `.onda/config.toml` overlay merge (DESIGN §5.7)
- Surface: editor options (numbers, scrolloff, tabwidth, expandtab…), keymap overrides
  (remap/unmap into the trie from T2.1's data tables), theme selection
- Strict parse errors: bad config → clear startup message + defaults, never a crash
- Hot reload on `:config-reload` (file-watch auto-reload → BACKLOG)
- **Accept:** startup bench with a realistic config shows < 1ms parse cost; bad TOML
  produces a friendly diagnostic with line number

### T8.3 — UI completeness
- Statusline v2: filetype, register-recording indicator (`recording @q`), search count
  (`[3/17]`), pending-keys display (`2d` shown while waiting)
- `:` command completion (command names + file paths) using the picker matcher
- Soft wrap: **explicitly deferred** (BACKLOG, revisit before Phase 5) — document the
  decision in DESIGN changelog
- **Accept:** snapshot tests; command completion works for `:e src/ma<Tab>`

---

## W9 — Dogfooding & hardening (week 5)

### T9.1 — Perf re-verification & baseline update
- Full bench suite with highlighting on across all fixtures; update `baseline.json`
  with justification PR; run `bench-compare` vs nvim/helix *with syntax enabled* and
  refresh `BENCH_REPORT.md`
- **Accept:** all Phase 0 + Phase 1 gates green on macOS + Linux

### T9.2 — Dogfooding sprint
- Develop one real task (pick a small W-item or bug) entirely inside onda; log every
  friction point in `docs/DOGFOOD.md` with severity (blocker / annoying / nice-to-have)
- Crash triage: any panic during dogfooding becomes a regression test + fix before
  phase close
- **Accept:** `DOGFOOD.md` exists with honest findings; zero known panics

### T9.3 — Retro & Phase 2 prep
- Sweep `BACKLOG.md`; draft `PHASE2_PLAN.md` (LSP client, diagnostics/completion UI,
  integrated terminal, auto-session L1) in this same format; tag `v0.0.2-phase1`
- **Accept:** Phase 2 plan reviewed against DESIGN §7 Phase 2 scope

---

## Suggested order for Claude Code

```
T5.0 → T5.1 → T5.2 → T6.1 → T6.2 → T5.3 → T5.4 → T7.1 → T6.3 → T6.4 →
T8.1 → T5.5 → T7.2 → T6.5 → T7.3 → T8.2 → T8.3 → T7.4 → T9.1 → T9.2 → T9.3
```

Rationale: grammar plumbing (T5.1–5.2) early because it has external-tooling risk;
undo tree (T6.1) early because later features assume it; the picker component (T7.3)
after search settles so the overlay UI patterns are stable.

## Phase 1 risks

| Risk | Mitigation |
|---|---|
| tree-sitter dylib builds flaky across machines | T5.1 acceptance includes fresh-machine test; prebuilt artifacts tracked as Phase 5 release work |
| Vim-pattern regex translation rabbit hole | Support the documented common subset only; unsupported escapes → clear error, BACKLOG the rest |
| Picker/file-tree scope creep | T7.4 scope is locked to the listed accept criteria; everything else → BACKLOG |
| `%s` on huge files violating frame budget | Chunked Transaction apply designed in T7.2 from the start, not retrofitted |
