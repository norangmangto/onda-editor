# onda — Phase 6 Plan: IDE Shell & Croft-Parity

**Status:** Approved (decisions recorded below, 2026-06-15). Derived from the Croft /
Fresh competitive analysis.
**Duration:** ~8 weeks | **Milestone:** **v0.2** — onda is a *complete* terminal IDE:
file tree, command palette, full LSP UX, source-control, previews, and a debugger —
without losing vim-first speed or the plugin/agent platform.
**Design doc:** `docs/DESIGN.md` v0.3 | **Agent rules:** `AGENTS.md` | **Prereq:** Phase 5 green (v0.1)

## Why this phase

Benchmarking against **Croft** (a polished "VS Code in the terminal") and **Fresh**
(a mature zero-config terminal editor) showed onda's *engine* (text/LSP/terminal/
render/plugins/agent) is already strong, but it lacks the **IDE shell UX** that makes
Croft feel complete: a file-explorer sidebar, a command palette, a multi-pane layout,
full LSP interaction surfaces, a source-control view, and rich content previews.

**Guiding principle (keeps onda's identity):** absorb Croft's *shell completeness*,
but **keyboard-first**, and deliver the heavy/optional surfaces (source control,
previews, debugger) as **WASM plugins over a host UI-contribution API** (ADR-002),
not as hard-wired in-process features. Mouse is supported but never required.

## Exit criteria

- [ ] **File-explorer sidebar**: tree navigation, open/create/rename/delete, keyboard +
      mouse, lazy-loaded on large trees, optional git-status badges
- [ ] **Command palette**: every editor action is discoverable and runnable by fuzzy
      search; plugins contribute actions; `F1` opens a searchable keybinding reference
- [ ] **Multi-pane layout**: activity bar + sidebar + editor (splits) + terminal pane,
      keyboard-driven focus cycling, mouse drag-to-resize; render stays within budget
- [ ] **LSP UX complete**: rename-with-preview, code-actions menu, formatting,
      signature help, document-symbol breadcrumb/picker — all wired to the engine
- [ ] **Plugin host UI API**: plugins can contribute a sidebar tree view, a panel,
      palette items, and statusline segments (the surface SCM/preview build on)
- [ ] **Source-control surface**: status/stage/unstage/diff/commit, built on the
      Phase 3 git plugin + host UI API (no libgit2 in the editor core)
- [ ] **Rich previews**: inline images (kitty graphics / iTerm OSC 1337 / sixel
      fallback) and PDF; degrade gracefully on plain terminals
- [ ] All perf gates green: cold start < 40ms with the shell, keypress→render p99
      < 10ms with sidebar+palette open; **new gate:** layout reflow < 5ms

## Workstreams

```
T33.0 harness ─► W33 Layout/shell ─► W34 File explorer ─┐
                          └─► W35 Command palette ───────┼─► W37 Plugin UI API ─► W38 SCM ─► W39 Previews
                          └─► W36 LSP UX ────────────────┘
W40 DAP debugger (core) — independent, can run in parallel
```

---

## T33.0 — Harness update (day 1)
- New gate: `layout_reflow_ms < 5` (resize / pane toggle); re-run keypress p99 with the
  sidebar and palette open. No new heavyweight deps — the compositor owns layout (ADR-004).
- AGENTS "commonly wrong" addition: shell chrome must not force full redraws; pane
  toggles damage only the affected rects.
- **Accept:** gates wired; a benchmark fixture opens the shell and measures reflow.

## W33 — Multi-pane layout & shell (weeks 1–2)
- **T33.1** Layout engine: activity bar (mode switch: Explorer/Search/SCM/Run/Agent) +
  collapsible sidebar + editor area (existing splits) + bottom terminal pane. Pure
  layout tree over the existing compositor; damage-tracked.
- **T33.2** Focus model: keyboard cycle across panes (`<C-w>` family extended), a
  documented focus ring; mouse click-to-focus and drag-to-resize borders.
- **T33.3** Statusline/tabline polish: buffer tabs, diagnostics counts, mode, git branch
  segment hook.
- **Accept:** open/close/resize panes by keyboard and mouse; reflow gate green.

## W34 — File explorer sidebar (weeks 2–3)
- **T34.1** Tree model over the `ignore`-aware walker (lazy children; never blocks the
  main loop — directory reads go to the async worker, ADR rule 2).
- **T34.2** Actions: open, create file/dir, rename, delete (trash-safe), reveal-current;
  full keyboard map + mouse.
- **T34.3** Git-status badges via the Phase 3 git plugin / host vcs interface (optional).
- **Accept:** navigate and mutate a large repo tree from the keyboard; no frame stalls.

## W35 — Command palette & action registry (weeks 2–3)
- **T35.1** Central **action registry**: every command/keybinding registers a typed
  action with id + title + category; the existing `:` ex-commands and keymap feed it.
- **T35.2** Palette UI on the Phase 1 picker (`nucleo`): fuzzy search actions, recent,
  with keybinding hints; plugins contribute actions.
- **T35.3** `F1` keybinding reference (searchable, generated from the registry/keymap).
- **Accept:** any action runnable from the palette; reference lists all bindings.

## W36 — LSP UX completeness (weeks 3–4)
- **T36.1** Rename with preview (WorkspaceEdit applier already exists from Phase 2/4) +
  code-actions menu + formatting + signature help.
- **T36.2** Document/workspace symbol picker + breadcrumb.
- **Accept:** rename/code-action/format/symbols all driven from the UI against a real
  server (rust-analyzer); table-driven tests on the pure edit-application paths.

## W37 — Plugin host UI-contribution API (weeks 4–5)
- **T37.1** Extend `wit/onda` (still `@unstable`) so plugins can contribute: a sidebar
  **tree view**, a **panel**, **palette items**, and **statusline segments** — batched,
  non-blocking (ADR-002 + the no-blocking host rule).
- **T37.2** Capability + activation wiring for UI contributions; review doc.
- **Accept:** a sample plugin adds a sidebar view + palette item; perf budget held.

## W38 — Source-control surface (weeks 5–6) ✅
- **T38.1** SCM panel: changed files, stage/unstage/discard, hunk view, diff, commit —
  built on the Phase 3 git plugin (`git-blame-inline` → grow into `git`) via the W37 UI
  API and a host `vcs` interface. **No libgit2 in the editor core.**
- **Accept:** stage→commit a change entirely from the SCM panel.
- **Done (3050f7b):** Source Control sidebar view backed by the `git` CLI on a worker
  thread (`crates/onda/src/scm.rs`: porcelain parser + `run_git`/`status`). Lists
  changed files with status badges; `a` stage / `u` unstage / `c` commit / `R` refresh;
  command-palette entry. **Diverges from plan:** implemented via the `git` CLI subprocess
  rather than the W37 plugin `vcs` host interface (W37 callback contributions are still
  pending — see KNOWN_ISSUES); keeps libgit2 out of core as required. Hunk-level
  staging/discard + inline diff view deferred.

## W39 — Rich content previews (weeks 6–7)
- **T39.1** Terminal graphics layer: detect & use kitty graphics / iTerm2 OSC 1337 /
  sixel; **plain-terminal fallback** to a metadata card. Inline images + PDF page render.
- **T39.2** Wire previews for image/PDF buffers; reuse the existing CSV/JSONL data views.
- **Accept:** open an image and a PDF inline on a supporting terminal; graceful
  degradation verified on a non-graphics terminal.

## W40 — DAP debugger (core, weeks 1–3, parallel)
Restores the debugger as a **core feature crate** (`onda-dap`), the same architectural
tier as LSP (DESIGN §1.3 lists LSP in core; DAP is structurally identical — external
adapter process + Content-Length framed protocol + deep editor integration). A WASM
plugin can't own the adapter subprocess under the sandbox, so the engine belongs in the
host; see the decision rationale recorded below.

- **T40.1** Restore `onda-dap` (protocol/transport/session/client + `onda-mock-dap`) and
  wire it: `<F9>` breakpoints + gutter markers, `:DapRun`/`:DapStop`, F5/F10/F11/F12
  step control, stop-line marker, `:DapStack`/`:DapVars`/`:DapEval`, `dap.toml` config.
- **T40.2** Adapters documented: `lldb-dap` (rust/c/cpp), `debugpy` (python). Conformance
  via `onda-mock-dap` in CI; real adapters are manual targets.
- **T40.3 (hybrid extension)** Expose debug **state/events read-only** to plugins via the
  W37 host API later, so custom debug UIs are possible without putting the protocol in
  the sandbox. (Engine = core, surface = extensible.)
- **Accept:** mock-adapter E2E green; breakpoints/stepping/stack/vars/eval work; the
  `dap_on_keypress_p99_ms < 10` gate holds while a session is attached.

## Decisions (recorded 2026-06-15)

1. **DAP debugger → CORE.** Implemented as a feature crate alongside LSP (W40). Rationale:
   the adapter subprocess + transport must live host-side regardless (the WASM sandbox
   denies process spawning), so a plugin would only add a churny WIT surface on top of a
   host engine — strictly more work for little isolation benefit, and DAP is structurally
   the LSP twin (which is already core). Plugins may later consume debug state read-only
   (T40.3).
2. **Remote SSH → deferred to Phase 7.** Large, needs a live host.
3. **Language expansion (beyond rust/python) → deferred to Phase 7.**
4. **Sequencing:** this phase is **v0.2**, after Phase 5 ships v0.1.

> **Phase 7 (later):** remote SSH editing (`russh`/`russh-sftp`), language coverage
> expansion (c/c++/typescript/… grammars + servers).

## Phase 6 risks

| Risk | Mitigation |
|---|---|
| IDE chrome erodes the perf budget | `layout_reflow_ms` gate + damage-only pane toggles (ADR-004); chrome is opt-in |
| Becoming a mouse-first VS Code clone (identity loss) | Keyboard-first is an exit criterion; mouse always optional |
| UI host API churn locks plugins in | Keep `@unstable`; design review (T37) before SCM/preview build on it |
| SCM/preview as plugins feel second-class | Host UI API must be first-class enough that the reference SCM matches a built-in feel |
