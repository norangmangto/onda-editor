# onda — Phase 3 Plan: IDE Maturity

**Duration:** 8 weeks | **Milestone demo:** "onda replaces my IDE for systems programming"
**Design doc:** `docs/DESIGN.md` v0.5 | **Agent rules:** `AGENTS.md` | **Prereq:** Phase 2 exit criteria all green

## Goal

Phase 2 made onda a capable IDE with LSP, an integrated terminal, sessions, and a Lua plugin
foundation. Phase 3 closes the gap to daily-driver status for systems programmers: a DAP
debugger, git integration with hunk-level staging, remote editing over SSH, full terminal
emulation via libvterm, tree-sitter text objects, a complete theme system, and a polished
command-line completion experience. The phase ends with a v0.0.3 release that includes
prebuilt grammars and an installer.

The hard constraint does not change: **every new feature must clear the existing bench gates,
and new gates are added for the debugger UI and git annotation paths.**

---

## Exit criteria

- [ ] DAP debugger works with `lldb-dap` (Rust/C/C++) and `debugpy` (Python): set breakpoints
      from the editor, run to breakpoint, inspect locals and call stack in a side panel
- [ ] Keypress → render p99 < 10ms remains green with debugger attached (new bench gate:
      DAP-on path)
- [ ] Git status decorations (modified/added/deleted gutter signs) appear on buffer open and
      update within 1s of an external `git` change; `:GitDiff`, `:GitBlame`, and
      hunk-level `:GitStageHunk` all work on a real repository
- [ ] Remote editing: `:e scp://user@host/path/to/file` opens a file from a remote host via
      SSH; saves write back via the same transport; remote LSP launch documented and optional
- [ ] libvterm replaces the Phase 2 `vt100` terminal emulator; `nvim` running inside the
      terminal pane renders correctly (the primary regression target for full VT compliance)
- [ ] Tree-sitter text objects from `textobjects.scm`: `af`/`if` (function), `ac`/`ic`
      (class), `aa`/`ia` (argument) are wired for Rust, Go, Python, TypeScript, and C
- [ ] Theme system: at least three built-in themes (dark default, light, high-contrast);
      `:theme <name>` switches without restart; themes hot-reload when the file changes
      on disk; a Lua plugin can register custom highlight groups
- [ ] Command-line completion: `<Tab>` on `:e` completes file paths; `<Tab>` after `:` with
      a partial name fuzzy-completes command names; completion popup integrates with the
      existing picker widget
- [ ] `v0.0.3` tag: prebuilt tree-sitter grammar bundles for the five tier-1 languages,
      `cargo xtask install` copies the binary + grammars to `~/.local/bin`
- [ ] **Dogfooding gate:** onda is used as the primary editor for the full duration of Phase 3
      planning and coding; friction list captured in `docs/DOGFOOD.md` Phase 3 section

---

## Workstreams & dependency order

```
T15.0 harness update ─► W15 DAP debugger ─────────────┐
                     ─► W16 Git integration ────────────┤
                     ─► W17 Remote editing ─────────────┼─► W19 Hardening & release
                     ─► W18 Polish & theme system ───────┘
```

W15–W18 are largely parallel (different crates). W19 is the final two weeks.

---

## T15.0 — Harness update (day 1, before anything else)

- Extend `AGENTS.md` pre-approved deps: `dap-types` (or hand-rolled DAP JSON types),
  `libvterm-sys` (bindgen wrapper for libvterm), `russh` (SSH transport for remote editing),
  `git2` (libgit2 bindings for status/blame/diff)
- New bench fixtures: a Rust project with a `main.rs` breakpoint for DAP smoke test; a
  terminal pane running `nvim` for libvterm regression
- New gates in `xtask bench --check`:
  - DAP-on keypress latency (p99 < 10ms)
  - Git blame annotation render cost (< 2ms for a 500-line file)
  - Theme switch latency (< 5ms full re-render)
- **Accept:** gates run in CI; any synchronous DAP or SSH call on the main thread fails CI

---

