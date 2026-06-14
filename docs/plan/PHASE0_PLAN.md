# onda — Phase 0 Plan: Skeleton + Performance Harness

**Duration:** 3 weeks | **Milestone demo:** open a 1GB file and scroll it faster than nvim
**Design doc:** `docs/DESIGN.md` v0.3 | **Agent rules:** `AGENTS.md`

## Goal

> "An empty shell, but the fastest modal editor in the world."

Phase 0 builds the minimal editable editor **and** the measurement infrastructure that
enforces the performance philosophy forever after. The harness (W0) is not overhead —
it is the product guarantee. Every later phase inherits these gates.

## Exit criteria (all must hold on macOS + Linux)

- [ ] `onda <file>` opens, edits with core vim motions/operators, saves, quits
- [ ] Cold startup < 40ms (hyperfine, release build)
- [ ] Keypress → render p99 < 10ms (built-in latency tracer)
- [ ] 1GB synthetic file: open < 2s, scrolling at 60fps
- [ ] CI green: fmt, clippy `-D warnings`, tests, cargo-deny, **bench regression gate**
- [ ] `BENCH_REPORT.md` comparing onda vs nvim vs helix published in repo

## Workstreams & dependency order

```
W0 Harness ──► W1 Core ──► W2 Modal ──┐
         └───► W3 Render ─────────────┴──► W4 Integration & Demo
```

W0 lands first (T0.1–T0.3 in days 1–3). W1/W3 can proceed in parallel afterwards.
W2 depends on W1. W4 stitches everything.

---

## W0 — Repository & Harness (week 1, highest priority)

### T0.1 — Workspace scaffold
Create the cargo workspace exactly as `docs/DESIGN.md` §6:
`crates/{onda,onda-core,onda-modal,onda-render,onda-config}` (others are empty stubs
with a README), plus `xtask/`, `bench/`, `docs/`, `runtime/`.
- `rust-toolchain.toml` (pin stable), `rustfmt.toml`, `clippy.toml`, `deny.toml`
- License: Apache-2.0 OR MIT dual (decide once, record as ADR-010 in design doc)
- `cargo build --workspace` succeeds; binary prints version and exits
- **Accept:** fresh clone → `cargo xtask ci` runs fmt+clippy+test locally

### T0.2 — Agent harness
- Add `AGENTS.md` (provided) at repo root; symlink `CLAUDE.md -> AGENTS.md` so Claude
  Code picks it up regardless of which filename it prefers
- `.github/PULL_REQUEST_TEMPLATE.md` with mandatory sections: *Task ID*, *Bench results*
  (or "N/A — no hot path touched"), *New dependencies + justification*
- `docs/BACKLOG.md` (empty, for agent follow-up notes)
- **Accept:** Claude Code session started in repo root cites the perf budgets unprompted

### T0.3 — CI pipeline (GitHub Actions)
- Jobs on macOS + Linux: fmt check, clippy `-D warnings`, `cargo test --workspace`,
  `cargo deny check`, release build artifact upload
- **Accept:** intentionally bad commit (fmt violation) fails CI

### T0.4 — Benchmark harness (`cargo xtask bench`)  ← the heart of Phase 0
- **Startup bench:** hyperfine wrapper measuring `onda --bench-startup` (init everything,
  render one frame to a null backend, exit). Warmup runs, JSON output
- **Input latency tracer:** feature-gated instrumentation in the event loop recording
  t(key event received) → t(frame flushed); `onda --bench-replay <keys.json>` replays a
  scripted editing session against a synthetic buffer, reports p50/p95/p99
- **Large-file bench:** generate synthetic files (100MB/1GB text, 100k-line Rust source)
  via `xtask gen-fixtures`; measure open time + scripted scroll frame times
- **Memory bench:** RSS after startup with empty buffer
- `bench/baseline.json` committed; `cargo xtask bench --check` exits non-zero on >5%
  regression. CI job runs the check on every PR (Linux runner; thresholds account for
  runner noise — use medians of 10 runs)
- **Accept:** deliberately add a 5ms sleep in the render path → CI fails

