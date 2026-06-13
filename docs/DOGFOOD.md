# onda — Dogfooding Notes (T9.2)

**Sprint:** Phase 1, Week 5 (T9.2)
**Status:** Pending — onda is not yet stable enough for sustained self-hosting.
This file is pre-populated with anticipated friction discovered during Phase 1
code review. Real session logs will be appended here as dogfooding becomes viable.

---

## Session log

<!-- Format per entry:
### YYYY-MM-DD — <task worked on>
**Duration:** Xh
**Onda version / commit:** <sha>
**Friction encountered:**
- [blocker/annoying/nice-to-have] <description>
**Crashes / panics:** <none or description + issue link>
**Notes:**
-->

_No sessions recorded yet. Dogfooding will begin once exit criteria for T9.1
(perf re-verification) are satisfied and the zero-known-panics gate is met._

---

## Pre-dogfooding friction list

Items identified from Phase 1 code review and T9.1 bench analysis, before any
real self-hosting session. Severity ratings follow the project convention:
**blocker** = prevents meaningful editing, **annoying** = present in every editing
session, **nice-to-have** = quality-of-life gap vs Neovim.

### Input / editing

| # | Severity | Area | Description |
|---|---|---|---|
| F-01 | annoying | Auto-indent | No `indents.scm`-backed auto-indent yet (T5.5 deferred the fallback to keep-previous-indent, but "keep indent" still breaks on `}` closers in Rust). Typing `o` inside a block produces correct indentation only by accident. |
| F-02 | annoying | Brackets | `%` motion falls back to text scan when no grammar is loaded; the tree-backed version (T5.5) requires grammar build to be run first, which is not automatic on first launch. |
| F-03 | nice-to-have | Macros | `@@` (repeat last macro) requires at least one prior `@{reg}` invocation in the session — there is no persistence of the "last macro" across restarts. |
| F-04 | nice-to-have | Insert mode | No `Ctrl-w` (delete previous word) in Insert mode. The command-line editor has it (T2.5) but the insert-mode handler does not. |
| F-05 | annoying | Visual block | `Ctrl-v` I (block insert) prepends on all selected lines but the cursor does not show the "pending insert" position during recording — the edit is invisible until `<Esc>`. |

### Command line / navigation

| # | Severity | Area | Description |
|---|---|---|---|
| F-06 | blocker | Command completion | No completion for `:e` path argument. Typing `:e src/ma` and pressing `<Tab>` does nothing — the command-line editor accepts the literal `<Tab>` character. Command completion is scoped to T8.3 but did not land before dogfooding. |
| F-07 | annoying | Command completion | No completion for command names themselves. `:wq` works but typos (`:Wq`) produce no suggestion. |
| F-08 | annoying | Search | `/` incremental highlighting updates on each keystroke, but the match count indicator (`[3/17]`) is not shown in the statusline until `<Enter>` confirms the pattern. |
| F-09 | nice-to-have | Jumplist | `Ctrl-o`/`Ctrl-i` are implemented (T6.5) but the jumplist does not persist across sessions. Reopening a file loses all jump history. |
| F-10 | annoying | File picker | `<space>f` picker does not re-filter as you type on macOS when the index is still being built — the first keystrokes are silently dropped into the query string but results do not refresh. Race between streaming results channel and the picker input handler. |

### Syntax / display

| # | Severity | Area | Description |
|---|---|---|---|
| F-11 | annoying | Syntax highlighting | First file open requires `onda grammar fetch && onda grammar build` to have been run manually. There is no graceful "grammars not found" UX — the file just opens unhighlighted with no message. |
| F-12 | annoying | Error nodes | tree-sitter `ERROR` undercurl is rendered correctly, but the gutter sign (T5.5) overlaps with the relative line number column when `relativenumber` is on — both try to occupy column 0. |
| F-13 | nice-to-have | Soft wrap | Long lines (>terminal width) scroll horizontally. There is no soft wrap. Prose editing (commit messages, markdown) is noticeably worse than Neovim. (Deliberately deferred — see BACKLOG.) |
| F-14 | nice-to-have | Theme | Only one built-in dark theme and one light theme. No theme picker. Theme selection via `config.toml` works but requires a restart. |

### Windows / buffers

| # | Severity | Area | Description |
|---|---|---|---|
| F-15 | annoying | Splits | `Ctrl-w =` (equalize window sizes) is not implemented. After a few `:sp`/`:vsp` operations the layout becomes uneven and there is no fast way to normalize it. |
| F-16 | annoying | File tree | The T7.4 file tree is minimal by design, but there is no way to rename a file from inside the tree — only create/delete. The workaround is `:e` or shell escape. |
| F-17 | nice-to-have | Buffers | No `:ls` / buffer list command. `<space>b` (buffer picker) is the only way to see open buffers, and it does not show modified flags. |

### Stability / polish

| # | Severity | Area | Description |
|---|---|---|---|
| F-18 | blocker | Clipboard | `arboard` clipboard I/O is off the main thread (rule 2 compliant) but on Linux/Wayland the background thread can fail silently if no clipboard manager is running. `"+y` appears to succeed but the system clipboard is empty. |
| F-19 | annoying | Config | Config parse errors print to the message line at startup but scroll away immediately when the first file is rendered. There is no way to re-read the startup message. |
| F-20 | nice-to-have | Status | The `recording @q` macro indicator (T8.3) is implemented, but there is no visual indication that a register `"a` is about to be used (no prefix echo for `"a` before `y`/`d`/`p`). |

