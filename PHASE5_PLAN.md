# onda — Phase 5 Plan: Data-File Superpowers + Public Release

**Duration:** 5 weeks | **Milestone:** **v0.1.0 public release** — installable in one command, benchmarks published
**Design doc:** `docs/DESIGN.md` v0.3 §5.4.2, §5.8, §7 | **Agent rules:** `AGENTS.md` | **Prereq:** Phase 4 green
**Note:** re-validate at Phase 4 retro. This phase is half product (data views), half release engineering — don't let either half eat the other.

## Goal

Ship onda's differentiators and ship onda itself. The CSV virtual-table and JSONL
record views are features no mainstream editor has built-in — they're the launch
story for data-adjacent developers. The other half is making everything *installable,
documented, and trustworthy*: prebuilt grammars, packaging, docs site, and the public
benchmark report that backs the performance claim.

## Exit criteria

- [ ] CSV/TSV virtual table mode: aligned columns, pinned header, column-wise
      motions/selection, mismatch diagnostics — on a 1GB CSV within frame budget
- [ ] JSONL record view: line=record fold/expand, lazy per-line parsing, field table
      overlay — streams a 10GB file without loading it eagerly
- [ ] Session L2 (persistent undo) + named sessions shipped per DESIGN §5.8
- [ ] Theme system: TOML theme format, 3 built-in themes, runtime switching
- [ ] `onda doctor` diagnoses environment (terminal caps, grammars, servers, clipboard)
- [ ] Install paths verified on clean machines: `cargo install onda`, Homebrew tap,
      GitHub Releases (macOS universal + Linux musl static) with prebuilt grammars
- [ ] Docs site live (install, vim-users guide, plugin book, agents guide, config ref)
- [ ] `BENCH_REPORT.md` v1.0: reproducible methodology, onda vs nvim vs helix, published
- [ ] All gates green; `v0.1.0` tagged; announcement posted

## Workstreams

```
W27 CSV table mode ──┐
W28 JSONL view ──────┼─► W30 Theme/polish ─► W31 Release engineering ─► W32 Launch
W29 Sessions L2 ─────┘        (W31 packaging spikes start week 1 in parallel — see risk table)
```

---

## T27.0 — Harness update (day 1)

- New fixtures: 1GB CSV (wide + narrow variants), 10GB JSONL (generated, not stored —
  `xtask gen-fixtures` streams it), malformed rows/records corpus
- New gates: table-mode scroll budget on 1GB CSV, JSONL time-to-first-record,
  persistent-undo load cost (must not violate the 40ms startup or lazy-restore gates)
- Release env: clean-machine VMs/containers for install verification (macOS runner +
  Ubuntu + Alpine for musl)
- **Accept:** fixtures + gates in CI; release-verification workflow skeleton exists

## W27 — CSV virtual table mode (weeks 1–2)

### T27.1 — Detection & model
- Delimiter sniffing (`,` `\t` `;` `|` + quote rules, header heuristic) layered on the
  Phase 1 detection chain; manual override `:set csv-delim=`
- Column index built lazily per visible region + background completion for the file
  (worker); **the rope stays the source of truth** — table mode is a *view*, edits are
  normal text Transactions (no parallel data model to desync)
- **Accept:** sniffer test corpus (quoted commas, ragged rows, BOM, CRLF); index on
  1GB file builds in background without latency impact

### T27.2 — Table rendering
- Virtual alignment: cells padded to per-column display width at render time (rope
  text untouched); pinned header row; column separators; rainbow column tinting;
  ragged-row cells flagged inline + gutter diagnostic
- Wide-file handling: horizontal viewport over columns, current column highlighted,
  column ruler in statusline (`col 17/240 "user_id"`)
- **Accept:** golden-grid snapshots; 1GB scroll gate green; toggling `:table` on/off
  is instant (view-only switch)

### T27.3 — Column-aware editing
- Motions: next/prev cell (`<Tab>/<S-Tab>` in table mode), column top/bottom; text
  objects `ic/ac` (cell), `iC/aC` (column → multicursor over every row's cell, ADR-006
  again); column select → standard visual ops (yank column, delete column)
- Sort/preview operations → BACKLOG (read-mostly v0.1 scope; editing stays cell-local)
- **Accept:** "rename a value in every row of column 3" via `iC` + `c` test; vim test
  tables for cell motions

## W28 — JSONL record view (weeks 1–2, parallel)

### T28.1 — Streaming model
- Line-indexed lazy access: never parse beyond viewport + small lookahead; per-line
  parse results cached with invalidation on edit (ChangeSet-driven); parse errors are
  per-record diagnostics (line N: unexpected token), file stays editable as plain text
- 10GB handling: progressive load path from T4.2 extended — record count estimates,
  `G` jumps to tail without full parse
- **Accept:** 10GB fixture: time-to-first-record < 500ms; scroll + `G` within budget;
  editing a record mid-file keeps diagnostics consistent

### T28.2 — Record interaction
- Fold/expand per record: collapsed = single line with summary (first K fields),
  expanded = pretty-printed virtual view (read view virtual; `:record-edit` opens
  the pretty form in a scratch buffer, writes back minified on accept — round-trip
  preserves key order)
- Field table overlay: `:fields` shows union of keys across sampled records with
  types/counts — instant schema feel for unknown datasets
- **Accept:** expand/edit/write-back round-trip property test (parse→pretty→minify
  == semantically identical, key order kept); overlay on heterogeneous fixture

## W29 — Sessions L2 + named sessions (week 2–3)

