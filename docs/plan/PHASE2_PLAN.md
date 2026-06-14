# onda — Phase 2 Plan: Language Intelligence + Workspace

**Duration:** 7 weeks | **Milestone demo:** a full day of real Python/Rust work in onda, no VS Code open
**Design doc:** `docs/DESIGN.md` v0.3 §5.4, §5.8 | **Agent rules:** `AGENTS.md` | **Prereq:** Phase 1 exit criteria green, dogfooding active

## Goal

Turn onda from a fast editor into an IDE: a production-grade LSP client with
diagnostics, completion, navigation, rename, and formatting — verified against
rust-analyzer, basedpyright + ruff, taplo, and vscode-json-languageserver. Plus the
workspace tools that make a project feel inhabited: live grep, symbol pickers, an
integrated terminal, and automatic session restore (L1).

The hard constraint, as always: **language servers are guests, not owners.** A slow,
chatty, or crashed server may degrade intelligence, never input latency. The bench
gates encode this.

## Exit criteria

- [ ] Typing in a real Rust workspace with rust-analyzer attached: keypress→render
      p99 < 10ms (new bench gate — LSP attached, server warm)
- [ ] Completion popup: visible within 5ms of server response (UI overhead gate);
      stale responses (older doc version) are discarded, never rendered
- [ ] Diagnostics, hover, goto def/ref, rename (cross-file), format-on-save work on
      all four server integrations; multi-server Python (basedpyright + ruff) merges
      cleanly
- [ ] Server crash → automatic restart with backoff; editor never blocks or panics
- [ ] Live grep + document/workspace symbol pickers over the picker component
- [ ] Integrated terminal: split/floating, full vim-mode scrollback, cwd follows project
- [ ] Auto-session L1: quit onda, reopen in same directory → layout, buffers, cursors,
      jumplist restored; cold start with a 30-buffer session still < 40ms time-to-input
      (lazy restore gate)
- [ ] CI runs E2E tests against pinned server versions (Linux)
- [ ] `BENCH_REPORT.md` refreshed: onda+LSP vs nvim+lspconfig vs helix

## Workstreams & dependency order

```
T10.0 harness ─► W10 LSP core ─► W11 Feature UI ─► W12 Server verification ─┐
            └──► W13 Workspace tools (after picker reuse is clear) ─────────┼─► W16
            └──► W14 Terminal (independent) ────────────────────────────────┤
            └──► W15 Session L1 (independent) ──────────────────────────────┘
```

W10→W11→W12 is the critical path. W13–W15 are parallelizable; schedule terminal and
session early-ish since dogfooding benefits immediately.

---

## T10.0 — Harness update (day 1)

- Pre-approved deps added to `AGENTS.md`: `lsp-types`, `serde_json`, `alacritty_terminal`
  (VT parser — we do **not** hand-roll one), `portable-pty`, `which`, `sha2` (session
  keys), `bincode` or `postcard` (session blobs)
- New bench fixtures: a real-ish Rust workspace (vendored small crate), a Python package
  with venv; scripted "warm server typing" scenario
- New gates: LSP-attached typing latency, completion UI overhead, session lazy-restore
  startup; CI installs pinned servers (rust-analyzer, basedpyright, ruff, taplo,
  vscode-langservers-extracted) for the E2E job
- **Accept:** gates wired into `xtask bench --check`; server versions pinned in
  `xtask/servers.toml`

---

## W10 — LSP client core (`onda-lsp`, weeks 1–3) ← critical path

### T10.1 — Transport & lifecycle
- stdio JSON-RPC: Content-Length framing, request/response/notification routing on the
  tokio runtime; careful stdin/stdout pump design (no deadlock when both sides write —
  dedicated reader/writer tasks + bounded channels)
- Lifecycle: spawn → `initialize` (client capabilities declared honestly — only what
  W11 implements) → `initialized` → … → `shutdown`/`exit`; kill-on-timeout
- Server stderr captured to the log file (`ONDA_LOG`), not the terminal
- **Accept:** lifecycle integration test against a mock server + rust-analyzer;
  chaos test: server writes garbage → client isolates the error, editor unaffected

