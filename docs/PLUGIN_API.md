# onda Plugin API

> **Stability: `@unstable` (host API v0.1).** The interfaces described here are
> versioned with the `onda:plugin` WIT package and may break without deprecation
> until the Phase 5 release freeze (DESIGN.md §8 risk row). The host advertises its
> API version; a plugin manifest declares `min-api-version` and **fails to load** if
> the host cannot satisfy it. See [Migration / Compatibility](#migration--compatibility).

---

## Table of Contents

1. [Overview](#overview)
2. [Execution & Threading Model](#execution--threading-model)
3. [The WIT World](#the-wit-world)
4. [Quickstart](#quickstart)
5. [Manifest (`onda-plugin.toml`)](#manifest-onda-plugintoml)
6. [Permission Model](#permission-model)
7. [Shared Types](#shared-types)
8. [Host Interfaces (plugins import)](#host-interfaces-plugins-import)
9. [Guest Exports (the host calls)](#guest-exports-the-host-calls)
10. [Performance Contract](#performance-contract)
11. [Plugin Manager (install / list / remove)](#plugin-manager-install--list--remove)
12. [Reference Plugins](#reference-plugins)
13. [Migration / Compatibility](#migration--compatibility)

---

## Overview

onda plugins are **WebAssembly Components** (ADR-002), not scripts. The runtime is
[`wasmtime`](https://wasmtime.dev) with the **Component Model**, and the host API is
defined as typed WIT interfaces under `wit/onda/*.wit`. Any language with WIT
bindings can target it; **Rust is first-class** (`wit-bindgen` + `wasm32-wasip2`).

Why WASM and not an embedded scripting language (ADR-002, rejecting Lua):

- **Sandboxing** — a plugin is a capability-confined component. A crash or hang is
  contained by the host; it cannot take down the editor.
- **Multi-language** — Rust / Python / JS / Go can all compile to a component.
- **Near-native speed** with predictable, interruptible execution.

The plugin crate is `onda-plugin`. The full WIT reference lives in `wit/README.md`;
the typed surface is `wit/onda/{world,types,host,guest}.wit`.

> The previous Lua (`mlua`) plugin system and the `onda.*` Lua API have been
> **removed** (ADR-002). If you are looking for `onda.buf.get_lines` /
> `_onda_callbacks` / `runtime/plugins/*.lua`, those no longer exist — the
> equivalents are the `buffer`, `commands`, and `keymap` WIT interfaces below.

---

## Execution & Threading Model

The threading rules are the same as the rest of the editor (AGENTS.md rule 2 +
ADR-002): **no host call may block the caller, and nothing a plugin does is an await
point that can stall the render path.**

1. Plugin handlers run on the **main thread, between frames** — never inside the
   event loop's input path.
2. Each handler runs under an **epoch deadline (default 5ms, T18.2)**. Exceeding it
   suspends and demotes the plugin (a busy loop is trapped, not awaited).
3. **Reads** (`buffer.text`, `selection.get`, `editor.cursor`, …) are answered from
   a **snapshot** prepared before the plugin's budget begins — they are consistent
   and synchronous, never racing the live buffer.
4. **Writes** (`buffer.apply`, `selection.set`, `decorations.set`, …) are **queued**
   and applied by the main loop between frames. Buffer mutation goes through
   `onda-core`'s `Transaction`/`ChangeSet` — there is **no raw rope access**, and a
   batch of edits lands as **one undo step**.

---

## The WIT World

A plugin is a component targeting the `plugin` world (`wit/onda/world.wit`):

```wit
package onda:plugin@0.1.0;

world plugin {
    import types; import log; import buffer; import selection; import editor;
    import commands; import keymap; import decorations; import ui; import config;
    // Capability-gated — present only when the manifest declares + user approves.
    import fs; import http;
    export guest;
}
```

The plugin **imports** the host interfaces and **exports** `guest` (its lifecycle and
handlers). `fs` and `http` are linked into the instance **only** when the manifest
declares them and the grant is in place — an ungranted import is a **link-time
failure**, so declaration is enforcement.

**v0 non-goals** (intentionally absent): arbitrary UI windows beyond
float/picker/statusline; raw filesystem/network outside the capability-gated
`fs`/`http`; raw buffer/rope access; any synchronous host call that can block a frame.

---

## Quickstart

A condensed version of `docs/plugin-book/quickstart.md`.

**Prerequisite:** `rustup target add wasm32-wasip2`

```toml
# Cargo.toml
[package]
name = "my-plugin"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]

[dependencies]
wit-bindgen = "0.58"
```

```rust
// src/lib.rs
wit_bindgen::generate!({ world: "plugin", path: "wit/onda" });

use exports::onda::plugin::guest::{Event, Guest};
use onda::plugin::{buffer, log};
use onda::plugin::types::NotifyLevel;

struct Plugin;

impl Guest for Plugin {
    fn init() {
        log::debug("my-plugin activated");
    }

    fn handle_event(ev: Event) {
        if let Event::BufferOpen(b) = ev {
            let n = buffer::line_count(b.buf).unwrap_or(0);
            log::notify(&format!("{} has {n} lines", b.path), NotifyLevel::Info);
        }
    }

    fn run_command(_id: u64, _args: Vec<String>) {}
    fn run_keymap(_id: u64) {}
}

export!(Plugin);
```

Import paths mirror the package: host interfaces are `onda::plugin::<iface>`, the
guest trait is `exports::onda::plugin::guest::Guest`.

```sh
cargo build --release --target wasm32-wasip2
onda plugin install ./my-plugin     # local dir, or github:user/repo[@rev]
```

---

## Manifest (`onda-plugin.toml`)

Every plugin ships a manifest alongside its `.wasm` component. It is parsed by
`onda-plugin::manifest`; an unsatisfiable `min-api-version` makes the plugin fail to
load (the host API is currently **v0.1**).

```toml
[plugin]
name = "git-blame-inline"
version = "0.1.0"
entry = "git_blame_inline.wasm"   # the component file, relative to the plugin dir
min-api-version = "0.1"           # host must satisfy this (major must match, minor >=)

[permissions]
buffer = "read"                   # none | read | write
filesystem = ["./.git"]           # project-root-relative whitelist (capability)
network = false                   # capability — gates the `http` import
shell = false                     # reserved; no shell interface in v0

[activation]
events = ["buffer-open", "cursor-hold"]   # lazy activation — protects startup
```

`activation.events` are the [guest events](#guest-exports-the-host-calls) the plugin
subscribes to. Declaring them up front lets the host avoid instantiating a plugin
until something it cares about happens.

---

## Permission Model

Capabilities are **declared in the manifest** and (intended to be) **approved by the
user**. Enforcement is at link time and at the host boundary:

| Permission | Effect |
|---|---|
| `buffer = "none" \| "read" \| "write"` | Whether `buffer.apply` / `selection.set` are honored. |
| `filesystem = [paths]` | Wires in the `fs` interface, scoped to those paths via `cap-std` preopens. `..` escapes are **rejected by the host**. An empty/absent list means `fs` is not linked. |
| `network = true` | Wires in the `http` interface (host-mediated; no raw sockets). |
| `shell` | Reserved. There is no shell interface in v0. |

The effective grant is `request ∩ grant`; an import the plugin was not granted
**fails to link**, so a plugin literally cannot call a capability it didn't declare.

> **Current limitation (see `docs/BACKLOG.md`).** `discover` currently **auto-grants
> the declared capabilities** — the install-time and first-use approval *prompt* is
> not yet wired. The path-scoping, `..`-rejection, and link-time enforcement above
> are all live; only the interactive consent UI is outstanding.

---

## Shared Types

From `wit/onda/types.wit`. **Positions are char indices** (0-based, UTF-8 scalar
boundaries) — there are no line/col footguns in v0.

| Type | Definition |
|---|---|
| `char-idx` | `u32` — a character index into a buffer. |
| `buffer-id` | `u64` — stable for the lifetime of an open buffer. |
| `window-id` | `u64` — a window handle. |
| `range` | `{ anchor: char-idx, head: char-idx }` — half-open; `anchor == head` is a caret, `anchor` may be `> head`. |
| `selection` | `{ ranges: list<range>, primary: u32 }` — **always 1..N ranges** (ADR-006); `primary` indexes the main cursor. |
| `notify-level` | `info \| warn \| error`. |
| `mode` | `normal \| insert \| visual \| visual-line \| visual-block \| command` (read-only). |
| `style` | `{ fg: option<string>, bg: option<string>, bold: bool, italic: bool, underline: bool }`. Colors are `#rrggbb` or theme-scope/ANSI names; empty = inherit. |
| `host-error` | `invalid-handle \| out-of-bounds \| permission-denied(string) \| rejected(string)`. Plugins must handle these without panicking. |

---

## Host Interfaces (plugins import)

Every fallible call returns `result<_, host-error>`. None of these block.

### `log`
```wit
notify: func(msg: string, level: notify-level);   // message line
debug:  func(msg: string);                         // ONDA_LOG file, never the screen
```

### `buffer`
Read from a snapshot; mutate transactionally.
```wit
current:    func() -> buffer-id;
len:        func(buf) -> result<char-idx, host-error>;
line-count: func(buf) -> result<u32, host-error>;
text:       func(buf, range) -> result<string, host-error>;   // by char range
lines:      func(buf, start: u32, end: u32) -> result<list<string>, host-error>;
record edit { range, text }                                    // anchor==head = insert; empty text = delete
apply:      func(buf, edits: list<edit>) -> result<_, host-error>;  // ONE transaction / undo step
```
`apply` is rejected if the buffer changed under the plugin since its snapshot — edits
are expressed in the pre-edit coordinate space and the host orders/maps them.

### `selection`
```wit
get: func(buf) -> result<selection, host-error>;
set: func(buf, sel: selection) -> result<_, host-error>;
```
Selections are full multi-range values — never assume a single cursor (ADR-006).

### `editor`
```wit
current-window: func() -> window-id;
current-mode:   func() -> mode;
cursor:         func(win) -> result<char-idx, host-error>;
set-cursor:     func(win, pos: char-idx) -> result<_, host-error>;
```

### `commands` / `keymap`
The guest exports the handlers; registration just declares them by `id`.
```wit
// commands
create: func(name: string, id: u64, desc: option<string>, nargs: u8);  // :name → run-command(id, args)
// keymap
set:    func(mode: string, lhs: string, id: u64, desc: option<string>); // lhs → run-keymap(id)
```

### `decorations` — the perf-critical surface (T17.2)
**Batch only.** Submit the whole decoration set for a namespace in one call;
replacing a namespace clears the plugin's previous decorations in it (no per-item
add/remove churn). The compositor diffs the batch onto the cell grid.
```wit
record virt-text { at: char-idx, text: string, style }   // virtual text after a char
record sign      { line: u32, text: string, style }      // gutter sign
record highlight { range, style }                        // inline highlight over a range
record batch     { namespace: string, virt-texts: list<virt-text>,
                   signs: list<sign>, highlights: list<highlight> }
set:       func(buf, batch) -> result<_, host-error>;
clear:     func(buf, namespace: string);
set-group: func(group: string, style);   // define/override a theme highlight group
```
`set-group` is the replacement for the old Lua `onda.highlight.set` — see
`docs/THEMES.md`. Overrides persist across `:theme` switches.

### `ui`
```wit
float:              func(title: string, lines: list<string>, width: u16, height: u16);  // read-only float
record picker-item  { label: string, detail: option<string> }
pick:               func(title: string, items: list<picker-item>, id: u64);  // result → picker-result event
statusline-segment: func(id: string, text: string, style);  // owned statusline segment
```

### `config`
Typed reads from the merged `config.toml` (global + project overlay, DESIGN §5.7).
```wit
get-string: func(key: string) -> option<string>;
get-bool:   func(key: string) -> option<bool>;
get-int:    func(key: string) -> option<s64>;
```

### `fs` — capability-gated
Linked only when `filesystem` is declared + granted. Paths resolve against the
project root through `cap-std` preopens; `..` escapes are rejected.
```wit
read:     func(path: string) -> result<list<u8>, host-error>;
write:    func(path: string, data: list<u8>) -> result<_, host-error>;
read-dir: func(dir: string) -> result<list<string>, host-error>;
```

### `http` — capability-gated
Linked only when `network = true`. Host-mediated; no raw sockets.
```wit
record response { status: u16, body: list<u8> }
get:  func(url: string) -> result<response, host-error>;
post: func(url: string, body: list<u8>, content-type: string) -> result<response, host-error>;
```
> **Current limitation:** the host `http` implementation is **v0-stubbed**
> (`docs/BACKLOG.md`). The interface links and the capability gate works, but real
> request execution is not yet implemented.

---

## Guest Exports (the host calls)

From `wit/onda/guest.wit`. Handlers run on the main thread under the 5ms epoch
deadline and reach editor state only through the host interfaces above.

```wit
init:         func();                                  // once, on first activation
handle-event: func(ev: event);                         // a subscribed event fired
run-command:  func(id: u64, args: list<string>);       // a :name command was invoked
run-keymap:   func(id: u64);                            // a registered key sequence was pressed
```

**Events** (`event` variant — subscribe via the manifest `activation.events`):

| Event | Payload | Fires when |
|---|---|---|
| `buffer-open` | `{ buf, path }` | a buffer is opened |
| `buffer-save` | `{ buf, path }` | a buffer is saved |
| `buffer-change` | `{ buf, path }` | a buffer's text changed (post-transaction) |
| `cursor-hold` | `{ buf, pos }` | the cursor was idle for the hold interval |
| `mode-change` | `mode` | the editor mode changed |
| `picker-result` | `{ id, index: option<u32> }` | a `ui.pick` this plugin opened resolved (or was cancelled) |

---

## Performance Contract

Plugin execution runs on the main thread; every µs spent there is a µs not available
to rendering (16ms frame budget, 60fps).

- **Per-handler budget: 5ms (epoch deadline).** Wasmtime epoch interruption + a
  watchdog trap a handler that overruns; the plugin is suspended/demoted rather than
  allowed to stall a frame. Do heavy work incrementally across events.
- **Reads are snapshot-consistent and synchronous**; **writes are queued** and applied
  between frames. A host call never awaits.
- **Decorations must be batched** — submit the full namespace set per frame's worth of
  work via `decorations.set`; never one host call per item.
- **Memory is bounded** per instance (the `Store` enforces a memory limit).
- All errors come back as `host-error`; a plugin that panics is contained, but should
  handle errors and keep running.

---

## Plugin Manager (install / list / remove)

`onda-plugin::manager` (`PluginManager`) manages a plugin store dir plus a
`plugins.lock`. Sources:

- `github:user/repo[@rev]`
- a git URL (including `file://`)
- a local directory

Install is **staging → promote**: a bad manifest or a missing entry component can't
half-install. The resolved commit sha is recorded in the lockfile.

```sh
onda plugin install github:user/repo@v0.1.0
onda plugin install ./my-plugin
onda plugin list
onda plugin remove my-plugin
```

> **Outstanding** (`docs/BACKLOG.md`): `update` (re-resolve lockfile),
> `onda plugin dev --watch`, and a `cargo generate` template are not yet implemented.

---

## Reference Plugins

Working WASM components under `plugins/` (built to `wasm32-wasip2`):

| Plugin | Demonstrates |
|---|---|
| `todo-highlighter` | decoration batch + event flow — visibly marks `TODO`/`FIXME` lines |
| `git-blame-inline` | `fs` capability + virtual text + `cursor-hold` (shows the branch at the cursor line; real per-line blame awaits a host `vcs` interface, deferred) |
| `http-client` | `network` capability + command + picker (host HTTP is v0-stubbed) |
| `hostile-test` | the containment fixture — a busy loop trapped by the epoch budget; an ungranted capability that fails to link |

---

## Migration / Compatibility

**Now (host API v0.1, `@unstable`):**

- The interface set in `wit/onda/*.wit` is the contract. Breaking changes are allowed
  until the Phase 5 freeze; `min-api-version` gates compatibility (major must match,
  host minor must be `>=` the requested minor).
- Capability **consent UI** is not wired yet — declared capabilities are auto-granted
  at discovery (link-time enforcement and path-scoping are live).
- Plugins currently **instantiate eagerly at startup** so their command tables are
  known; event-driven lazy activation per `activation.events` is outstanding.
- Plugin **keymaps, picker contributions, and statusline segments** are received but
  not yet fully applied.
- The host `http` implementation is a v0 stub; a `vcs` interface for real blame is
  planned.

**Coming (Phase 3→5):** install-time + first-use permission prompts; true
event-driven lazy activation; keymap/picker/statusline contribution wiring; a real
`http` host impl and a `vcs` interface; an API stability freeze with a migration guide
at the Phase 5 release.

See `docs/BACKLOG.md` ("Outstanding — plugin follow-ups") for the live status of each
item, and `wit/README.md` for the design-review notes behind the interface set.