### T0.5 — Comparison bench vs nvim/helix
- `xtask bench-compare`: same fixtures driven through `nvim --headless` + `--startuptime`
  and helix where comparable; emits `BENCH_REPORT.md` table
- Run weekly via scheduled CI, not per-PR (external tools = noise)
- **Accept:** report generates locally on the Mac Mini with nvim installed

### T0.6 — Observability
- `tracing` + `tracing-subscriber`, `ONDA_LOG=debug` env filter, log file in state dir
- Debug overlay (feature flag): frame time, damage cell count in the corner
- **Accept:** frame-time overlay visible with `--features debug-overlay`

---

## W1 — Core text engine (`onda-core`, week 1–2)

### T1.1 — Document & buffer
- `Document` wrapping `ropey::Rope`; open (UTF-8, lossy fallback with warning flag),
  save (atomic write: temp file + rename), line-ending detection (LF/CRLF) preserved
  on save
- Position types: `CharIdx`, `(line, col)` conversions; grapheme-aware column movement
  (`unicode-segmentation`)
- **Accept:** round-trip property test — open + save with no edits = byte-identical;
  1GB fixture opens within budget

### T1.2 — Transactions & ChangeSet
- `ChangeSet` = ordered retain/insert/delete ops; `Transaction` = ChangeSet + selection
  mapping; composition (`a.compose(b)`) and position mapping (`map_pos`)
- Every mutation of a Document goes through `Document::apply(Transaction)` — enforce by
  making rope mutation private to the crate
- **Accept:** property tests — compose associativity, apply(a then b) == apply(a.compose(b)),
  positions map correctly across random edits

### T1.3 — Selection model (multicursor-ready, ADR-006)
- `Selection { ranges: SmallVec<Range>, primary: usize }`; `Range { anchor, head }`
- Normalization: sort + merge overlapping ranges after every transform
- All motion/operator APIs take `&Selection` and return a new `Selection` — **never a
  single cursor** (see AGENTS.md rule 3)
- **Accept:** unit tests for merge/normalize; API review confirms no single-cursor paths

### T1.4 — Undo/redo (linear for now)
- Stack of inverse Transactions with edit-grouping (insert-mode run = one undo step)
- Designed so Phase 1 can swap in a tree without API change (`UndoHistory` trait)
- **Accept:** scripted edit session — undo/redo restores exact text + selection

---

## W2 — Modal engine (`onda-modal`, week 2)

### T2.1 — Mode state machine & key parsing
- Modes: Normal / Insert / Visual / VisualLine / Command
- Keymap trie supporting multi-key sequences (`gg`), counts (`3w`, `2dd`), pending
  operator state (`d` waiting for motion), `<Esc>` cancels any pending state
- Keymaps defined as data (static table) — Phase 1 will load from TOML; don't hardcode
  dispatch in match arms scattered around
- **Accept:** table-driven tests for sequences incl. counts, cancellation, invalid keys

### T2.2 — Motions
- `h j k l`, `w b e` (word, vim word rules), `W B E` (WORD), `0 ^ $`, `f t F T` + `; ,`,
  `gg G`, `{ }` (paragraph), `Ctrl-d/u` (half page)
- Motions are pure functions `(text, range, count) -> range`, applied per-selection-range
- Vertical movement keeps goal column (sticky column across short lines)
- **Accept:** test table ports a subset of vim's documented motion semantics; all pass
  with 1 and N cursors

### T2.3 — Operators & edits
- `d c y` composed with any motion; `p P` (charwise + linewise registers), `x s`,
  `dd yy cc` linewise, `o O`, `a A i I`, `r`, `J`
- Single unnamed register for Phase 0 (named registers = Phase 1)
- Each operator application = one Transaction (clean undo grouping)
- **Accept:** `d3w`, `c2fx`-style compositions tested; multicursor `d` produces correct
  result on overlapping-adjacent ranges

### T2.4 — Visual modes
- `v` / `V`: extend selection with motions, operators consume the selection, `o` swaps
  anchor/head. (Visual-block `Ctrl-v` deferred to Phase 1 — note in BACKLOG)
