# onda

**A fast, modal terminal editor/IDE written in Rust.**

onda is a vim-like modal editor built around one hard constraint: **it must be as
fast as or faster than Neovim, always.** It owns its whole stack — text engine, cell
compositor, syntax, LSP, terminal, plugins — so input latency and startup stay tiny
even with IDE features turned on.

> **Status:** early / pre-1.0 (`0.0.1`). The editor is usable for real work; some
> features need external tools (language servers, debug adapters, an ACP agent), and a
> few are still in progress — see [Status](#status) and [`docs/BACKLOG.md`](docs/BACKLOG.md).

## Highlights

- **Modal vim editing** — motions, operators, registers, macros, `.`-repeat, marks,
  visual / visual-line / visual-block, regex search & substitute, splits, a fuzzy
  file/buffer picker, and command-line completion (`<Tab>`).
- **Tree-sitter syntax** highlighting + structural **text objects** (`af`/`if`,
  `ac`/`ic`, `aa`/`ia`) for Rust and Python.
- **LSP** — hover, completions, go-to-definition, diagnostics (rust-analyzer, …).
- **Integrated terminal**, **persistent sessions**, and **persistent undo** (opt-in).
- **Themes** — TOML format with inheritance, live `:theme` switching, and hot-reload.
- **WASM plugins** (Component Model) — sandboxed, permissioned, multi-language; lazy
  activation and per-call time budgets keep them off the hot path.
- **AI agent panel (ACP)** — chat, streaming responses, `@`-mentions, a permission
  gate, and hunk-level review of agent-proposed edits.
- **Data-file superpowers** — CSV/TSV virtual **table** view and a JSONL **field**
  schema overlay.
- **Performance budgets enforced in CI** — see [`BENCH_REPORT.md`](BENCH_REPORT.md)
  (cold start ~6 ms, theme switch ~0.1 ms on an M4).

## Install

onda is built from source for now (no published binaries yet).

```sh
git clone https://github.com/onda-editor/onda
cd onda

# Run it directly:
cargo run --release -- path/to/file

# …or install the binary + runtime (themes, grammars) to ~/.local:
cargo run -p xtask -- install   # onda → ~/.local/bin, runtime → ~/.local/share/onda
onda --version                  # ensure ~/.local/bin is on your PATH
```

Check your environment is ready (terminal capabilities, language servers, clipboard,
ripgrep, config parse status — with fix-it hints):

```sh
onda doctor
```

## Quick start

```sh
onda README.md
```

onda is modal, like vim:

- **Normal** — navigate and run commands (the default).
- `i` / `a` / `o` … enter **Insert** to type; `<Esc>` returns to Normal.
- `v` / `V` / `<C-v>` enter **Visual** / **Visual-Line** / **Visual-Block**.
- `:` opens the **command line** (e.g. `:w`, `:q`, `:e file`). `<Tab>` completes
  command names and file paths.

Core motions/operators (`hjkl`, `w`/`b`/`e`, `0`/`$`, `gg`/`G`, `d`/`c`/`y` +
motion/text-object, `u`/`<C-r>`, `/`pattern, `:%s/…/…/g`, …) work as you'd expect.

## Feature reference

### Files, windows, search
| Command / key | Action |
|---|---|
| `:w` `:q` `:wq` `:q!` | write / quit / write-quit / force-quit |
| `:e <path>` | open a file (`<Tab>` completes paths) |
| `:sp` / `:vsp [file]` | horizontal / vertical split |
| fuzzy picker | open via the file/buffer picker; type to filter |
| `/pat`, `n`/`N`, `:%s/a/b/g` | search & substitute |

### Themes
| Command | Action |
|---|---|
| `:theme` | show the active theme + built-ins |
| `:theme <name>` | switch live (`onda-dark`, `onda-light`, `onda-contrast`, `onda-wave`) |

See [`docs/THEMES.md`](docs/THEMES.md) for the TOML format and `inherits`.

### Plugins (WASM)
Plugins are WebAssembly components — sandboxed, permissioned, and written in any
language that targets the Component Model. They activate lazily and run under
per-call time budgets so they can't stall input.

```sh
onda plugin install github:<user>/<repo>   # fetch + verify + install
onda plugin list / update / remove
onda plugin dev --watch                     # rebuild + hot-reload during development
```

The host API is defined in [`wit/onda/`](wit/onda); a quickstart is in
[`docs/plugin-book/quickstart.md`](docs/plugin-book/quickstart.md). Git integration
ships as the `git-blame-inline` reference plugin (see `plugins/`).

### AI agent (ACP)
Speaks the Agent Client Protocol to an external agent (e.g. `claude-code acp`),
configured in `~/.config/onda/agents.toml`.

| Command | Action |
|---|---|
| `:agent <name>` | connect & open the panel (input box at the bottom) |
| `:agent` | toggle the panel |
| `:agent-review` | review agent-proposed edits per-hunk (`a`/`r`/`A`/`R`, `⏎` apply) |
| `:agent-export` | export the transcript to a buffer |

In the input box, attach context with `@file:…`, `@selection`, `@buffer:…`,
`@diagnostics`. Tool requests prompt for permission (allow once / always / deny).

### Data files
| Command | Action |
|---|---|
| `:table` | toggle the CSV/TSV aligned virtual-table view |
| `:fields` | show the JSONL field schema (keys, counts, types) |

### Other
| Command | Action |
|---|---|
| `:terminal` | open an integrated terminal pane |
| `:session save/restore [name]` | persist / restore buffers + layout |
| `:messages` `:ls` | message history · buffer list |

## Configuration

onda reads `~/.config/onda/config.toml` (and a project-local `.onda/config.toml`):

```toml
theme = "onda-dark"

[editor]
tab_width = 4
expand_tab = true
mouse = true
persistent_undo = false   # opt-in: restore undo history across sessions
```

Other config files under `~/.config/onda/`: `themes/<name>.toml`, `agents.toml`, and
the plugin lockfile `plugins.lock`. Plugins are WASM components managed by
`onda plugin …` (API: [`wit/onda/`](wit/onda), guide:
[`docs/plugin-book/`](docs/plugin-book)).

## Building & contributing

```sh
cargo build --workspace        # build everything
cargo test --workspace         # run the test suite
cargo run -p xtask -- ci       # fmt check + clippy -D warnings + tests
cargo run -p xtask -- bench    # performance gates (see BENCH_REPORT.md)
```

The workspace is split into focused crates (`onda-core`, `onda-modal`, `onda-render`,
`onda-syntax`, `onda-lsp`, `onda-terminal`, `onda-session`, `onda-plugin`,
`onda-agent`, `onda-data`, and the `onda` binary). Architecture and the
performance rules live in [`docs/DESIGN.md`](docs/DESIGN.md) and `AGENTS.md` — please
read `AGENTS.md` before contributing (the gates apply to human and agent PRs alike).

## Status

Working: modal editing, syntax highlighting + text objects (Rust, Python), LSP,
terminal, sessions, persistent undo, themes, command-line completion, the WASM plugin
system, the agent panel + diff review, CSV/JSONL views, and `onda doctor`.

Not yet done (tracked in [`docs/BACKLOG.md`](docs/BACKLOG.md)): the Phase 3 reference
plugins are being finalized (including `git-blame-inline` — git integration now ships
as a plugin, not built in), remote editing over SSH (`scp://`), the libvterm terminal
backend, and release packaging (Homebrew, prebuilt binaries, docs site). The agent
protocol path is covered by a mock agent in CI; driving real `claude-code` needs it
installed. A debugger (DAP) is deferred to the post-v0.1 backlog.

## License

Licensed under either of Apache-2.0 or MIT at your option.