## W15 — DAP debugger (`onda-dap`, weeks 1–5)

### T15.1 — DAP transport & lifecycle
- New crate `onda-dap`: JSON over stdin/stdout (same pattern as `onda-lsp`); `DapManager`
  in the background worker pool manages one adapter process per debug session; adapters
  configured in `~/.config/onda/dap.toml` (adapter binary, args, language patterns)
- Protocol: `initialize` → `launch`/`attach` → event loop; `terminated` and `exited` events
  tear down the session cleanly; adapter crashes restart with backoff up to 3 times
- `BgMessage::DapEvent` carries adapter events to the main loop; the main thread never
  awaits DAP responses (same rule 2 enforcement as LSP)
- **Accept:** `lldb-dap` starts for a compiled Rust binary; `initialized` event received;
  log shows the capability handshake; stopping the session terminates the adapter process

### T15.2 — Breakpoints
- `<F9>` (configurable) toggles a breakpoint on the current line in Normal mode; breakpoints
  stored per file in the `DapSession` and sent as `setBreakpoints` on session start and on
  every buffer save
- Gutter column: `●` for verified breakpoints, `◌` for unverified/pending, `✕` for
  rejected; breakpoints survive buffer closes within the same session
- `:DapBreakpointList` opens a picker of all active breakpoints (file, line, condition);
  `<Enter>` jumps to the location, `dd` removes it
- Conditional breakpoints: `<space>B` prompts for an expression forwarded in `setBreakpoints`
- **Accept:** set a breakpoint in a Rust `main.rs`; run the program; adapter stops at the
  line; gutter shows `●`; removing with `<F9>` updates the gutter and sends a new
  `setBreakpoints` with the line absent

### T15.3 — Execution control & call stack
- Control commands: `<F5>` continue, `<F10>` next (step over), `<F11>` step into,
  `<F12>` step out; all send the matching DAP request and wait for the `stopped` event
  before updating the UI
- On stop: the current frame is highlighted with a `→` gutter marker; `stackTrace` request
  populates the call stack panel (a side split, toggleable with `:DapStack`); `<Enter>` on
  a frame navigates to that source location
- Multi-thread: `threads` request; call-stack panel groups frames by thread; focused thread
  is configurable
- **Accept:** step through a 5-function call chain in Rust; call stack panel shows all
  frames in order; `<Enter>` on a frame navigates to the correct file and line; `<F12>`
  returns up one frame

### T15.4 — Variable inspection
- On stop: `scopes` + `variables` requests populate the variables panel (side split,
  toggleable with `:DapVars`); locals, arguments, and globals in a tree; `<Tab>`
  expands/collapses structured values
- Inline variable hints: current-frame values shown as virtual text at end-of-line for the
  stopped frame (optional, config: `dap.inline_values = true`)
- Hover evaluation: `<space>e` in Normal mode with a debug session active prompts for an
  expression; `evaluate` request sent; result shown in a floating window
- **Accept:** stop at a breakpoint inside a function; variables panel shows all locals with
  correct types and values; expand a `struct` value to see its fields; `<space>e` evaluates
  `vec.len()` and shows the result in a float

### T15.5 — DAP configuration & debugpy
- `dap.toml` schema documented in `docs/DAP.md`; provide default configs for `lldb-dap`
  (Rust/C) and `debugpy` (Python); `:DapRun` launches with the current file's adapter or
  prompts for a config when multiple match
- `debugpy` smoke test: set a breakpoint in a Python script; step through; inspect a dict
  local — confirms the protocol implementation is not Rust-specific
- **Accept:** a Python script with a breakpoint stops correctly; variable inspection works;
  `docs/DAP.md` exists with setup instructions for both adapters

---

## W16 — Git integration (`onda-git`, weeks 1–4)

### T16.1 — Git status & gutter decorations
- New crate `onda-git`: wraps `git2` for status, diff, and blame queries; all `git2` calls
  run on a dedicated background thread, never on the main loop
