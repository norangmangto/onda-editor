# onda — Phase 3 Plan: WASM Plugin System

**Duration:** 6 weeks | **Milestone demo:** an outsider writes a working plugin from the docs alone
**Design doc:** `docs/DESIGN.md` v0.3 §5.5 | **Agent rules:** `AGENTS.md` | **Prereq:** Phase 2 green
**Note:** Phases 3–5 plans are written ahead of time; re-validate scope at the Phase 2 retro (T16.3) before starting.

## Goal

Make onda a platform. A WASM Component Model plugin system where third parties extend
onda safely (sandboxed, permissioned), in multiple languages, **without the ability to
hurt performance**: lazy activation protects startup, per-call time budgets protect the
frame, and crashes are isolated.

The most important deliverable is not code — it's the **WIT API surface**. It will be
marked `unstable`, but every name we choose here is a future compatibility cost. Design
reviews on the WIT files outrank implementation speed this phase.

## Exit criteria

- [ ] `wit/onda/*.wit` host API v0 published with versioning policy (`@unstable`)
- [ ] Plugins load via wasmtime, activate lazily on declared events, and are killed/
      demoted when exceeding their time budget — proven by a hostile test plugin
- [ ] Permission model enforced: fs whitelist (preopens), network/shell deny-by-default,
      user approval UI on install + on first privileged call
- [ ] `onda plugin install github:<user>/<repo>` / `update` / `list` / `remove` work
      with a lockfile; `onda plugin dev --watch` hot-reloads during development
- [ ] 3 reference plugins shipped: git-blame-inline, todo-highlighter, http-client
- [ ] Startup with 10 installed (inactive) plugins: still < 40ms (gate)
- [ ] Plugin-call overhead benchmarked and documented (host-call round trip target
      < 50µs; decoration batch path amortized)
- [ ] **External-tester gate:** one person outside the project builds a plugin using
      only `docs/plugin-book/` — friction log captured

## Workstreams

```
T17.0 harness ─► W17 WIT API design ─► W18 Runtime host ─► W19 Manager & DX ─► W20 Reference plugins ─► W21 Verification
                        (W18 can start on transport while W17 iterates on surface)
```

---

## T17.0 — Harness update (day 1)

- Pre-approved deps: `wasmtime` (component model on), `wit-bindgen`, `wasmparser`,
  `cap-std`; pin wasmtime major version, record upgrade policy in AGENTS.md
- New gates: startup-with-plugins, host-call overhead microbench, time-budget
  enforcement test (a busy-loop plugin must be demoted, frame budget intact)
- AGENTS.md "commonly wrong" additions: *never* expose a host function that can block;
  *never* pass raw buffer access — everything goes through the transaction API
- **Accept:** hostile-plugin fixture exists and CI proves containment

## W17 — WIT API v0 (weeks 1–2, design-review heavy)

### T17.1 — Core interfaces
- `onda:plugin/host` — what plugins import: buffer ops (read slices, apply
  transactions — mirrors `onda-core` semantics incl. multi-range selections),
  selection get/set, command + keymap registration, event subscription
  (buffer-open/save/change, cursor-hold, mode-change), config read (typed getters)
- `onda:plugin/guest` — what plugins export: `init`, event handlers, command handlers
- All buffer mutation flows through transactions; positions are `(char-idx)` with
  helpers — no line/col footguns in v0
- **Accept:** WIT compiles; design review doc lists every interface with a "why" and
  an explicit non-goals section (no UI windows in v0, no direct fs/net in core API)

### T17.2 — UI & decoration interfaces
- Decorations: virtual text, gutter signs, highlights — **batch API** (one call per
  frame worth of decorations, not per-item; this is the perf-critical surface)
- Picker contribution: plugins can open a picker with items + callbacks (reuses the
  Phase 1 component); statusline segment registration
- **Accept:** decoration batch round-trip microbench within target; API review signed

### T17.3 — Capability & manifest schema
- `onda-plugin.toml` final v0 schema (per DESIGN §5.5): permissions (buffer r/w, fs
  path whitelist, network bool, shell bool), activation events, min-api-version
- Capability tokens: privileged WIT interfaces (`fs`, `http`) are only wired into the
  instance if the manifest declares + user approved — absence is enforcement
- **Accept:** schema documented; a plugin requesting undeclared capability fails to
  link, with a clear error

## W18 — Runtime host (`onda-plugin`, weeks 2–4)

### T18.1 — Engine & instance lifecycle
- wasmtime engine: component model, epoch-based interruption for time budgets,
  per-plugin store with memory limits; instances created lazily on first matching
  activation event, torn down on unload
- Host functions implemented over channels to the editor core (rule 2: a plugin call
  that needs editor state gets a snapshot or queues a transaction — never locks)
