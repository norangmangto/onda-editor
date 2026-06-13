# onda — Design Document v0.3

## 1. Vision

onda is a modal terminal editor/IDE written in Rust. Performance is the core philosophy:
onda must be as fast as or faster than Neovim, always. The architecture is designed to
enforce this guarantee at the CI level.

## 2. Performance budgets

| Metric | Budget | Enforcement |
|---|---|---|
| Cold startup | < 40ms | `cargo xtask bench --check` |
| Keypress → render p99 | < 10ms | latency tracer in event loop |
| 1GB file open | < 2s | bench fixture |
| 1GB file scroll | 60fps | bench fixture |
| Idle RSS (empty buffer) | < 40MB | bench |

## 3. Architecture overview

```
┌─────────────────────────────────────────────────────────┐
│                     onda (binary)                        │
│  EventLoop  ─►  Modal Engine  ─►  Core  ─►  Renderer    │
│      │               │              │          │         │
│   crossterm        keymap       Document     Grid        │
│   EventStream      motions      Rope         Backend     │
│      │             operators   Selection      │          │
│      │                │        Transaction  Terminal     │
│   tokio channel ◄─────┘            │                    │
│   (file I/O,        Undo        ChangeSet                │
│    future LSP)    History                                │
└─────────────────────────────────────────────────────────┘
```

## 4. Channel architecture (§4)

The main event loop is single-threaded. Background work (file I/O, future LSP, syntax
highlighting) runs in the tokio thread pool and communicates back via a `mpsc` channel.

```
MainLoop ◄── mpsc::Receiver<BgMessage> ◄── tokio::spawn(workers...)
```

Workers never hold references into editor state. They communicate back with owned data.
The main loop drains the channel once per frame. If a worker is slow, the loop renders
without its results.

## 5. Data flow for an edit

```
KeyEvent → modal_engine.process(key) → Transaction → document.apply(tx) → undo_history.push(inverse_tx)
                                                   └→ selection updated
→ compositor.mark_dirty() → frame_flush()
```

## 6. Crate structure (§6)

```
onda (bin)
├── onda-core    — Document, Rope, Transaction, Selection, Undo
├── onda-modal   — Mode, Keymap, Motions, Operators, Command-line
├── onda-render  — Backend, Grid, Compositor, View, Statusline
└── onda-config  — Config deserialization (stub in Phase 0)
```

Dependency direction: `onda` → `{onda-modal, onda-render}` → `onda-core`.
`onda-modal` and `onda-render` must not depend on each other.

Feature crates added in later phases follow the same rule (each may depend on
`onda-core`, never on a sibling feature crate; the binary wires them together):
`onda-syntax`, `onda-lsp`, `onda-terminal`, `onda-session`, `onda-lua`, and — Phase 3 —
`onda-git` (libgit2 status/diff/blame; depends only on `git2`, runs all work on a
dedicated worker thread). Tree-sitter text objects (T18.2) live in `onda-syntax`;
`onda-modal` only names the `TextObj` variants and the binary resolves them.

## 7. Architecture Decision Records

### ADR-001: Rope data structure
**Decision:** Use `ropey` for the text buffer.
**Rationale:** O(log n) insert/delete/slice; handles 1GB+ files; pure Rust; actively maintained.
**Alternatives considered:** Gap buffer (simpler but poor random-access), Vec<Line> (catastrophic
for large files).

### ADR-002: Terminal backend — crossterm
**Decision:** Use `crossterm` for terminal I/O; own the compositor.
**Rationale:** Cross-platform, actively maintained, supports kitty keyboard protocol.
**Alternatives considered:** termion (Unix-only), ratatui (opinionated layout we don't need).

### ADR-003: Async runtime — tokio
**Decision:** Use tokio for background I/O.
**Rationale:** File loading must not block the event loop. tokio gives us a thread pool
with structured cancellation for free. Only used for truly background work.

### ADR-004: Own compositor (no TUI framework)
**Decision:** onda owns its cell grid and damage compositor.
**Rationale:** TUI frameworks (ratatui, tui-rs) impose layout abstractions that add latency
and prevent fine-grained damage tracking. We need O(changed cells) flush cost.

### ADR-005: Transaction-based mutation
**Decision:** All rope mutations go through `Transaction`/`ChangeSet`.
**Rationale:** Single point of truth for LSP sync, undo, plugin notifications, and selection
mapping. Direct rope mutation is private to `onda-core`.

### ADR-006: Multicursor as first-class (Selection = 1..N ranges)
**Decision:** `Selection` always holds 1..N `Range` values. Single cursor = Selection with
one range where anchor == head.
**Rationale:** Retrofitting multicursor later requires touching every motion/operator. Doing
it from day one costs nothing extra in the single-cursor case.

### ADR-007: Damage-tracking compositor
**Decision:** Double-buffer the cell grid; flush only changed cells.
**Rationale:** Terminal I/O is the dominant cost in the render path. Minimising writes
directly translates to latency headroom.

### ADR-008: Pure motion functions
**Decision:** Motions are `fn(text, range, count) -> Range`, not methods that mutate state.
**Rationale:** Composable, testable in isolation, easy to apply per-cursor.

### ADR-009: Static keymap tables in Phase 0
**Decision:** Keymaps are static data tables, not loaded from config files.
**Rationale:** Phase 0 goal is correctness and performance, not configurability. Phase 1
will add TOML loading using the same table schema.

### ADR-010: License — Apache-2.0 OR MIT
**Decision:** Dual license Apache-2.0 OR MIT.
**Rationale:** Standard Rust ecosystem dual license. Compatible with all pre-approved
dependencies. Maximally permissive for downstream users.