- Gutter signs: `+` (added lines), `~` (modified), `-` (deleted, shown at the line above);
  signs computed by diffing buffer content against HEAD via `git2::Diff`
- Signs update when: buffer is written, `BufEnter`, or a `notify` watch fires on
  `.git/index` or `.git/HEAD` (debounced 200ms)
- `:GitStatus` opens a picker of modified files (untracked, modified, staged); `<Enter>`
  opens the file; `s` stages it; `u` unstages; `dd` discards changes
- **Accept:** edit a tracked file; `+`/`~` signs appear in the gutter within 1s; save;
  signs update; `:GitStatus` lists the file

### T16.2 — Diff view
- `:GitDiff` (or `<space>gd`) opens a two-pane diff view: left pane HEAD version (read-only
  scratch buffer), right pane working-copy; diff hunks highlighted with theme-mapped colors
  (`diff.add`, `diff.delete`, `diff.change`)
- `]h`/`[h` navigate between hunks in either pane (consistent with LSP diagnostic
  navigation pattern)
- `:GitDiff %` diffs the current buffer; `:GitDiff HEAD~1` diffs against an arbitrary ref;
  syntax highlighting active in both panes
- **Accept:** open a modified Rust file; `:GitDiff` shows a correctly colored two-pane
  diff; `]h`/`[h` jump between all hunks; close the diff view with `q`

### T16.3 — Blame
- `:GitBlame` (or `<space>gb`) toggles an inline blame annotation column:
  `<hash> <author> <date>` per line, dimmed with the `comment` theme scope
- Blame data fetched async via `git2::Repository::blame_file`; column renders when data
  arrives (no frame blocking)
- `<Enter>` on a blame line opens a float with the full commit message and diff stat
- **Accept:** `:GitBlame` on a file with at least 3 distinct commits shows per-line
  annotations; `<Enter>` on a line shows the commit message; toggling off removes the
  column with no layout shift

### T16.4 — Hunk-level staging
- `:GitStageHunk` (`<space>gs`) stages the hunk under the cursor (patch sent to
  `git apply --cached`); `:GitResetHunk` (`<space>gr`) discards the hunk
- Visual selection spanning multiple hunks stages/resets all covered hunks
- After stage/reset the gutter signs update and `:GitStatus` reflects the new index state
- File-tree git badges (deferred from Phase 1, `docs/BACKLOG.md`): `onda-git` exposes a
  `FileStatus` map that the file-tree component consumes; tree shows `M`/`A`/`?`/`D`
  beside file names
- **Accept:** modify 3 separate hunks in a file; stage one with `:GitStageHunk`; `:GitStatus`
  shows the file as partially staged; `git diff --cached` confirms exactly that hunk staged

---

## W17 — Remote editing & libvterm (weeks 2–5)

### T17.1 — SSH transport
- `RemoteDocument` in `onda-core` (or a thin new module): URL scheme `scp://user@host/path`
  and `ssh://user@host/path` parsed by `:e`; connection via `russh`; file content fetched
  async and loaded into a scratch buffer
- `:w` on a remote buffer writes back via SFTP put; conflict detection: server mtime checked
  before write, warn on mtime mismatch
- Connection pooling: one SSH connection per host reused across multiple remote buffers;
  connections closed on `:qa`
- **Accept:** `:e scp://localhost/tmp/test.txt` opens the file; editing and `:w` writes
  back (verifiable with `ssh localhost cat /tmp/test.txt`); a second remote buffer on the
  same host reuses the existing connection (logged at DEBUG)

### T17.2 — Remote LSP (documented, not new code)
- Document in `docs/REMOTE.md`: how to forward a remote LSP server over SSH stdio
  (`ssh user@host rust-analyzer`); `onda-lsp` transport is already stdin/stdout, so a
  remote server works with a custom `lsp.toml` `command = ["ssh", "host", "rust-analyzer"]`
- Write an automated integration test that stubs the SSH command with a mock rust-analyzer
- **Accept:** `docs/REMOTE.md` explains the pattern; the integration test passes