- **Accept:** lifecycle tests; memory-bomb plugin hits its limit and is unloaded
  with a user-visible notice, editor unaffected

### T18.2 — Scheduling & budgets
- Sync event handlers get an epoch deadline (default 5ms); exceeding → handler
  suspended and demoted to async completion + warning; repeated offenders disabled
  per session (three-strikes, mirrors LSP backoff pattern)
- Async tasks for long work: plugins request a task handle, report progress,
  results land as events next frame
- **Accept:** busy-loop and slow-IO hostile plugins both contained; latency gate green
  with 5 active plugins

### T18.3 — Permission enforcement & approval UX
- fs: cap-std preopens limited to manifest whitelist (paths resolved against project
  root; `..` escapes rejected); network: host-mediated http interface with domain
  allowlist from manifest; shell: v0 = denied always (revisit Phase 6)
- Approval UI: on install show a permission summary (style: short, concrete — "can
  read files under ./.git"); privileged first-use prompts allow once / always / deny,
  persisted per plugin+capability in state dir
- **Accept:** escape-attempt test suite (path traversal, symlink, undeclared domain)
  all blocked; approval persistence round-trips

## W19 — Plugin manager & developer experience (weeks 3–5)

### T19.1 — Install & lifecycle commands
- `onda plugin install github:user/repo[@rev]`: fetch (git, shallow), read manifest,
  prefer prebuilt `plugin.wasm` from releases, else build (`cargo component build`)
  with user consent; verify wasm is a component + api-version compatible
- `update` (respects lockfile + manifest semver), `list`, `remove`; lockfile committed
  per-user (`~/.config/onda/plugins.lock`)
- **Accept:** full install→use→update→remove cycle on the reference plugins from
  a clean machine

### T19.2 — Developer loop & docs
- `cargo generate onda-editor/plugin-template` (Rust); `onda plugin dev --watch`:
  rebuild + hot-reload instance on change, plugin logs streamed to a scratch buffer
- `docs/plugin-book/`: quickstart (blank → virtual text in 15 minutes), API reference
  generated from WIT doc-comments, permission guide, performance guide (budgets,
  batch decorations), one non-Rust walkthrough (JS via componentize-js or Python
  via componentize-py — pick the more mature toolchain at build time, the other →
  BACKLOG)
- **Accept:** quickstart timed at ≤ 15 min by someone who didn't write it

## W20 — Reference plugins (weeks 4–5)

- **T20.1 git-blame-inline** — cursor-hold → async `git blame` (via host fs read of
  `.git`? No — blame needs git; expose it through a host `vcs` interface instead of
  shell. Decide in W17 review; if cut, blame reads via the fs interface on
  `.git` internals are out — fallback: host-provided `vcs.blame()` API). Validates:
  async tasks, virtual text, fs/vcs permissions
- **T20.2 todo-highlighter** — buffer-change → batched decorations on TODO/FIXME/HACK
  with config-driven keywords. Validates: event flow, decoration batching, config API
- **T20.3 http-client** — `:http` command + picker UI to send requests from a buffer
  (rest-client style), responses to a scratch buffer. Validates: network permission
  UX end-to-end, picker contribution, command registration
- **Accept (all):** installable via T19.1 from their own repos; each plugin doubles
  as an integration test in CI

## W21 — Verification & freeze review (week 6)

- **T21.1 External-tester gate:** recruit one outside dev; they build a plugin from
  docs alone; friction log → fixes for blockers within the week
- **T21.2 API review:** walk every WIT interface against the friction log + reference
  plugin experience; rename/reshape now (still `@unstable`); document the road to
  v1 stability (planned for Phase 5 release notes as "unstable, will break")
- **T21.3 Perf + retro:** full gate run with all reference plugins active; sweep
  BACKLOG; tag `v0.0.4-phase3`
- **Accept:** exit criteria checklist green; Phase 4 plan re-validated

## Phase 3 risks

| Risk | Mitigation |
|---|---|
| WIT API designed around Rust-only ergonomics | Non-Rust walkthrough (T19.2) forced before freeze review; component model keeps ABI language-neutral |
| Component-model toolchain churn (wasmtime/wit-bindgen majors) | Versions pinned day 1; upgrade policy in AGENTS.md; vendor WIT deps |
| Host API accidentally enables blocking | T17.0 rule + review checklist item; every host fn audited for await-points |
| git-blame plugin forces a shell/vcs decision late | Decision explicitly scheduled in W17 review (host `vcs` interface vs cut) |
| "Platform" scope creep (themes-as-plugins, LSP-as-plugins…) | v0 non-goals section in T17.1 is binding; extensions → BACKLOG with API-version gates |
