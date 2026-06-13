# AGENTS.md — onda

Instructions for AI coding agents (Claude Code, etc.) working on this repository.
**Read this fully before writing any code. These rules are not suggestions.**

## What onda is

onda is a Rust modal editor/IDE (vim-like keybindings, own ecosystem). Performance is the
core philosophy: **onda must be as fast as or faster than Neovim, always.** Design doc:
`docs/DESIGN.md` (architecture decisions are recorded as ADR-001..009 — do not violate
them without updating the doc first).

## Non-negotiable rules

### 1. Performance gates
- Run `cargo xtask bench` before every commit that touches `onda-core`, `onda-modal`,
  `onda-render`, or the main event loop. A regression > 5% vs `bench/baseline.json` is a
  **blocker** — fix it or explicitly justify and update the baseline in the same PR.
- Performance budgets (enforced by CI):
  - Cold startup: **< 40ms**
  - Keypress → render (p99): **< 10ms**
  - 1GB file open: **< 2s**, scrolling stays at 60fps
  - Idle memory (empty buffer): **< 40MB RSS**
- Never claim a perf improvement without a benchmark number in the PR description.

### 2. The main thread never blocks
- No file I/O, no network, no process spawning, no lock that can be held by a worker,
  no unbounded loops on the main event-loop thread.
- Anything potentially slow goes to the tokio runtime and communicates back via channels.
  The main loop drains channels once per frame — workers never call into editor state.
- If syntax/decoration data isn't ready within the frame budget (16ms), render without it.
  Input latency beats decoration consistency.

### 3. Architecture boundaries
- `onda-core` is the single source of truth for text state. All mutations go through
  `Transaction`/`ChangeSet` — never mutate rope content directly. LSP sync, undo, and
  plugin notifications all consume the same ChangeSet.
- `Selection` is always 1..N ranges (multicursor is first-class, ADR-006). Never write
  code that assumes a single cursor.
- Crate dependency direction: `onda` (bin) → feature crates → `onda-core`. Feature crates
  must not depend on each other unless documented in `docs/DESIGN.md` §6.
- Rendering uses damage tracking: mutate the cell grid, let the compositor diff. Never
  force a full redraw outside resize/theme-change.

### 4. Code standards
- `cargo fmt` and `cargo clippy --workspace --all-targets -- -D warnings` must pass.
- Libraries use `thiserror`; only the binary crate uses `anyhow`.
- No `.unwrap()` / `.expect()` outside tests and `xtask`, except for invariants —
  which require a `// INVARIANT:` comment explaining why it cannot fail.
- `unsafe` requires a `// SAFETY:` comment and reviewer sign-off; avoid it in Phase 0–2.
- New dependencies require justification in the PR description (binary size, compile
 time, and maintenance are costs). Pre-approved: ropey, crossterm, tokio, tracing,
 thiserror, anyhow, criterion, unicode-segmentation, unicode-width, tree-sitter,
 libloading, regex, nucleo-matcher, toml, notify, arboard, ignore,
 lsp-types, portable-pty, vt100, mlua (lua54 feature), serde_json, url, tempfile,
 git2 (libgit2 bindings — git status/diff/blame, Phase 3 W16),
 russh + russh-sftp (SSH transport for remote editing, Phase 3 W17),
 libvterm-sys (vendored libvterm FFI — terminal emulation, Phase 3 W17;
 `unsafe` FFI requires `// SAFETY:` comments per rule 4),
 dap-types (hand-rolled DAP JSON types acceptable, Phase 3 W15).
 Phase 4 (ACP agent integration): the `agent-client-protocol` crate is the upstream
 option but the spec is moving — onda **vendors** the ACP JSON-RPC types in `onda-agent`
 (no new external dep; built on the pre-approved serde/serde_json/tokio), with a thin
 adapter layer so the protocol surface can churn without touching the UI. The
 `mock-agent` test binary (in `onda-agent`) owns protocol conformance.

### 5. Testing
- Every motion/operator gets table-driven tests: `(input keys, before, after, selection)`.
- Core text operations get property tests (apply ChangeSet → invariants hold).
- Bug fixes land with a regression test in the same commit.
- Run `cargo test --workspace` before claiming a task done.

## Workflow for agents

1. Work on **one task ID** (from `PHASE0_PLAN.md`, `PHASE1_PLAN.md`, or `PHASE2_PLAN.md`) per session. Don't drift into
   adjacent tasks; note follow-ups in `docs/BACKLOG.md` instead.
2. Before coding: restate the task's acceptance criteria; list files you expect to touch.
3. Definition of done = acceptance criteria + tests + fmt/clippy + bench (if applicable).
4. Commit format: `feat(core): T1.2 changeset composition` — conventional commit +
   task ID. Small, reviewable commits.
5. If a task conflicts with this file or the design doc, **stop and ask** — do not
   silently reinterpret the architecture.

## Things agents commonly get wrong here

- Adding `std::fs` calls in event handlers (violates rule 2) — use the async worker.
- Treating selection as a single cursor (violates ADR-006).
- "Optimizing later": perf is verified per-commit, not per-phase.
- Full-screen redraws "to keep it simple" — the compositor exists; use it.
- Reaching for a heavyweight TUI framework — onda owns its compositor (ADR-004).