### T17.3 — libvterm full terminal emulation
- Replace the Phase 2 `vt100` crate with `libvterm-sys` (C bindings to libvterm); link
  libvterm statically via a `build.rs` that vendors the libvterm source under `vendor/`
- `VtermScreen` wraps `libvterm::Screen`; the `onda-terminal` crate swaps its internal
  `Screen` type — public API to the rest of the editor is unchanged
- Regression targets that must pass: `nvim` inside terminal pane (opens, edits, closes
  without corrupting onda's outer screen), `tmux` (attaches, splits, detaches), `htop`
  (renders colors, updates correctly)
- **Accept:** all three regression targets pass; `cargo test --workspace` green; Phase 2
  `vt100` crate removed from `Cargo.toml`

### T17.4 — rsync-backed large-file remote editing
- For files > 10MB on remote hosts, use `rsync --checksum` to compute a local cache at
  `~/.cache/onda/remote/<host>/<path>`; writes sync back with rsync delta transfer
- Falls back to full-copy transfer if rsync is not available on the remote host; logged as
  a warning; `:RemoteSync` forces a re-fetch
- Cache invalidation: mtime-based
- **Accept:** open a 50MB file from a remote host via rsync path; initial load < 3s on a
  loopback SSH connection; second open (cache hit) < 100ms

---

## W18 — Polish & theme system (weeks 1–5)

### T18.1 — Full theme system
- Theme file format: TOML under `runtime/themes/<name>.toml`; keys are scope names
  (`ui.background`, `ui.statusline`, `syntax.keyword`, `diff.add`, etc.); values are
  `{ fg = "#hex", bg = "#hex", bold = bool, italic = bool, underline = bool }`
- Three built-in themes: `onda-dark` (current default, extracted to TOML), `onda-light`
  (inverted palette), `onda-contrast` (WCAG AA high-contrast)
- `:theme <name>` switches live (full re-render, < 5ms gate); `config.toml` `theme = "name"`
  sets the default
- Hot-reload: `notify` watch on the theme file (opt-in, `theme.live_reload = true`);
  debounced 100ms; errors in the theme file printed to the message line, never a crash
- Lua API: `onda.highlight.set(group, opts)` lets plugins define or override highlight
  groups after theme load; fires on every theme switch via the `ThemeChanged` autocmd
- **Accept:** switch from `onda-dark` to `onda-light` mid-session; all UI elements repaint
  correctly; edit the theme TOML and see changes within 200ms; bench gate < 5ms passes

### T18.2 — Tree-sitter text objects
- Populate `textobjects.scm` query files for Rust, Go, Python, TypeScript, and C under
  `runtime/queries/<lang>/textobjects.scm`; scopes: `@function.outer`, `@function.inner`,
  `@class.outer`, `@class.inner`, `@parameter.outer`, `@parameter.inner`
- Wire `af`/`if` (function), `ac`/`ic` (class/struct), `aa`/`ia` (argument) operator
  targets in `onda-modal`; motion resolver queries tree-sitter when a textobject target is
  requested and a grammar is loaded; graceful fallback (no-op + message) when grammar absent
- Table-driven tests for each language: `(keys, before, after, selection)` covering
  nested functions, multi-parameter lists, and empty bodies
- **Accept:** in a Rust file, `vaf` selects the entire function under cursor; `dif` deletes
  the function body; `via` selects the argument under cursor; all five languages have at
  least 10 table-driven test cases passing

### T18.3 — Command-line completion
- `<Tab>` in the command line after `:e ` completes file paths using the existing
  `nucleo-matcher` picker logic; completions shown inline (cycling) and in a small popup
  above the command line (up to 8 items)
- `<Tab>` after `:` with a partial command name fuzzy-completes registered command names
  (built-in + Lua-registered); `<S-Tab>` cycles backwards
- Completion popup styled with the theme (`ui.menu`, `ui.menu.selected` scopes); `<Esc>`
  dismisses without completing; `<CR>` accepts the highlighted item
- **Accept:** `:e src/<Tab>` shows the `src/` directory contents; typing narrows the list;
  `<Enter>` completes to the selected path; `:Gi<Tab>` completes to `:GitBlame`,
  `:GitDiff`, etc.

### T18.4 — Backlog polish items
- **Undo-tree visualization**: `:UndoTree` opens a picker-based overlay showing the branch
  tree; nodes show timestamp and preview text; `<Enter>` jumps the buffer to that state
  (resolves the Phase 1 BACKLOG deferral)
- **Hot-reload config**: `notify` watch on `~/.config/onda/config.toml` and
  `<project>/.onda/config.toml`; debounced 500ms; only non-structural settings hot-reload
  (keymaps, theme, tab width); structural changes (plugin list) require restart with a
  notice (resolves Phase 1 BACKLOG deferral)
- **File-tree rename + multi-select**: `R` in the file tree renames in-place via the
  command line; `<Space>` toggles multi-select; `d` on a multi-selection deletes all with
  a confirmation prompt (resolves Phase 1 BACKLOG deferral)
- **Async progressive file loading**: files > 100MB stream into the rope in 8MB chunks;
  the buffer is editable after the first chunk loads; a progress indicator in the statusline
  shows `loading… 24%` (resolves Phase 0 BACKLOG deferral)
- **Accept (each sub-item):** undo-tree picker shows at least 2 branches for a buffer with
  divergent edits; config hot-reload updates tab width within 600ms of file save; file-tree
  rename works and updates open buffers with the new name; a 200MB file becomes interactive
  < 500ms after the first chunk loads

---

## W19 — Hardening & release (weeks 7–8)

### T19.1 — Perf re-verification
- Full bench suite: DAP-on, git blame active, libvterm terminal open, themes loaded, Lua
  plugins running; update `baseline.json`; run `bench-compare` vs nvim/helix with all
  equivalent features enabled and update `BENCH_REPORT.md`
- Specifically verify the 1GB file gate still holds after the progressive loading changes
  in T18.4
- **Accept:** all Phase 0 + Phase 1 + Phase 2 + Phase 3 gates green on macOS + Linux;
  no gate regressed vs Phase 2 baseline

### T19.2 — Dogfooding sprint (Phase 3)
- Use onda as the primary editor for the entire 8-week phase; log friction points in
  `docs/DOGFOOD.md` Phase 3 section as they arise (not just at the end)
- Crash triage: any panic during dogfooding → regression test + fix before phase close;
  zero known panics at phase exit
- Use the DAP debugger on at least one real debugging session in a Rust project; note any
  friction in `DOGFOOD.md`
- **Accept:** `DOGFOOD.md` Phase 3 section populated with at least 10 friction items (even
  if resolved); zero known panics; DAP session logged

### T19.3 — Fuzzing & security hardening
- `cargo-fuzz` targets: DAP JSON response parser (malformed responses, truncated frames);
  SSH transport (malformed host key, early EOF); libvterm input (arbitrary byte sequences
  including null bytes and overlong sequences)
- Review all `unsafe` blocks introduced in Phase 3 (primarily `libvterm-sys` FFI); each
  requires a `// SAFETY:` comment; CI lint rejects new `unsafe` without the comment
- At least one fuzz corpus commit with seeds from real DAP/SSH traffic
- **Accept:** 24h fuzz run finds zero panics (or all found panics fixed and
  regression-tested)

### T19.4 — v0.0.3 release packaging
- `cargo xtask bundle`: compiles the binary in release mode, runs `cargo xtask grammar-fetch`
  to download prebuilt grammar `.so`/`.dylib` files for Rust, Go, Python, TypeScript, and C
  from the GitHub Releases artifact store, and packs them into a `dist/` directory
- `cargo xtask install`: copies the binary to `~/.local/bin/onda` and grammars to
  `~/.local/share/onda/grammars/`; idempotent; prints the installed version
- `CHANGELOG.md` entry for v0.0.3 covering all Phase 3 features; `docs/DESIGN.md` updated
  to v0.5 (new crates, ADR updates for any overridden decisions)
- Tag `v0.0.3` on the release commit; retro written; `PHASE3_PLAN.md` completion status
  updated; `BACKLOG.md` updated with Phase 4 deferrals
- **Accept:** `cargo xtask install` on a fresh macOS and Linux machine produces a working
  `onda` binary with all five tier-1 language grammars; `onda --version` prints `0.0.3`;
  GitHub Release has downloadable artifacts for macOS arm64, macOS x86_64, Linux x86_64,
  and Linux arm64

---

## Suggested implementation order

```
T15.0 →
  T15.1 → T16.1 → T18.1 → T15.2 → T17.1 →
  T16.2 → T18.2 → T15.3 → T17.3 → T16.3 →
  T18.3 → T15.4 → T16.4 → T17.2 → T18.4 →
  T15.5 → T17.4 →
  T19.1 → T19.2 → T19.3 → T19.4
```

Rationale: DAP transport (T15.1) and git status (T16.1) first because they carry the most
external-tooling risk (adapter binaries, libgit2 linking); theme system (T18.1) early
because every subsequent UI feature needs the new scope names; libvterm (T17.3) in the
middle after the terminal path is well-exercised so the backend swap has a narrow blast
radius; T18.4 polish items are non-blocking and can run in parallel with later W15/W16
work; hardening and release always last.

---

## Phase 3 risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| libvterm static linking conflicts with system libvterm on Linux | High | Vendor libvterm source under `vendor/libvterm/` with a pinned version; `build.rs` always builds from source; version recorded in `CHANGELOG.md` |
| DAP protocol variance between `lldb-dap` versions and `debugpy` | High | T15.1 acceptance test runs against pinned adapter versions; log all DAP traffic at DEBUG; add a protocol conformance test corpus |
| `git2` binary size increase > 2MB | Medium | Measure in CI with `cargo bloat`; if too large, fall back to shelling out to `git` for status/diff and keep `git2` only for blame |
| SSH key agent forwarding edge cases with `russh` | Medium | Phase 3 supports only `ssh-agent` auth and password auth; cert-based and FIDO keys deferred and documented in `docs/REMOTE.md` |
| Tree-sitter textobject queries wrong for nested or degenerate syntax | Medium | Each language gets >= 10 table-driven tests including edge cases (empty body, single-arg, nested classes); failures block merge |
| Async progressive loading race between chunk delivery and edit transactions | Medium | Chunks applied as Transactions through the existing ChangeSet path; no direct rope mutation; property tests verify invariants hold mid-load |
| Theme hot-reload causing visible flicker on slow disks | Low | Debounce 100ms; rerender is a single damage-tracked compositor pass; bench gate < 5ms enforced |
| v0.0.3 prebuilt grammar CI matrix (four platform targets) | Low | Grammar build matrix added to CI in week 5 (T19.4 prep); grammars built with the tree-sitter CLI version pinned in `xtask` |

---

## New crates introduced in Phase 3

| Crate | Purpose | Key new dep |
|---|---|---|
| `onda-dap` | DAP client, JSON transport, debugger session management | `dap-types` (listed in T15.0) |
| `onda-git` | Git status, diff, blame, hunk staging via libgit2 | `git2` (listed in T15.0) |

Existing crates extended in Phase 3:

| Crate | Extension |
|---|---|
| `onda-terminal` | libvterm backend replaces `vt100`; `libvterm-sys` listed in T15.0 |
| `onda-core` | `RemoteDocument` type for SSH-backed buffers; `AsyncChunkLoader` for progressive loading |
| `onda-modal` | Tree-sitter textobject motion targets (`af/if`, `ac/ic`, `aa/ia`) |
| `onda-render` | Theme TOML loader, hot-reload watcher, `onda.highlight.set` Lua bridge |

Crate dependency rules from `docs/DESIGN.md` §6 still apply: `onda-dap` and `onda-git`
depend on `onda-core`, never on each other or on `onda-lsp`/`onda-terminal`/`onda-lua`.
The binary wires all crates together.
