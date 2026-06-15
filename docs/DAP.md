# Debugging (DAP) — onda

onda speaks the [Debug Adapter Protocol](https://microsoft.github.io/debug-adapter-protocol/)
to external debug adapters. Set breakpoints, run to them, step, and inspect the call
stack and variables without leaving the editor (Phase 3 W15).

The adapter runs as a subprocess on a background worker; the main loop never blocks
on it (AGENTS.md rule 2). The wire protocol lives in the `onda-dap` crate; the
`onda-mock-dap` adapter exercises the full protocol in CI, while `lldb-dap` and
`debugpy` are the real targets.

## Setup

Install an adapter and (optionally) configure it in `~/.config/onda/dap.toml`
(see `runtime/dap.toml` for a template):

- **Rust / C / C++** — `lldb-dap` (ships with LLVM; `xcrun -f lldb-dap` on macOS).
  Set `program` to your built binary, e.g. `target/debug/my-bin`.
- **Python** — `debugpy` (`pip install debugpy`). `program` defaults to the current
  file.

onda has built-in defaults for both; `dap.toml` overrides or adds adapters by `name`.

## Workflow

1. Open a source file and place breakpoints with **`<F9>`** (toggles the current
   line). A breakpoint shows `◌` in the gutter (pending) and `●` once the adapter
   verifies it.
2. Start the session with **`:DapRun`** — onda picks the adapter for the file's
   language, launches it, sends your breakpoints, and runs the program.
3. When execution stops at a breakpoint the stopped line is marked `→` in the gutter.
4. Control execution:
   - **`<F5>`** continue
   - **`<F10>`** step over (next)
   - **`<F11>`** step into
   - **`<F12>`** step out
5. Inspect state:
   - **`:DapStack`** — the call stack (top frame marked `→`).
   - **`:DapVars`** — locals in the stopped frame.
   - **`:DapEval <expr>`** — evaluate an expression in the stopped frame.
6. **`:DapStop`** ends the session.

## Commands

| Command | Action |
|---|---|
| `:DapRun` | Launch a debug session for the current file |
| `:DapStop` | Disconnect / end the session |
| `:DapBreakpoint` | Toggle a breakpoint on the current line (same as `<F9>`) |
| `:DapStack` | Show the call stack |
| `:DapVars` | Show current-frame variables |
| `:DapEval <expr>` | Evaluate an expression at the stop |

## Notes

- Conditional breakpoints and a dedicated side-panel/thread picker are tracked in
  `docs/BACKLOG.md`; v1 ships breakpoint gutter markers, control keys, and the
  stack/vars floats above.
- Adapter crashes surface as a `dap: adapter disconnected` message; re-run `:DapRun`.
