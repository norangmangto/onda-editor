# onda — Phase 6 Plan: IDE Shell & Croft-Parity (Draft)

**Status:** **DRAFT — needs user approval before starting.** Derived from the Croft /
Fresh competitive analysis (2026-06-15). Two items (DAP debugger, remote SSH) reverse
or pull forward previously-deferred decisions and are explicitly gated — see
*Decisions needed* below.
**Duration:** ~8 weeks | **Milestone:** onda is a *complete* terminal IDE — file tree,
command palette, full LSP UX, source-control and previews — without losing vim-first
speed or the plugin/agent platform.
**Design doc:** `docs/DESIGN.md` v0.3 | **Agent rules:** `AGENTS.md` | **Prereq:** Phase 5 green

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
                          └─► W36 LSP UX ────────────────┘                         W40 Env parity (gated)
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

## W38 — Source-control surface (weeks 5–6)
- **T38.1** SCM panel: changed files, stage/unstage/discard, hunk view, diff, commit —
  built on the Phase 3 git plugin (`git-blame-inline` → grow into `git`) via the W37 UI
  API and a host `vcs` interface. **No libgit2 in the editor core.**
- **Accept:** stage→commit a change entirely from the SCM panel.

## W39 — Rich content previews (weeks 6–7)
- **T39.1** Terminal graphics layer: detect & use kitty graphics / iTerm2 OSC 1337 /
  sixel; **plain-terminal fallback** to a metadata card. Inline images + PDF page render.
- **T39.2** Wire previews for image/PDF buffers; reuse the existing CSV/JSONL data views.
- **Accept:** open an image and a PDF inline on a supporting terminal; graceful
  degradation verified on a non-graphics terminal.

## W40 — Environment parity (gated, weeks 7–8)
- **T40.1 (gated)** Remote SSH editing (`russh` + `russh-sftp`) — currently a backlog
  item; large and needs a live host. Only if approved.
- **T40.2** Language coverage: decide whether to widen syntax/LSP beyond rust/python
  (Croft does py/rust/c/c++); add grammars + servers per the matrix.
- **Accept:** (if approved) edit a file over `scp://`; (always) the language matrix is
  explicit and documented.

## Decisions needed before starting (do not enact silently)

These touch earlier product decisions; they need the user's explicit go-ahead:

1. **DAP debugger** — deliberately removed this session (reclassified to post-v0.1
   backlog), but it is one of Croft's headline features. Options: (a) keep deferred,
   (b) bring back as a **plugin** over a host debug API (preferred — fits ADR-002),
   (c) re-add to the core. Not scheduled above until decided.
2. **Remote SSH (W40.1)** — was Phase 3 backlog; large. Confirm before scheduling.
3. **Language expansion (W40.2)** — Phase 1 narrowed to rust/python on purpose.
4. **Scope vs v0.1** — this phase is post-v0.1 (Phase 5 ships v0.1); confirm sequencing.

## Phase 6 risks

| Risk | Mitigation |
|---|---|
| IDE chrome erodes the perf budget | `layout_reflow_ms` gate + damage-only pane toggles (ADR-004); chrome is opt-in |
| Becoming a mouse-first VS Code clone (identity loss) | Keyboard-first is an exit criterion; mouse always optional |
| UI host API churn locks plugins in | Keep `@unstable`; design review (T37) before SCM/preview build on it |
| SCM/preview as plugins feel second-class | Host UI API must be first-class enough that the reference SCM matches a built-in feel |