- **Accept:** `vjjd`, `Vp` behave like vim

### T2.5 — Command line
- `:` mode with line editor (insert, backspace, history of current session)
- Commands: `:w [path]`, `:q`, `:q!`, `:wq`, `:e <path>`, `:bn/:bp` (buffer cycle)
- **Accept:** modified-buffer guard — `:q` refuses with message, `:q!` forces

---

## W3 — Rendering (`onda-render`, week 1–2, parallel with W1)

### T3.1 — Terminal backend
- crossterm: raw mode, alternate screen, panic hook that restores the terminal
  (a panicking editor must never leave the shell broken), resize events,
  kitty keyboard protocol detection with graceful fallback, true-color detection
- `Backend` trait with `TerminalBackend` + `NullBackend` (for benches/tests)
- **Accept:** force a panic → terminal restored; resize redraws correctly

### T3.2 — Cell grid & damage compositor
- `Grid` of `Cell { grapheme, style }`; double buffer; diff produces minimal spans;
  flush batches cursor moves + style changes into one write
- Unicode width handling (`unicode-width`): CJK double-width, tabs expanded per config
- **Accept:** golden tests — scripted edits produce expected diff spans (not full rows);
  bench shows single-char edit flushes O(1) cells

### T3.3 — Document view
- Viewport with vertical scrolling + scrolloff; horizontal scroll for long lines
  (no soft wrap in Phase 0 — BACKLOG); relative+absolute line numbers; selection &
  multicursor highlighting (primary vs secondary style); visible cursor per mode
  (block/bar via DECSCUSR)
- Rendering reads rope slices lazily — only visible lines are touched (this is what
  makes the 1GB demo work)
- **Accept:** 1GB fixture scrolls at 60fps with damage overlay showing bounded work

### T3.4 — Statusline & messages
- Statusline: mode indicator, filename, modified flag, position, percentage
- Message line for command feedback / errors (the `:q` guard message lands here)
- **Accept:** visual snapshot tests via NullBackend grid dumps

---

## W4 — Integration & demo (week 3)

### T4.1 — Event loop & frame scheduling
- Main loop: crossterm EventStream → keymap → editor core → compositor → flush;
  coalesce burst input (paste, key repeat) into one frame; 16ms frame budget with
  the latency tracer hooks from T0.4
- tokio runtime started but only hosting the (currently trivial) file I/O worker —
  the channel architecture from DESIGN §4 exists from day one, even if underused
- **Accept:** paste of 10k chars renders in one frame; latency bench passes budget

### T4.2 — Large-file hardening
- Profile the 1GB path end-to-end (open, jump `G`, scroll, edit at end, save)
- Async file loading: show the first screen as soon as the head of the file is read
  (progressive load), don't block the loop on the full rope build
- **Accept:** time-to-first-frame on 1GB file < 300ms; full load < 2s; edit+save works

### T4.3 — Bench report & demo
- Run `xtask bench-compare`, commit `BENCH_REPORT.md`, record a terminal cast
  (asciinema) of the 1GB side-by-side vs nvim
- **Accept:** every Phase 0 exit criterion checked off with numbers

### T4.4 — Retro & Phase 1 prep
- Sweep `docs/BACKLOG.md` into Phase 1 plan; note any AGENTS.md rules that caused
  friction and tune them; tag `v0.0.1-phase0`
- **Accept:** Phase 1 plan drafted with the same task-ID structure

---

## Suggested Claude Code workflow

```
# one task per session, in dependency order
claude "Implement T0.1 per docs/PHASE0_PLAN.md. Restate acceptance criteria first."
# before accepting any hot-path change:
cargo xtask bench --check && cargo xtask ci
```

Recommended order: T0.1 → T0.2 → T0.3 → T1.1 → T3.1 → T0.4 (needs a binary that
starts) → T1.2 → T1.3 → T3.2 → T2.1 → T2.2 → T1.4 → T2.3 → T3.3 → T2.4 → T2.5 →
T3.4 → T0.5 → T0.6 → T4.x