### T29.1 — Persistent undo
- Per-file undo tree serialized (postcard blob) keyed by content hash + mtime;
  mismatch → discard silently (DESIGN §5.8 invalidation rule); size caps + LRU
  eviction across the store; opt-in config (`undo.persistent = true`), default off
  for v0.1 (flip decision at launch retro)
- Load is lazy (on first undo past session boundary) — protects startup gate
- **Accept:** edit→quit→reopen→`u` walks into the previous session's history;
  corrupted blob = clean fallback; gates unaffected

### T29.2 — Named sessions
- `:session save <name>` / `:session open <name>` / picker; `onda --session <name>`;
  named sessions are full L1 snapshots decoupled from the auto-session key
- **Accept:** branch-switch workflow test: save "feature-x", switch context, restore
  intact while auto-session of cwd keeps tracking separately

## W30 — Themes, doctor & polish (week 3)

### T30.1 — Theme system
- Theme TOML: palette + scope table (names frozen since T5.4) + UI elements (statusline,
  pickers, diffs, table mode); inheritance (`inherits = "onda-dark"`); runtime switch
  `:theme` with live preview in picker; 3 built-ins: onda-dark, onda-light, onda-wave
  (brand theme — ocean palette)
- **Accept:** snapshot per theme; theme hot-switch within one frame; theme docs page

### T30.2 — `onda doctor` & polish sweep
- Doctor checks: terminal capabilities (true color, kitty kbd, undercurl), grammar
  presence/build env, LSP servers on PATH + versions, ripgrep, clipboard provider,
  config parse status — with fix-it hints per failure
- Soft-wrap decision point (deferred since Phase 1): implement minimal soft wrap for
  prose/markdown **only if** DOGFOOD.md shows it as a recurring blocker; else document
  as known limitation in release notes (decide week 3, timebox 4 days if yes)
- **Accept:** doctor output on a broken env names every problem actionably

## W31 — Release engineering (weeks 3–5; packaging spikes from week 1)

### T31.1 — Artifacts & packaging
- Prebuilt grammars bundled per platform (the Phase 3 risk closes here); binaries:
  macOS universal2, Linux x86_64/aarch64 musl static; `cargo install onda` (decide
  crates.io grammar story: build-on-install via xtask hook vs `onda grammar fetch`
  first-run prompt — record as ADR-011); Homebrew tap `onda-editor/tap`
- Reproducible release workflow: tag → CI builds, checksums, signed (minisign),
  release notes generated from conventional commits
- **Accept:** clean-machine matrix (macOS, Ubuntu, Alpine): install → `onda doctor`
  green → open Rust project with LSP working, ≤ 5 minutes each

### T31.2 — Documentation site
- mdbook (or zola) site: install, **"onda for vim users"** migration page (what's
  identical, what's different, what's missing — honesty builds trust), config
  reference (generated from option definitions — single source), plugin book (P3),
  agents guide (P4), data-views guide (P5), FAQ ("why not helix/nvim?" — the three
  differentiators)
- **Accept:** docs build in CI; every config option in the reference is generated,
  not hand-listed

### T31.3 — Benchmark publication
- `BENCH_REPORT.md` v1.0: methodology (hardware, versions, run counts, how to
  reproduce — `xtask bench-compare` instructions), results vs nvim/helix across
  startup, input latency (plain + LSP-attached), large-file, memory; honest notes
  where onda loses or ties — credibility over marketing
- Asciinema set: 1GB file, completion latency, agent review loop, CSV table mode
- **Accept:** an outsider reproduces the numbers from the doc alone (±noise)

### T31.4 — Project hygiene
- LICENSE finalized (ADR-010), CONTRIBUTING.md (incl. AGENTS.md pointer — "agent PRs
  follow the same gates"), CODE_OF_CONDUCT, issue/PR templates, security policy,
  `good-first-issue` seeding from BACKLOG (10+ curated)
- **Accept:** repo passes a community-health checklist; templates route bench results

## W32 — Launch (week 5)

- **T32.1 Release candidate week:** feature freeze; RC builds to a handful of early
  testers (vim + data-eng profiles); blocker-only fixes; final gate run
- **T32.2 v0.1.0 + announcement:** tag + publish; announcement post structured around
  the three differentiators (vim keybindings · WASM plugins · first-class agent) +
  the benchmark report + the CSV/JSONL demo gifs; HN "Show HN", r/rust, r/vim,
  lobste.rs; Korean dev communities (GeekNews 등) — author's home turf
- **T32.3 Post-launch triage protocol:** 2-week issue-triage rotation plan, label
  taxonomy, "known limitations" pinned issue; launch retro → Phase 6 candidate list
  prioritized by real user feedback (DAP, GUI backend, own agent engine, inline
  ghost-text, plugin registry, Windows, hot exit)
- **Accept:** v0.1.0 live and installable; triage protocol survives week 1 contact
  with reality

## Phase 5 risks

| Risk | Mitigation |
|---|---|
| Packaging discovered late to be hard (grammar dylibs, musl, signing) | **Packaging spikes start week 1** in parallel with W27/W28; ADR-011 decision deadline = end of week 2 |
| Table/record views become editors-within-the-editor | Hard rule: rope is the only model, views are virtual; sort/transform ops explicitly BACKLOG'd |
| Persistent undo corrupts trust at launch | Default off for v0.1; opt-in with the discard-on-mismatch rule; flip decision deferred to post-launch data |
| Launch-day perf claims challenged | T31.3 reproducibility gate + honest-losses policy; respond with methodology, not adjectives |
| Solo triage burnout post-launch | T32.3 protocol with label taxonomy + pinned limitations; agent-assisted triage (Claude Code on issue summaries) |