### T10.2 — Document synchronization
- `didOpen`/`didClose` driven by buffer lifecycle; `didChange` **incremental**, derived
  directly from `ChangeSet` (this is why ADR on transactions exists — no diffing);
  `didSave` with optional include-text; document version counter is the single
  source for staleness checks
- UTF-16 position encoding handled at the boundary (negotiate `positionEncoding`,
  prefer utf-8 where servers support it; conversion utilities property-tested —
  this is the classic LSP bug farm, test it hard)
- **Accept:** property test — random edit sequences keep server-side text identical to
  rope content (mock server echoes back full text for comparison)

### T10.3 — Request manager
- Typed request API with: per-request timeout, `$/cancelRequest` on supersession
  (new completion request cancels in-flight one), document-version tagging so stale
  responses are dropped before reaching UI, debounce policies per method
- Backpressure: bounded in-flight requests per server; diagnostics are pull-merged
  per publish, never queued unboundedly
- **Accept:** typing burst test shows cancellations issued and zero stale popups;
  latency gate passes with an artificially slow mock server (500ms responses)

### T10.4 — Server registry & multi-server routing
- `runtime/languages.toml` extended: per-language server list with command, args,
  root-dir detection (e.g. `Cargo.toml`, `pyproject.toml`, `.git`), init options,
  capability-based routing (completion from basedpyright, diagnostics from both
  basedpyright + ruff, formatting from ruff)
