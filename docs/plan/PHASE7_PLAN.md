# onda — Phase 7 Plan: Remote & Polyglot

**Status:** Draft for approval (2026-06-16). Continues Phase 6 (v0.2, W33–W40) and the
items it deferred. Workstream IDs continue at **W41**.
**Duration:** ~7 weeks | **Milestone:** **v0.3** — onda edits **remote** projects over
SSH with a **live LSP** and speaks the **mainstream languages**, without losing
vim-first latency or the plugin/agent platform.
**Design doc:** `docs/DESIGN.md` | **Agent rules:** `AGENTS.md` | **Prereq:** Phase 6 green (v0.2 tagged)

## Why this phase

v0.2 made onda a complete *local* terminal IDE. Two gaps keep it from being a daily
driver for real work:

1. **It isn't actually talking to language servers.** `onda-lsp` is built and tested but
   the binary never spawns one (KNOWN_ISSUES) — diagnostics, hover, go-to-def, rename and
   the W36 UX are all dormant. This is the highest-leverage fix in the project: it lights
   up features already written.
2. **It's local-only and effectively bilingual.** Croft/VS Code's pull is *remote
   development* and *every language working out of the box*. Phase 6 deferred both
   (decisions 2 & 3). Remote SSH editing + a real language matrix is the v0.3 story.

Phase 7 also completes the plugin **UI-contribution** surface (the W37 residual: a
plugin-owned sidebar tree/panel) and clears the smaller debts that accumulated while
shipping fast (permission UI, lazy activation, visual-mode dot-repeat, sixel/PDF).

## Exit criteria

- [ ] **Live LSP** in the binary: server lifecycle (spawn/`did_open`/debounced
      `did_change`/`did_close`), diagnostics in the gutter, and hover / go-to-definition /
      references / format / rename / code-action / document-symbol driven from the UI
      against **rust-analyzer** and **one more** real server.
- [ ] **Remote editing over SSH**: connect (`:remote <user@host[:path]>`), browse + open +
      save files over SFTP, with the **editor never blocking on the network** (keypress→
      render p99 < 10ms holds while remote); reconnect is graceful.
- [ ] **Remote language tooling**: LSP (and DAP where available) run **on the remote host**
      over an SSH exec channel; diagnostics/hover work on a remote file.
- [ ] **Language matrix**: TypeScript/JavaScript, C/C++, Go, plus JSON/YAML/TOML/Markdown
      — tree-sitter highlighting + an LSP config each; text objects for the C-family and
      TS where the grammar supports it.
- [ ] **Plugin sidebar contribution**: a sample plugin adds a **sidebar tree view** and a
      **panel** via a new `@unstable` `wit/onda` surface; debug state is exposed read-only
      to plugins (T40.3). Compiled sample plugin exercises it in CI.
- [ ] **Debts cleared**: interactive plugin permission approval; lazy-by-event plugin
      activation; visual-mode `.`-repeat; sixel image previews + PDF page render (if a
      pre-approved rasterizer lands — else explicitly re-deferred).
- [ ] All perf gates green incl. new remote budgets; `v0.3.0` tagged; docs updated
      (remote guide, language matrix, plugin tree/panel book chapter).

## Workstreams

```
W41 LSP base wiring ──┬─► W42 Language matrix ──┐
                      └─► W44 Remote LSP/DAP ────┤
W43 Remote core (SSH/SFTP) ───────────► W44 ─────┼─► W46 Polish + release v0.3
W45 Plugin contribution surface (parallel) ──────┘
```

`W41` unblocks both the language matrix (W42) and remote tooling (W44). `W43` (transport)
and `W45` (plugin surface) can start in parallel from day 1.

---

## T41.0 — Harness update (day 1)
Restate acceptance per task; add the Phase 7 perf gates to `bench/baseline.json` +
`xtask` (see Decisions §perf). Wire the new `lsp_roundtrip_ms` and `remote_open_ms`
benches (mock server / loopback SSH) so they're enforced, not aspirational (the existing
`dap_on_keypress_p99_ms` placeholder pattern is the model — make these real).

## W41 — Live LSP in the binary (weeks 1–2)
Closes the KNOWN_ISSUES blocker; turns dormant W36 code on.
- **T41.1** Spawn `LspManager` in `run_editor`; map language → server from config
  (`languages.toml`). On file open: `ensure_server` + `did_open`; on edit: debounced
  `did_change` with version numbers; on close/quit: `did_close`/`shutdown`.
