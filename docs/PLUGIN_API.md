# onda Plugin API

> **Stability: unstable.** The API described here is in active development during
> Phase 2. Breaking changes will occur without deprecation notices. Stability is
> targeted for Phase 3. See [Migration / Compatibility](#migration--compatibility).

---

## Table of Contents

1. [Overview](#overview)
2. [Plugin Loader](#plugin-loader)
3. [Sandbox Restrictions](#sandbox-restrictions)
4. [API Reference](#api-reference)
   - [onda.notify](#ondanotifymsg-level)
   - [onda.log](#ondalogmsg)
   - [onda.buf.get_lines](#ondabufget_linesbuf-start-end)
   - [onda.buf.set_lines](#ondabufset_linesbuf-start-end-lines)
   - [onda.win.get_cursor](#ondawinget_cursorwin)
   - [onda.win.set_cursor](#ondawinset_cursorwin-pos)
   - [onda.keymap.set](#ondakeymapsetmode-lhs-callback_id-opts)
   - [onda.cmd.create](#ondacmdcreatename-callback_id-opts)
   - [onda.ui.float](#ondauifloatopts)
   - [onda.autocmd.create](#ondaautocmdcreateevent-pattern-callback_id)
5. [Callback Registry Protocol](#callback-registry-protocol)
6. [Performance Contract](#performance-contract)
7. [Example Plugins](#example-plugins)
8. [Migration / Compatibility](#migration--compatibility)

---

## Overview

onda embeds a **Lua 5.4** runtime (via [mlua](https://github.com/mlua-rs/mlua))
for user plugins. The runtime follows the same threading rules as the rest of the
editor: **Lua runs on the main thread between frames, never inside the event loop
itself.**

The execution model is:

1. Before each frame, the main loop fires any pending Lua callbacks (keybindings,
   commands, autocmds) that were triggered by input in the previous frame.
2. Each call into Lua is bounded by the frame budget (500 µs).
3. Lua functions that need to modify editor state do not call back into the editor
   synchronously. Instead every mutating API function enqueues a `LuaApiCall` into
   a bounded channel (capacity 1 024).
4. After Lua returns, the main loop drains the channel and applies the queued calls
   to `App` in order.

Read-only API calls (`buf.get_lines`, `win.get_cursor`) are answered synchronously
from a snapshot of editor state. See [implementation note](#ondabufget_linesbuf-start-end)
for current limitations.

The sandboxed Lua VM loads only a safe subset of the Lua standard library. No file
I/O, no subprocess execution, no reflection via the `debug` library.

---

## Plugin Loader

`PluginLoader::load_all` scans two directories at startup, in this order:

| Priority | Path | Purpose |
|---|---|---|
| 1 (user) | `$XDG_CONFIG_HOME/onda/plugins/*.lua` | per-user plugins |
| 1 (user, fallback) | `~/.config/onda/plugins/*.lua` | if `XDG_CONFIG_HOME` is unset |
| 2 (project) | `<cwd>/.onda/plugins/*.lua` | per-project plugins |

Rules:

- Only files with the `.lua` extension are loaded. Other files in those directories
  are silently ignored.
- If a directory does not exist it is skipped without error.
- Both locations are scanned every time an onda session starts. There is no hot-reload
  during a session.
- A syntax or runtime error in a plugin is logged to the editor message line and to
  the tracing subscriber. The plugin is skipped; the editor continues loading
  remaining plugins. Errors never crash the editor.
- Both directories are loaded unconditionally. Project plugins do not shadow or
  replace user plugins; they are additive.

---

## Sandbox Restrictions

The Lua VM is created with a restricted standard library:

**Loaded:** `string`, `table`, `math`, `utf8`, `package`

**Not loaded at all:** `io` (standard channels `io.stdin`/`io.stdout`/`io.stderr`
may be present), `os` (partial — see below), `debug`, `coroutine`, `ffi`

In addition, the following functions are set to `nil` after library load:

| Symbol | Reason |
|---|---|
| `io.open` | filesystem read/write |
| `io.lines` | filesystem read |
| `io.popen` | subprocess I/O |
| `io.tmpfile` | filesystem write |
| `os.execute` | arbitrary subprocess execution |
| `os.remove` | filesystem mutation |
| `os.rename` | filesystem mutation |
| `os.exit` | would terminate the editor process |
| `loadfile` | load arbitrary Lua from the filesystem |
| `dofile` | load and run arbitrary Lua from the filesystem |
| `debug` (the whole table) | reflection / sandbox escape |
| `package.loadlib` | native C extension loading |

The global `require` is replaced with a whitelist implementation. Only these module
names are allowed:

- `string`
- `table`
- `math`
- `utf8`
- `bit32`
- Any name starting with `onda.` (resolved from pre-loaded injected tables, not the
  filesystem)

Attempting `require` of any other module returns a Lua runtime error:

```
sandbox: require of 'io' is not allowed
```

`onda.*` sub-namespaces (`onda.buf`, `onda.win`, etc.) are injected as globals
directly and do not need to be `require`d.

---

## API Reference

All functions live under the global `onda` table, which is injected into every
plugin VM at startup.

---

### onda.notify(msg, level)

Display a message in the editor message line.

**Parameters**

| Name | Type | Required | Description |
|---|---|---|---|
| `msg` | `string` | yes | The message text to display. |
| `level` | `string` | no | Severity level. One of `"info"`, `"warn"`, `"error"`. Defaults to `"info"` when omitted or unrecognised. |

**Returns:** nothing

**Behaviour:** Enqueues a `Notify` API call. The message is rendered by the main
loop on the next frame drain.

**Example**

```lua
onda.notify("Plugin loaded successfully")
onda.notify("File not found", "warn")
onda.notify("Critical failure", "error")
```

---

### onda.log(msg)

Convenience alias for `onda.notify(msg, "info")`.

**Parameters**

| Name | Type | Required | Description |
|---|---|---|---|
| `msg` | `string` | yes | The message text to display. |

**Returns:** nothing

**Example**

```lua
onda.log("debug: value = " .. tostring(x))
```

---

### onda.buf.get_lines(buf, start, end)

Read lines from a buffer.

**Parameters**

| Name | Type | Description |
|---|---|---|
| `buf` | `integer` | Buffer ID. `0` conventionally refers to the current buffer. |
| `start` | `integer` | First line index (0-based, inclusive). |
| `end` | `integer` | Last line index (0-based, exclusive). `-1` means the last line. |

**Returns:** `string[]` — a Lua array of line strings.

**Implementation note (Phase 2):** The snapshot wiring is not yet connected. This
function currently returns an empty table (`{}`) regardless of arguments. The
call signature and semantics are final; the implementation will be completed in
Phase 3. Plugins that use `get_lines` should handle an empty result gracefully.

**Example**

```lua
local lines = onda.buf.get_lines(0, 0, -1)
for i, line in ipairs(lines) do
    -- process each line
end
```

---

### onda.buf.set_lines(buf, start, end, lines)

Replace a range of lines in a buffer.

**Parameters**

| Name | Type | Description |
|---|---|---|
| `buf` | `integer` | Buffer ID. |
| `start` | `integer` | First line to replace (0-based, inclusive). |
| `end` | `integer` | First line after the replaced range (0-based, exclusive). |
| `lines` | `string[]` | Replacement lines. May be empty to delete the range. |

**Returns:** nothing

**Behaviour:** Enqueues a `BufSetLines` API call. The buffer mutation is applied by
the main loop on the next frame drain and goes through `onda-core`'s
`Transaction`/`ChangeSet` mechanism (ADR compliance — no direct rope mutation).

**Example**

```lua
-- Replace lines 2–4 (0-based) with two new lines
onda.buf.set_lines(0, 2, 4, { "first replacement line", "second replacement line" })

-- Delete line 0
onda.buf.set_lines(0, 0, 1, {})
```

---

### onda.win.get_cursor(win)

Get the cursor position in a window.

**Parameters**

| Name | Type | Description |
|---|---|---|
| `win` | `integer` | Window ID. |

**Returns:** `{ row: integer, col: integer }` — a table with `row` and `col` fields
(both 0-based).

**Implementation note (Phase 2):** The snapshot wiring is not yet connected. This
function always returns `{ row = 0, col = 0 }`. The call signature and return shape
are final.

**Example**

```lua
local pos = onda.win.get_cursor(0)
onda.notify("cursor at row=" .. pos.row .. " col=" .. pos.col)
```

---

### onda.win.set_cursor(win, pos)

Move the cursor in a window.

**Parameters**

| Name | Type | Description |
|---|---|---|
| `win` | `integer` | Window ID. |
| `pos` | `{ row: integer, col: integer }` | Target position. Missing keys default to `0`. |

**Returns:** nothing

**Behaviour:** Enqueues a `WinSetCursor` API call.

**Example**

```lua
-- Move cursor to line 10, column 0
onda.win.set_cursor(0, { row = 9, col = 0 })
```

---

### onda.keymap.set(mode, lhs, callback_id, opts)

Register a keybinding that fires a Lua callback.

**Parameters**

| Name | Type | Description |
|---|---|---|
| `mode` | `string` | Vim mode string. Common values: `"n"` (normal), `"i"` (insert), `"v"` (visual). |
| `lhs` | `string` | Key sequence to bind, using onda key notation (e.g. `"<Space>rb"`, `"<C-p>"`). |
| `callback_id` | `integer` | Numeric ID that maps to a function in `_onda_callbacks`. See [Callback Registry Protocol](#callback-registry-protocol). |
| `opts` | `table?` | Optional options table. |

**`opts` fields**

| Field | Type | Default | Description |
|---|---|---|---|
| `noremap` | `boolean` | `true` | Prevent recursive mapping. |
| `silent` | `boolean` | `false` | Suppress display of the key sequence in the message line. |
| `desc` | `string` | `nil` | Human-readable description shown in key listing commands. |

**Returns:** nothing

**Behaviour:** Enqueues a `KeymapSet` API call. When the registered key sequence is
pressed, the runtime fires `_onda_callbacks[callback_id]()`.

**Example**

```lua
_onda_callbacks = _onda_callbacks or {}
local MY_CALLBACK_ID = 2001

_onda_callbacks[MY_CALLBACK_ID] = function()
    onda.notify("My keybinding fired!")
end

onda.keymap.set("n", "<Space>x", MY_CALLBACK_ID, {
    noremap = true,
    silent  = true,
    desc    = "My custom action",
})
```

---

### onda.cmd.create(name, callback_id, opts)

Register a custom editor command (callable as `:Name` from the command line).

**Parameters**

| Name | Type | Description |
|---|---|---|
| `name` | `string` | Command name. Must start with an uppercase letter by convention. |
| `callback_id` | `integer` | Numeric ID that maps to a function in `_onda_callbacks`. See [Callback Registry Protocol](#callback-registry-protocol). |
| `opts` | `table?` | Optional options table. |

**`opts` fields**

| Field | Type | Default | Description |
|---|---|---|---|
| `nargs` | `integer` | `0` | Number of arguments the command accepts. |
| `desc` | `string` | `nil` | Human-readable description. |

**Returns:** nothing

**Behaviour:** Enqueues a `CmdCreate` API call. When the command is invoked, the
runtime calls `_onda_callbacks[callback_id](args)` where `args` is a 1-based Lua
array of string arguments.

**Example**

```lua
_onda_callbacks = _onda_callbacks or {}
local GREET_ID = 3001

_onda_callbacks[GREET_ID] = function(args)
    local name = args[1] or "world"
    onda.notify("Hello, " .. name .. "!")
end

onda.cmd.create("Greet", GREET_ID, {
    nargs = 1,
    desc  = "Greet someone by name",
})
-- Usage: :Greet Alice
```

---

### onda.ui.float(opts)

Open a floating window displaying static content.

**Parameters**

`opts` is a required table with the following fields:

| Field | Type | Default | Description |
|---|---|---|---|
| `title` | `string` | `""` | Title shown in the floating window border. |
| `lines` | `string[]` | `{}` | Lines of content to display. |
| `width` | `integer` | `40` | Width of the floating window in columns. |
| `height` | `integer` | `10` | Height of the floating window in rows. |

**Returns:** nothing

**Behaviour:** Enqueues a `UiFloat` API call. The compositor opens the window on the
next frame drain. There is no return value or window handle; interaction with the
opened window is not yet supported in Phase 2.

**Example**

```lua
onda.ui.float({
    title  = "Help",
    lines  = { "Line one", "Line two", "Line three" },
    width  = 50,
    height = 5,
})
```

---

### onda.autocmd.create(event, pattern, callback_id)

Register an automatic command that fires on an editor event.

**Parameters**

| Name | Type | Description |
|---|---|---|
| `event` | `string` | Event name (e.g. `"BufEnter"`, `"BufWrite"`, `"CursorMoved"`). The full event list will be documented when the autocmd system stabilises in Phase 3. |
| `pattern` | `string` | File pattern filter (e.g. `"*.rs"`, `"*"`). |
| `callback_id` | `integer` | Numeric ID that maps to a function in `_onda_callbacks`. |

**Returns:** nothing

**Behaviour:** Enqueues an `AutocmdCreate` API call. When the matched event fires on
a file matching `pattern`, the runtime calls `_onda_callbacks[callback_id]()`.

**Example**

```lua
_onda_callbacks = _onda_callbacks or {}
local ON_BUF_ENTER_ID = 4001

_onda_callbacks[ON_BUF_ENTER_ID] = function()
    onda.notify("Entered a Rust buffer", "info")
end

onda.autocmd.create("BufEnter", "*.rs", ON_BUF_ENTER_ID)
```

---

### onda.highlight.set(group, opts)

Define or override a theme highlight group (T18.1). Overrides apply immediately and
persist across `:theme` switches (re-applied on top of every newly-loaded theme).

**Parameters**

| Name | Type | Description |
|---|---|---|
| `group` | `string` | Scope name, e.g. `"syntax.keyword"`, `"ui.statusline"` (see `docs/THEMES.md`). |
| `opts` | `table` | `{ fg, bg, bold, italic, underline }`. `fg`/`bg` are `#rrggbb` or ANSI names; flags are booleans (default false). |

**Returns:** nothing

**Behaviour:** Enqueues a `HighlightSet` API call applied between frames; triggers a
full damage-tracked re-render.

**Example**

```lua
onda.highlight.set("syntax.keyword", { fg = "#ff79c6", bold = true })
onda.highlight.set("ui.statusline", { fg = "black", bg = "#88c0d0" })
```

---

## Callback Registry Protocol

onda's API functions that need to call back into Lua (`keymap.set`, `cmd.create`,
`autocmd.create`) do not accept Lua functions directly. Instead they take a numeric
`callback_id`. The plugin is responsible for registering the actual function in the
global `_onda_callbacks` table before (or at the same time as) the API call.

```lua
-- Initialise the registry if another plugin created it first.
_onda_callbacks = _onda_callbacks or {}

local MY_ID = 9001                        -- pick a unique integer
_onda_callbacks[MY_ID] = function()       -- register the function
    onda.notify("fired!")
end
onda.keymap.set("n", "<Space>z", MY_ID)   -- pass only the id
```

When the runtime fires the callback (`fire_keybinding`, `fire_command`) it looks up
`_onda_callbacks[id]` and calls it.

**Choosing IDs:** There is no allocation mechanism in Phase 2. Use large, plugin-specific
integers to avoid collisions with other plugins. The bundled runtime plugins use the
range 1001–1004; user plugins should use values above 2000 or use a naming convention
derived from the plugin name.

---

## Performance Contract

Lua execution runs on the main thread. The per-callback budget is **500 microseconds
(`LUA_FRAME_BUDGET_US`)**.

- If a keybinding callback exceeds 500 µs, a warning is emitted via the tracing
  subscriber (`warn!`). The editor continues normally; there is no hard abort in the
  current implementation.
- The `drain_calls` method is called once per frame. The channel holds up to 1 024
  queued `LuaApiCall` values. If a plugin enqueues more than 1 024 calls in a single
  frame, excess calls are silently dropped (`try_send` is used, not blocking `send`).
- `onda.notify` and other enqueueing functions return immediately; they do not block
  waiting for the main loop to process the call.
- Avoid loops over large data sets inside plugin callbacks. If you need to process a
  whole file, do it incrementally across multiple keybinding invocations or wait for
  `buf.get_lines` snapshot wiring to be completed in Phase 3.
- The render pipeline must hit 60 fps (16 ms frame budget). Every µs spent in Lua is
  time not available to rendering. Keep callbacks short.

---

## Example Plugins

Three reference plugins ship in `runtime/plugins/`. They demonstrate the correct
patterns for using the API.

### rainbow_brackets.lua

**Location:** `runtime/plugins/rainbow_brackets.lua`

Demonstrates: `onda.notify`, `onda.keymap.set`, `onda.cmd.create`

Registers a normal-mode keybinding (`<Space>rb`) and an editor command
(`:RainbowToggle`) that both call the same toggle function. The toggle flips a
module-level boolean and notifies the user of the new state. The actual decoration
logic is a stub pending render-layer integration in a later phase.

Key patterns shown:
- Using `_onda_callbacks = _onda_callbacks or {}` to safely initialise the shared
  registry table.
- Registering the same callback ID for both a keymap and a command.
- `noremap = true, silent = true` as the default keybinding option set.

### word_count.lua

**Location:** `runtime/plugins/word_count.lua`

Demonstrates: `onda.buf.get_lines`, `onda.notify`, `onda.cmd.create`

Registers `:WordCount`. The command handler calls `onda.buf.get_lines(0, 0, -1)`
to read all lines of the current buffer, counts whitespace-delimited tokens with
`string.gmatch`, and reports the total via `onda.notify`.

Key patterns shown:
- Passing `0` as the buffer ID for "current buffer".
- Passing `-1` as the end index to read to the last line.
- Handling the Phase 2 stub state: when `get_lines` returns `{}` the word count
  is simply 0, which is a valid (if incomplete) result.

### project_todos.lua

**Location:** `runtime/plugins/project_todos.lua`

Demonstrates: `onda.notify`, `onda.cmd.create` (Phase 3 stub)

Registers `:ProjectTodos`. The command body is a Phase 3 stub that notifies the
user to use `:grep TODO` as a workaround. The plugin is intentionally minimal; it
documents the intended Phase 3 shape in its source comments
(`onda.ui.float`-based picker).

Key patterns shown:
- How to stub a Phase 3 feature with a useful fallback message today.
- Minimal plugin structure: one callback, one command.

---

## Migration / Compatibility

**Phase 2 (current):** The API is unstable. The following aspects are known to be
incomplete or subject to change:

- `onda.buf.get_lines` always returns `{}`. Snapshot wiring is not connected.
- `onda.win.get_cursor` always returns `{ row = 0, col = 0 }`. Snapshot wiring is
  not connected.
- `onda.ui.float` opens a window but there is no API to interact with it, close it
  programmatically, or receive input from it.
- `onda.autocmd.create` is registered but the full event vocabulary is not
  documented; only the registration call is stable.
- The `_onda_callbacks` integer ID protocol is a Phase 2 workaround. Phase 3 will
  introduce a cleaner function-reference or named-callback mechanism.
- No plugin versioning, no dependency declaration, no load-order guarantees.

**Phase 3 (planned):**

- `buf.get_lines` and `win.get_cursor` will return real data from the buffer
  snapshot prepared before each Lua frame.
- The callback protocol will be replaced or supplemented with a cleaner surface.
- `onda.ui.float` will gain interaction support (input handling, close callbacks).
- The full autocmd event list will be documented and stabilised.
- A migration guide will be published at the time of the Phase 3 release.

**Recommendation for plugin authors today:** write plugins against the current API,
accept that `get_lines` / `get_cursor` return stub values, and use `onda.notify` as
the primary output mechanism. Follow the `_onda_callbacks` pattern shown in the
reference plugins. Avoid relying on specific callback ID ranges; use values above
2000.
