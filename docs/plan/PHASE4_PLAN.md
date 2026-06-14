# onda — Phase 4 Plan: AI Agent Integration (ACP)

**Duration:** 5 weeks | **Milestone demo:** implement a feature via the agent panel — prompt → plan → diff review → apply — without leaving onda
**Design doc:** `docs/DESIGN.md` v0.3 §5.6 | **Agent rules:** `AGENTS.md` | **Prereq:** Phase 3 green
**Note:** re-validate at Phase 3 retro; ACP is an evolving spec — pin versions and re-check the protocol surface before starting.

## Goal

Replace the "Claude Code in a tmux split" workflow with a first-class agent experience:
onda speaks ACP (Agent Client Protocol) to external agents — Claude Code first — with a
streaming panel, hunk-level diff review, a permission gate for tool use, and editor
context (`@file`, `@selection`, `@diagnostics`) flowing to the agent.

Architecture stance from ADR-003: onda's own future agent engine (Phase 6+) will
implement the same ACP interface — so everything built here is the permanent UI; the
adapter layer is the only part that may churn with the spec.

## Exit criteria

- [ ] Claude Code connects via ACP: sessions, streaming responses, tool-call events,
      cancellation, reconnect after agent restart
- [ ] Agent panel renders streaming markdown (incl. code fences highlighted by
      onda-syntax) with **zero frame drops** during bursts (gate: panel-stream bench)
- [ ] Diff review: agent-proposed edits land as a reviewable changeset — hunk-level
      accept/reject, preview in real buffers, apply through the shared WorkspaceEdit
      applier (T11.5) as one undo step per buffer
- [ ] Permission gate: every agent tool request (write file, run command) prompts
      allow-once / always / deny; "always" rules persisted per agent+tool+scope;
      denials returned to the agent cleanly
- [ ] Context mentions: `@file`, `@selection`, `@diagnostics`, `@buffer` resolve and
      attach correctly; agent edits to open buffers respect unsaved state
- [ ] Mock-agent E2E suite in CI (protocol conformance + UI flows); Claude Code
      smoke test documented as a manual release check
- [ ] Editing latency gates unaffected while an agent streams (the panel is just
      another async worker)

## Workstreams

```
T22.0 harness ─► W22 ACP client core ─► W23 Agent panel UI ─► W24 Diff review & permissions ─► W25 Context integration ─► W26 Verification
```

---

## T22.0 — Harness update (day 1)

- Pre-approved deps: `agent-client-protocol` (pin exact version; spec is moving),
  fallback plan documented if the crate lags the spec (vendored types)
- Mock ACP agent binary in `xtask/mock-agent`: scriptable scenarios (streaming text,
  tool calls, permission requests, malformed messages, mid-stream death)
- New gates: panel-stream frame budget under burst (10k tokens/s synthetic), editing
  latency with active stream
- **Accept:** mock agent drives a scripted session in CI headlessly

## W22 — ACP client core (`onda-agent`, weeks 1–2)

### T22.1 — Transport & session lifecycle
- Spawn agent subprocess (command from config: `claude-code acp` etc.), JSON-RPC over
  stdio (reuse the pump architecture from T10.1 — same deadlock-safe pattern),
  initialize/capability negotiation, session create/resume, graceful + crash teardown
- Agent registry in config: named agents with command/args/env; `:agent <name>` to
  pick; multiple configured, one active per session panel (multi-panel → BACKLOG)
- **Accept:** lifecycle vs mock agent incl. kill-mid-stream → panel shows
  disconnected state, reconnect resumes a fresh session; garbage messages isolated

### T22.2 — Protocol surface
- Handle: streaming assistant message chunks, agent plan/thought events (if exposed),
  tool-call begin/update/end, permission request/response, file read/write requests
  (served from buffer state when the file is open — agents must see unsaved edits),
  cancellation (user `<Esc>`/stop button → protocol cancel)
- Version/capability gating: unknown message types logged + skipped, never fatal
- **Accept:** conformance tests against mock scenarios; file-read returns dirty
  buffer content (test with unsaved edit)

## W23 — Agent panel UI (weeks 2–3)

### T23.1 — Panel & streaming renderer
- Right-side split panel (toggle `<space>aa`), conversation thread view: user msgs,
  streaming assistant text via the T11.3 markdown-to-grid renderer (extended for
  incremental append — re-render only the tail), tool-call cards (name, args summary,
  collapsible output, status spinner), plan display
- Burst handling: chunks coalesced per frame; renderer works on the damage budget
- Input box at panel bottom (multi-line, `<CR>` send / `<S-CR>` newline), history
- **Accept:** panel-stream gate green; 200-message thread scrolls within budget;
  snapshot tests for tool-card states

