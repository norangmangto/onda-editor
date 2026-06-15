# Salvage: Lua plugin host API surface

The former `onda-lua` crate (a sandboxed Lua 5.4 plugin runtime) was **removed**:
it violated **ADR-002** (the plugin runtime is WASM Component Model, not an embedded
single-language interpreter). The WASM system replaced it (`onda-plugin`,
`wit/onda/*.wit`).

This file records the **host API surface** the Lua runtime exposed, kept only as a
reference checklist when designing the WASM **WIT** host API (`docs/plan/PHASE3_PLAN.md`
W17). It is documentation, not code. The full Lua implementation remains in git
history at commit `f5d467d~1` (`crates/onda-lua/`).

## Execution model (for reference)

- Lua ran **between frames only**, under a per-frame microsecond budget; over-budget
  plugins were aborted. (WASM equivalent: epoch-based interruption, per-call time
  budgets — PHASE3 T18.2.)
- Lua never called editor state directly. Every mutation was enqueued as a
  `LuaApiCall` and drained by the main loop once per frame; reads were answered from
  a snapshot prepared before the Lua budget began. (Mirrors AGENTS.md rule 2 — the
  WASM host must keep the same no-blocking, transaction-queue discipline.)

## Host functions exposed (the `onda.*` global)

| Lua call | Purpose | Kind |
|---|---|---|
| `onda.notify(msg, level)` | show a message in the message line (`info`/`warn`/`error`) | enqueue |
| `onda.log(msg)` | convenience alias for `notify` at info level | enqueue |
| `onda.buf.get_lines(buf, start, end)` | read a line range from a buffer | sync read (snapshot) |
| `onda.buf.set_lines(buf, start, end, lines)` | replace a line range in a buffer | enqueue |
| `onda.win.get_cursor(win)` | get `{row, col}` of a window's cursor | sync read (snapshot) |
| `onda.win.set_cursor(win, {row, col})` | move a window's cursor | enqueue |
| `onda.keymap.set(mode, lhs, callback_id, opts)` | register a keybinding (`opts`: `noremap`, `silent`, `desc`) | enqueue |
| `onda.cmd.create(name, callback_id, opts)` | register a custom `:command` (`opts`: `nargs`, `desc`) | enqueue |
| `onda.autocmd.create(event, pattern, callback_id)` | register an autocommand | enqueue |
| `onda.ui.float(opts)` | open a floating window (`opts`: `title`, `lines`, `width`, `height`) | enqueue |
| `onda.highlight.set(group, opts)` | define/override a theme highlight group (`opts`: `fg`, `bg`, `bold`, `italic`; colors `#rrggbb` or ANSI names) | enqueue |

Plugin entry points were callbacks referenced by `callback_id` (keymaps, commands,
autocmds) invoked by the editor when the event fired.

## Sandbox restrictions (deny-list, for reference)

The Lua VM stripped dangerous stdlib before loading any plugin:

- `io.open` / `io.lines` / `io.popen` / `io.tmpfile`
- `os.execute` / `os.remove` / `os.rename` / `os.exit` (and `os.getenv` unused)
- the entire `debug` library (reflection escape hatch)
- `loadfile` / `dofile` / `load` (arbitrary code from the filesystem)
- `package.loadlib` and `ffi`
- `require` replaced by a whitelist: `string`, `table`, `math`, `utf8`, `bit32`

The WASM model supersedes this deny-list with **capability-based** enforcement
(cap-std fs preopens, network/shell deny-by-default, manifest-declared permissions —
PHASE3 T17.3 / T18.3): absence of a capability is the enforcement, rather than
stripping a shared interpreter.