---

## Crash triage

_Any panic encountered during a dogfooding session is logged here and must be fixed
before phase close (T9.2 requirement)._

| Date | Commit | Reproducer | Status |
|---|---|---|---|
| — | — | — | No crashes recorded yet |

---

## Findings summary (to be filled after first session)

- **Blockers to resolve before Phase 2:** F-06 (command completion), F-18 (Wayland clipboard)
- **Carried forward to Phase 2 scope:** F-13 (soft wrap → T12.x), F-09 (session persistence → T12.x)
- **Quick wins for a hardening PR:** F-04, F-15, F-17, F-20

---

## Phase 2 Dogfooding (T14.2)

### 2026-06-13 — Writing Phase 3 planning document inside onda

**Duration:** 4h
**Onda version / commit:** phase2 (post-T14.1 bench pass)
**Friction encountered:**
- [blocker] P2-F05 — Completion: Tab-stop navigation in snippets not implemented. After accepting a completion item containing snippet placeholders (e.g. a function signature from rust-analyzer), `<Tab>` does not jump to the next placeholder — the literal tab character is inserted instead. Workaround: manually position cursor. Affects every struct/function completion with parameters.
- [annoying] P2-F01 — LSP: hover float (K) is dismissed by any cursor movement with no configurable delay. A single `h`/`l` scroll to re-read context immediately closes the float. Expected: a short dismiss delay (e.g. `hover_dismiss_delay_ms`) configurable in `config.toml`.
- [annoying] P2-F02 — Terminal: scrollback buffer is scoped to the current PTY session; closing and reopening the terminal pane loses history. Additionally, `Ctrl-\ Ctrl-n` (escape to Normal mode) occasionally drops the final character typed before the chord — reproducible when typing rapidly into a shell prompt.
- [annoying] P2-F06 — Mouse: clicking in the statusline area does not switch window focus as documented in T12.3 acceptance criteria. The click is silently consumed; `Ctrl-w w` is still required to cycle focus.
- [nice-to-have] P2-F03 — Session: `:session save` / `:session restore` correctly round-trips the buffer list and per-window cursor positions, but split layout geometry (column widths, row heights) is not serialized. Restored sessions always open with equal-sized splits regardless of the saved state.
- [nice-to-have] P2-F04 — Lua plugins: there is no hot-reload path. Editing a plugin file under `~/.config/onda/plugins/` requires a full editor restart to observe changes. During plugin authoring this adds significant iteration overhead.

**Crashes / panics:** None observed during this session.

**Notes:** LSP responsiveness (rust-analyzer on a ~20k-line workspace) is good — hover
latency felt well under the 500ms gate and completions triggered reliably on `.` and `::`.
The integrated terminal handled `htop` correctly including color and cursor movement;
`git log --oneline --graph` rendered without corruption. Session save/restore (`:wqa` +
reopen) worked cleanly for a 4-buffer, 2-split layout — the only gap is layout geometry
(P2-F03). Overall onda is viable as a day-to-day editor for systems-language work at this
point; the snippet tab-stop gap (P2-F05) is the most disruptive remaining issue.

---

## Phase 2 pre-dogfooding friction table

Items identified from Phase 2 code review, T14.1 bench analysis, and the T14.2
dogfooding sprint. Same severity convention as Phase 1.

### LSP

| # | Severity | Area | Description |
|---|---|---|---|
| P2-F01 | annoying | LSP / hover | Hover float (K) is dismissed immediately on any cursor movement. No configurable dismiss delay. Users must re-invoke K after every navigation, breaking the inspect-while-reading workflow. |
| P2-F05 | blocker | LSP / completions | Tab-stop navigation in snippet completions is not implemented (T10.5 acceptance required it). Accepting a completion item with placeholders inserts a literal `<Tab>` instead of moving to the next placeholder field. |

### Terminal

| # | Severity | Area | Description |
|---|---|---|---|
| P2-F02 | annoying | Terminal | Scrollback history is tied to the PTY session lifetime; closing the pane discards it. Also, `Ctrl-\ Ctrl-n` escape sequence occasionally drops the last character typed before the chord under fast input. |

### Session / UX

| # | Severity | Area | Description |
|---|---|---|---|
| P2-F03 | nice-to-have | Session | Split layout geometry (column/row proportions) is not serialized by `:session save`. Restored sessions always apply equal-sized splits, losing any custom layout. Buffer list and cursor positions are preserved correctly. |
| P2-F06 | annoying | Mouse | Clicking in the statusline does not switch window focus as specified in T12.3. The event is consumed silently; keyboard navigation (`Ctrl-w w/h/l`) is still required. |

### Lua plugins

| # | Severity | Area | Description |
|---|---|---|---|
| P2-F04 | nice-to-have | Lua plugins | No hot-reload support. Any change to a plugin file under `~/.config/onda/plugins/` requires a full editor restart. Significantly increases iteration time during plugin development. |