### T23.2 — Session UX
- Thread management: new/clear/resume (where agent supports), session list picker;
  transcript persisted to state dir (plain text export `:agent-export`)
- Statusline integration: agent busy indicator + active tool name
- **Accept:** restart onda → previous transcript viewable (read-only) even if the
  agent session itself can't resume

## W24 — Diff review & permission gate (weeks 3–4) ← the heart of Phase 4

### T24.1 — Proposed-change staging
- Agent file edits do **not** hit buffers directly: they accumulate in a staging
  changeset (per session) keyed by file, rebased on top of concurrent user edits
  where clean (conflict → marked stale, agent notified on apply attempt)
- Review entry: `:agent-review` or panel button → review mode
- **Accept:** agent edit + concurrent user edit on the same file: clean-rebase case
  applies; conflicting case flags the hunk stale instead of corrupting

### T24.2 — Hunk review UI
- Review mode: file list sidebar + diff view (deleted/added line styling via
  compositor), per-hunk `a`ccept / `r`eject / `e`dit-then-accept, accept-file,
  accept-all; progress indicator (3/7 hunks)
- Apply path: accepted hunks → T11.5 WorkspaceEdit applier (one undo step per buffer,
  all-or-nothing with rollback); rejected hunks reported back to the agent as context
- **Accept:** scripted multi-file review E2E; undo after apply restores pre-agent
  state per buffer; reject feedback visible in mock-agent log

### T24.3 — Permission gate
- Tool permission requests render as an inline panel card: tool, target (path/command),
  diff-preview when it's a write; allow-once / allow-always / deny (+ deny-always)
- Persistence: rules stored per agent + tool + scope pattern (e.g. "writes under
  src/ : always") in state dir; `:agent-permissions` picker to review/revoke
- Safety defaults: shell-execution requests always prompt unless an explicit always-
  rule exists; no blanket "always allow everything" shortcut in the UI
- **Accept:** rule persistence round-trips; revoke takes effect immediately;
  UX test: a deny mid-plan leaves the agent in a consistent state

## W25 — Context integration (week 4)

### T25.1 — Mentions & context assembly
- Input-box mentions with picker-backed completion: `@file` (fuzzy file picker),
  `@buffer` (open buffers), `@selection` (last visual selection w/ file+range),
  `@diagnostics` (current buffer or workspace, severity-filtered)
- Context attached via ACP's context/resource mechanism; size guards (line caps with
  truncation notice) so a fat `@file` can't blow the prompt silently
- **Accept:** each mention type round-trips to mock agent with correct content +
  metadata; oversize file truncates with visible notice

### T25.2 — Terminal context (resolves DESIGN §9.2-2)
- `@terminal`: attach visible scrollback (or last N lines) of the integrated terminal —
  the failing-pytest-output → agent loop without copy-paste
- Explicit mention only in this phase (no automatic terminal observation — privacy +
  noise; auto-capture → BACKLOG with an opt-in design)
- **Accept:** run failing test in W14 terminal, `@terminal` carries the traceback;
  decision recorded in DESIGN changelog

## W26 — Verification & polish (week 5)

- **T26.1 Claude Code real-world gauntlet:** five scripted real tasks on the onda
  repo itself (add a config option, fix a seeded bug, write tests, refactor, docs);
  measure: review-loop friction, latency, permission UX annoyance; fix blockers
- **T26.2 E2E + docs:** mock-agent suite in CI (protocol, review flows, permissions);
  `docs/agents.md` user guide (setup per agent, mentions, permission model);
  manual release-check checklist for live Claude Code
- **T26.3 Perf re-verification & retro:** all gates incl. stream-while-editing;
  sweep BACKLOG (multi-panel, auto terminal context, inline ghost-text edits —
  note: inline completion ghost text is a *different feature track*, record as a
  candidate Phase 6 item); re-validate Phase 5 plan; tag `v0.0.5-phase4`
- **Accept:** exit criteria green; asciinema of the full prompt→review→apply loop

## Phase 4 risks

| Risk | Mitigation |
|---|---|
| ACP spec / crate churn | Pinned versions + thin adapter layer (T22.x isolates protocol from UI); mock agent owns conformance |
| Agent edits racing user edits | Staging changeset + rebase design (T24.1) is the core invariant — built first, chaos-tested |
| Permission UX so annoying users click "always" everywhere | Scope-pattern rules (per-directory) make "always" safe-ish; gauntlet (T26.1) explicitly measures prompt fatigue |
| Streaming markdown renderer perf | Incremental tail-append design + coalescing; gate exists from day 1 |
| Claude Code behavior changes between releases | Smoke test is a documented manual release check; CI relies on mock agent only |