- **T41.2** Sync→async **request dispatch + `request_id` correlation**: a request queued
  from the main loop, the response delivered back as a `BgMessage` and applied between
  frames (rule 2). Diagnostics drain into `diagnostic_spans` (already rendered).
- **T41.3** Bind the interactions to keys/commands using the **already-built** appliers
  (`lsp_edit`, format/rename results): hover, go-to-definition, references, `:Format`,
  rename-with-preview, code-action menu, document-symbol picker, signature help.
- **Accept:** against real `rust-analyzer`: diagnostics appear; hover/def/refs/format/
  rename/code-action/symbols work from the UI. Pure paths stay table-tested; a `mock-lsp`
  (mirror of `onda-mock-dap`) drives lifecycle + dispatch in CI (no real server in CI).

## W42 — Language matrix (weeks 2–4)
- **T42.1** Grammars: TypeScript/JavaScript, C, C++, Go, JSON, YAML, TOML, Markdown —
  fetched/prebuilt via the Phase 5 grammar pipeline; highlight queries vendored.
- **T42.2** `languages.toml`: per-language server command + args + root markers + file
  associations (rust-analyzer, tsserver/`typescript-language-server`, clangd, gopls,
  `vscode-json-languageserver`, `yaml-language-server`, `taplo`, `marksman`). Documented;
  servers are user-installed (onda configures, doesn't bundle).
- **T42.3** Text objects (`if`/`af`/`ic`/`ac`/`ia`/`aa`) for the C-family + TS/JS via
  tree-sitter queries, reusing the Phase 1/2 query-driven object engine (narrow, per
  DESIGN; don't regress rust/python).
- **Accept:** open a `.ts`, `.c`, `.go` file → correct highlighting + a working server
  (diagnostics/hover) given the server installed; `if`/`af` select functions in each.

## W43 — Remote editing core: SSH + SFTP (weeks 1–4, parallel)
New feature crate **`onda-remote`** (`russh` + `russh-sftp`, pre-approved). Feature crate
→ `onda-core`; **all transport on the tokio runtime**, results via channels (rule 2).
- **T43.1** Connection manager: `:remote <user@host[:port][/path]>`; auth via agent /
  key file / known_hosts verification (prompt on unknown host key). One multiplexed SSH
  session per host; reconnect with backoff.
- **T43.2** Remote FS over SFTP: directory listing (feeds the **Explorer** tree),
  `open` (read into a **local-authoritative** rope), `save` (atomic remote write),
  rename/delete. The editor edits the local rope; the network is only touched on
  open/save/list — **never on keypress**.
- **T43.3** A `RemoteFs` abstraction the Explorer + `:e`/`:w` + picker route through, so
  remote vs local is a backend choice, not a UI fork. Path display shows `host:/path`.
- **Accept:** connect to a host (loopback `sshd` in CI), browse, open + edit + save a
  file; keypress→render p99 < 10ms throughout; killing the connection mid-session
  surfaces an error and offers reconnect without losing the buffer.

## W44 — Remote language tooling (weeks 4–5)
- **T44.1** SSH-exec transport backend for `onda-lsp` (and `onda-dap`): the server runs
  **on the remote host** (where the toolchain is), framed protocol tunneled over an exec
  channel. The W41 dispatch is transport-agnostic; this adds a remote transport impl.
- **T44.2** Path translation: LSP/DAP speak remote absolute paths ↔ onda's `host:/path`
  buffers; the `lsp_edit` char-offset conversion is unchanged (positions are per-buffer).
- **Accept:** open a remote rust file → rust-analyzer running on the remote host yields
  diagnostics + hover; a remote breakpoint stops (where a remote adapter exists).

## W45 — Plugin contribution surface completion (weeks 1–5, parallel)
Finishes the W37 residual and the plugin debts.
- **T45.1** Extend `wit/onda` (`@unstable`) with a **sidebar tree view** + **panel**
  contribution (declarative model + update calls), draining non-blocking like the W37
  callbacks (`pack_handle` attribution). Render in a plugin-owned `SidebarView`.
- **T45.2** **T40.3**: expose **read-only debug state** (stack/vars/breakpoints/events) to
  plugins via host getters — enables custom debug UIs without protocol in the sandbox.
- **T45.3** Interactive **permission approval** (install-time + first-use prompt; replaces
  `discover`'s auto-grant) and **lazy-by-event activation** (manifest command pre-scan →
  instantiate on first `:name`). Multi-key plugin `lhs`.
- **T45.4** A compiled **sample plugin** (in-repo source + build step) that adds a sidebar
  tree + panel + palette item + keymap — the W37/W45 CI conformance artifact.
- **Accept:** the sample plugin's tree + panel render and update; permission prompt gates
  an ungranted capability; a command-activated plugin instantiates lazily; perf held.

## W46 — Polish, debts & release v0.3 (weeks 5–7)
- **T46.1** Visual-mode `.`-repeat (KNOWN_ISSUES); revisit the float-based `:DapStack`/
  `:DapVars` now that the Run panel exists (keep or retire).
- **T46.2** Previews: **sixel** encoder + **PDF page render** *iff* a pre-approved
  rasterizer/encoder dep is agreed (needs user sign-off per rule 4); otherwise re-defer
  explicitly and keep the metadata card.
- **T46.3** Host `vcs` interface (real blame source) + `http` host impl (currently
  v0-stub) — backlog items that unblock richer plugins.
- **T46.4** Docs: remote guide, language matrix reference, plugin tree/panel book chapter;
  refresh `BENCH_REPORT.md` with remote numbers; `onda doctor` checks SSH + per-language
  servers.
- **Accept:** all gates green (incl. remote budgets); `v0.3.0` tagged; docs published.

## Decisions (proposed — confirm before building)

1. **LSP base wiring is Phase 7's first task, not a side quest.** It's a prerequisite for
   both the language matrix and remote tooling, and it activates code already written.
2. **Remote model = local-authoritative buffers + SFTP file transport, language servers
   run remote over SSH-exec.** The rope lives locally so keypresses never touch the
   network (rule 2); servers live where the toolchain is. A remote **headless onda
   daemon** (VS Code Server–style) is explicitly *out of scope* for v0.3 — revisit in
   Phase 8 if file-transport proves insufficient.
3. **onda configures servers; it does not bundle them.** Grammars are prebuilt (Phase 5
   pipeline); LSP/DAP servers are user-installed and discovered via `languages.toml` +
   `onda doctor`. Keeps binary size and licensing clean.
4. **New ADRs need user sign-off (AGENTS rule 3).** This plan likely warrants ADRs for
   the *remote architecture* and the *plugin UI-contribution model*; those will be
   **proposed for explicit approval**, not added unilaterally.
5. **Perf gates (new, enforced):** `lsp_roundtrip_ms` (mock server, p99) < 50ms;
   `remote_open_ms` (1MB over loopback SSH) < 300ms; **keypress→render p99 < 10ms must
   hold while remote + LSP are attached** (the headline invariant). Cold-start budget
   (< 40ms) unchanged — remote/LSP init must stay off the startup path (lazy).

## Risks

| Risk | Mitigation |
|---|---|
| Network I/O leaks onto the main thread (rule 2 violation) | `onda-remote` is async-only; the editor talks to it via channels; a CI test asserts no blocking call on the open/save/keypress paths |
| LSP debounce/version bugs corrupt sync | Reuse the ChangeSet/`rev` source of truth; `mock-lsp` drives version correlation in CI; property-test the offset conversions |
| Remote reconnect loses edits | Local-authoritative rope means edits survive a dropped link; save retries; never discard a dirty buffer on disconnect |
| Language matrix balloons scope | Grammars + config + (narrow) text objects only — **not** bespoke per-language features; servers are user-installed |
| Plugin UI surface churn locks plugins in | Keep `wit/onda` `@unstable`; ADR + design review (decision 4) before the sample plugin ships against it |
| SSH auth/host-key UX is a security footgun | Verify host keys against `known_hosts`, prompt on unknown, never auto-accept; document the threat model |

## Deferred to Phase 8 (recorded)
- Remote **headless onda daemon** (if file-transport remote proves insufficient).
- Collaborative / multi-cursor-over-network editing.
- Notebook (`.ipynb`) editing surface; richer data views beyond CSV/JSONL.
- Windows-native polish (Phase 7 targets macOS + Linux remote first).
