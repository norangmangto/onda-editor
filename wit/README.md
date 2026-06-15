# onda plugin host API (WIT) — v0 design review

**Status:** `@unstable` v0.1.0. Breaking changes allowed until the Phase 5 release
freeze (DESIGN §8). **Decision: WASM Component Model, not Lua (ADR-002).**

This is the design-review document for PHASE3 W17 (T17.1/T17.2/T17.3): every
interface with a *why*, plus the binding non-goals. The WIT lives in
`wit/onda/{world,host,guest,types}.wit`; the manifest + permission model live in
the `onda-plugin` crate.

## Why each interface exists

| Interface | Plugins… | Why it's in v0 |
|---|---|---|
| `log` | notify / debug-log | Minimal feedback surface; debug goes to the log file, never the screen (rule 2). |
| `buffer` | read slices/lines, `apply` edits | The core surface. **All mutation is transactional** (ADR-005): one `apply` = one `ChangeSet` = one undo step, mapped through the same path as LSP/user edits. No raw rope access. |
| `selection` | get/set multi-range selection | ADR-006: selection is always 1..N ranges. Plugins must round-trip the full set, never assume one cursor. |
| `editor` | window cursor, mode (read), focus | Editor-level state a decoration/command plugin needs without touching buffers. |
| `commands` | register `:name` | Lets a plugin add commands; the guest exports the handler (`run-command`). |
| `keymap` | register keybindings | Same pattern as commands; dispatch goes to `run-keymap`. |
| `decorations` | **batch** virt-text / signs / highlights | THE perf-critical surface (T17.2). Namespaced *batch replace* — one call per frame's worth, never per-item — so the compositor diffs O(changed cells). |
| `ui` | float / picker / statusline segment | Reuses the Phase 1 picker component; picker result returns via an event. No arbitrary windows. |
| `config` | typed config reads | Read merged config.toml (global + project, §5.7). No writes. |
| `fs` *(capability)* | read/write/list under preopens | **Only linked if the manifest declares paths AND the user approved.** `cap-std` preopens; `..` escapes rejected by the host. |
| `http` *(capability)* | GET/POST to allowlisted domains | **Only linked if `network = true` + approved.** Host-mediated, no raw sockets. |

Guest exports (`guest.wit`): `init`, `handle-event`, `run-command`, `run-keymap`.
Handlers run on the main thread inside a 5ms epoch deadline (T18.2).

## Non-goals (v0, binding)

- **No blocking host calls.** Every import is a snapshot read or a queued
  transaction — never an await point that stalls the frame (rule 2 + ADR-002).
- **No raw buffer/rope access.** Mutation is transactional; positions are char
  indices (no line/col footguns).
- **No arbitrary UI** beyond float/picker/statusline.
- **No raw fs/net in the core API.** Only the capability-gated `fs`/`http`
  interfaces, wired in solely on manifest declaration + user approval.
- **No shell.** v0 has no shell capability variant at all (DESIGN §5.5).

## Manifest (`onda-plugin.toml`)

```toml
[plugin]
name = "git-blame-inline"
version = "0.1.0"
entry = "plugin.wasm"
min-api-version = "0.1"   # optional; defaults to host version

[permissions]
buffer = "read"           # none | read | write   (default: none)
filesystem = ["./.git"]   # project-root-relative whitelist (default: [])
network = false           # default: false
shell = false             # ignored — v0 always denies shell

[activation]
events = ["buffer-open", "cursor-hold"]   # lazy activation; protects startup
```

Schema + validation: `onda-plugin::manifest`. Permission resolution (request ∩
user-grant, with `..`-escape rejection): `onda-plugin::permission`.

## Status / sequencing

- ✅ **W17:** WIT surface, manifest schema, permission model, host-call queue.
- ✅ **W18:** `wasmtime` engine + instance lifecycle, `bindgen!` host functions
  (WIT validated by both wit-bindgen guests and the wasmtime host), epoch budgets,
  memory limit, capability link-time wiring. Integration-tested against real
  components.
- ✅ **W19:** `onda plugin install/list/remove` + lockfile (`manager.rs`).
  `update`/`dev --watch`/`cargo generate` template are follow-ups.
- ✅ **W20:** 3 reference plugins built as real WASM components under `plugins/`
  (todo-highlighter, git-blame-inline, http-client) + a hostile containment fixture.
- ✅ **Final swap:** `onda-lua` + `mlua` + `runtime/plugins/*.lua` removed; the
  binary drives `onda-plugin` (`PluginHost` in `crates/onda/src/plugin_host.rs`):
  startup discovery, event-driven activation, `PluginApiCall` applied between
  frames, `onda plugin install|list|remove` CLI. Decoration rendering + permission
  approval UI are the remaining follow-ups (see `docs/BACKLOG.md`).