- Project-local overrides via `.onda/config.toml` (e.g. point at a venv's basedpyright)
- Crash restart with exponential backoff (max 3, then disabled with statusline notice);
  `:lsp-restart`, `:lsp-status` debug commands
- **Accept:** Python buffer runs two servers concurrently with merged diagnostics
  attributed per-source; kill -9 a server → auto-restart; third crash → clean disable

---

## W11 — Language feature UI (weeks 2–5)

### T11.1 — Diagnostics
- Rendering: undercurl on span + severity gutter sign + optional end-of-line virtual
  text (config); merge per buffer across servers, sorted, deduplicated
- Navigation: `]d`/`[d`, diagnostics picker (buffer + workspace scope) reusing the
  Phase 1 picker component, severity filter
- tree-sitter ERROR nodes (Phase 1) demoted automatically when an LSP provides
  syntax-level diagnostics for that filetype (no double-reporting)
- **Accept:** snapshot tests; 5k-diagnostic stress file (generated) scrolls within
  damage budget

### T11.2 — Completion
- Trigger: typed trigger characters + manual `<C-space>`; debounced; fuzzy-filtered
  client-side with the nucleo matcher as the user keeps typing (no re-request per key)
- Popup UI: kind icons (nerd-font with ASCII fallback), detail/doc panel lazily via
  `completionItem/resolve`, scrolling, `<C-n>/<C-p>` + `<Tab>` config
- Edits applied as Transactions: `textEdit`/`additionalTextEdits` (auto-import!)
  composed atomically — one undo step
- **Snippet engine (minimal):** LSP snippet syntax subset — tabstops `$1..$n`, `$0`,
  placeholders `${1:name}`; nested/choice/variables → BACKLOG. Tabstop navigation
  maps to multicursor selections (ADR-006 again)
- **Accept:** vim-feel test table (accept/dismiss/continue-typing); auto-import from
  rust-analyzer lands correctly; UI overhead gate < 5ms

### T11.3 — Hover & signature help
- Floating window component (shared with diagnostics-on-hover later): minimal
  markdown-to-grid renderer — headings, code fences (highlighted via onda-syntax!),
  bold/italic, lists; everything else rendered as plain text (scope locked)
- `K` hover, signature help auto-triggers on `(`/`,` in insert mode, dismiss rules
- **Accept:** rust-analyzer hover with code fence renders highlighted; float never
  steals focus or blocks input

### T11.4 — Navigation
- `gd` definition, `gD` declaration, `gr` references, `gi` implementation, `gy` type-def;
  single result jumps (pushes jumplist), multiple results open the location picker
  with preview pane
- **Accept:** cross-file `gd` in the fixture workspace; `Ctrl-o` returns correctly

### T11.5 — Rename & workspace edits
- `WorkspaceEdit` applier: groups edits per file, applies through Transactions
  (open buffers) or disk edit + reload (closed files), all-or-nothing with rollback
  on partial failure; summary message ("renamed in 7 files")
- This applier is **shared infrastructure** — Phase 4's agent diff-apply reuses it
  (note the interface in DESIGN §5.6 terms)
- **Accept:** rename a symbol used across 5 files (2 open, 3 closed); single undo
  per buffer restores; rollback test on injected failure

### T11.6 — Formatting
- `:format` + format-on-save (per-language config, default off in Phase 2); timeout
  (1s) → save proceeds unformatted with a notice (saving never hangs)
- Range formatting where supported
- **Accept:** ruff format + rustfmt-via-RA verified; timeout path tested

### T11.7 — Code actions (stretch — cut first if W14/W15 slip)
- `<space>a` code-action menu at cursor/selection; apply via the T11.5 applier
- **Accept:** rust-analyzer quickfix (e.g. add missing match arm) round-trips

---

## W12 — Server integrations & E2E verification (weeks 4–5)

### T12.1 — rust-analyzer
- Root detection incl. workspaces; `cargo check` diagnostics flow; inlay hints
  (type + parameter) behind config flag — render as virtual text (stretch within
  the task, cut to BACKLOG if needed)
- **Accept:** E2E suite: open fixture workspace → diagnostics, completion with
  auto-import, cross-crate gd, rename — all green in CI

### T12.2 — Python: basedpyright + ruff
- **venv auto-detection**: `.venv/`, `VIRTUAL_ENV`, `pyproject.toml` tool config →
  inject interpreter path into server settings; uv-managed projects covered
- Role split: basedpyright = types/completion/navigation; ruff = lint diagnostics +
  formatting; merged diagnostics show source labels
- **Accept:** E2E on fixture package with a venv: wrong-type diagnostic from
  basedpyright, lint from ruff, format-on-save via ruff, completion resolves venv deps

### T12.3 — taplo + JSON language server
- taplo: schema-aware completion/validation for `Cargo.toml`, `pyproject.toml`
- vscode-json-languageserver: JSON Schema Store mapping (`package.json`,
  `tsconfig.json`, …), `$schema` honored; JSONL stays on the Phase 1 per-line
  tree-sitter path (note: full JSONL record view is Phase 5)
- **Accept:** typing a bad key in `Cargo.toml` flags inline; schema completion works

### T12.4 — E2E test harness in CI
- `xtask e2e`: drives a headless onda (NullBackend) through scripted scenarios against
  the pinned real servers; golden assertions on editor state, not raw LSP traffic
- Flake policy: E2E failures quarantine to a retry lane, never silently skipped
- **Accept:** suite runs < 5 min in CI; a deliberate server-version bump that changes
  behavior is caught

---

## W13 — Workspace tools (weeks 3–4)

### T13.1 — Live grep
- `<space>/`: spawn `rg --json`, stream results into the picker as they arrive,
  regex + smart-case toggles, preview pane with syntax highlighting, accept → jump
  (jumplist push); graceful "install ripgrep" notice if missing
- **Accept:** grep over a 100k-file tree streams without UI stall; cancel kills rg

### T13.2 — Symbol pickers
- Document symbols (`<space>s`) with hierarchy flattening + kind icons; workspace
  symbols (`<space>S`) with query-as-you-type forwarding (`workspace/symbol`)
- **Accept:** symbol jump in fixture workspace; stale-version safety per T10.3

---

## W14 — Integrated terminal (weeks 3–5)

### T14.1 — PTY & VT state machine
- `portable-pty` for spawn/resize/lifecycle; `alacritty_terminal` crate as the VT
  parser/grid state (**we do not write an escape-sequence parser** — AGENTS.md gets
  a "common wrong" entry for this); PTY reader on the runtime, grid deltas to the
  compositor via the normal channel path
- **Accept:** `htop`, `git log` with pager, true-color test script all render right;
  bursty output (`yes`) cannot starve editor input (read throttling)

### T14.2 — Terminal UX
- Open as split or floating (`:term`, `<C-`>` toggle); Terminal-Insert mode (keys go
  to PTY) vs Normal mode over scrollback (full motions/search/yank over the terminal
  grid — vim users expect this); `<C-\><C-n>` to escape, mode shown in statusline
- cwd: new terminals start at project root; `:term!` at current buffer's dir
- Send-to-terminal: visual selection → `:send` (REPL workflow)
- **Accept:** run pytest in the terminal, yank a failing test name from scrollback
  in Normal mode, picker-jump to that test — full loop without leaving onda

---

## W15 — Auto-session L1 (`onda-session`, weeks 4–5)

### T15.1 — Session store & save
- Session key: hash of canonicalized git root (fallback cwd) →
  `~/.local/share/onda/sessions/<key>/session.toml`
- Persist (L1 scope per DESIGN §5.8): buffer list + focus, window split layout,
  per-window cursor/scroll, jumplist, search history, named registers (text size cap)
- Save on clean exit + debounced idle snapshot (crash resilience); atomic writes
- **Accept:** kill -9 during editing → reopen restores last snapshot (≤ idle interval old)

### T15.2 — Lazy restore
- Restore layout immediately; only the focused buffer's file is read on the critical
  path. Other buffers restore as placeholders (name + cursor metadata) hydrated on
  first focus via the async loader
- Invalidation: stored file mtime+hash mismatch → best-effort cursor (clamp to line),
  drop jumplist entries into that file
- `--no-session`, `:session-clear`; `onda <file>` = restore session + focus that file
- **Accept:** 30-buffer session: time-to-input < 40ms (gate); placeholder hydration
  imperceptible (< one frame stall budget)

---

## W16 — Integration, dogfooding & retro (weeks 6–7)

### T16.1 — Full-time dogfooding
- onda becomes the primary editor for onda development for ≥ 2 weeks (this overlaps
  W12–W15 tail); `DOGFOOD.md` triage cadence: blockers fixed same-week
- Target scenario from the design doc: real HTS-style workflow — Python + TOML +
  JSON in one session, venv respected, grep-navigate-edit-test loop
- **Accept:** one full workday log with zero editor-switching events

### T16.2 — Perf re-verification & report
- All gates re-run; `bench-compare` now includes nvim+lspconfig and helix with
  rust-analyzer attached; publish updated `BENCH_REPORT.md` + asciinema of
  completion latency side-by-side
- **Accept:** every Phase 0–2 gate green on macOS + Linux; report committed

### T16.3 — Retro & Phase 3 prep
- Sweep BACKLOG (snippets-full, code actions if cut, inlay hints, file-watch config
  reload, soft wrap decision point); draft `PHASE3_PLAN.md` (WASM plugin system:
  WIT API v0, wasmtime host, permissions, plugin manager, 3 reference plugins);
  tag `v0.0.3-phase2`
- **Accept:** Phase 3 plan drafted; WIT API sketch reviewed against the T11.5
  workspace-edit and picker component interfaces (plugins will want both)

---

## Suggested order for Claude Code

```
T10.0 → T10.1 → T10.2 → T10.3 → T11.1 → T10.4 → T11.2 → T15.1 → T11.4 →
T11.3 → T14.1 → T11.5 → T11.6 → T12.1 → T12.2 → T13.1 → T14.2 → T12.3 →
T13.2 → T15.2 → T12.4 → T11.7(stretch) → T16.x
```

Rationale: T10.1–10.3 before any feature UI (everything rides the request manager);
diagnostics (T11.1) first among features — it exercises the full pipeline with the
least UI; session save (T15.1) early because dogfooding wants it; E2E harness (T12.4)
after two real servers are integrated so the harness is shaped by reality.

## Phase 2 risks

| Risk | Mitigation |
|---|---|
| UTF-16 offset bugs (the classic LSP failure mode) | T10.2 property tests against a text-echo mock server; conversion utils isolated + fuzzed |
| stdio deadlocks with chatty servers | Dedicated pump tasks + bounded channels designed in T10.1, chaos-tested |
| Snippet/markdown/code-action scope explosion | Each has an explicit "minimal subset, rest → BACKLOG" line in its task; T11.7 is the designated cut |
| rust-analyzer warmup noise breaking bench gates | Gates measure warm-server state; warmup excluded and reported separately |
| Terminal emulator rabbit hole | alacritty_terminal adopted, never hand-rolled; correctness scope = its test suite |
| 7 weeks is the longest phase — drift risk | W11 feature-complete checkpoint at end of week 5; W12 verification cannot be compressed — cut T11.7 instead |
