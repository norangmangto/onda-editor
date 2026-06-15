# onda plugin quickstart

Write a WASM plugin for onda in ~15 minutes. Plugins are WebAssembly **components**
(ADR-002) targeting the host API in `wit/onda/*.wit`. Rust is the first-class
language; any language with WIT bindings works.

## Prerequisites

```sh
rustup target add wasm32-wasip2     # builds Rust → WASM components directly
```

## 1. Create the crate

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

## 2. The manifest

```toml
# onda-plugin.toml
[plugin]
name = "my-plugin"
version = "0.1.0"
entry = "my_plugin.wasm"
min-api-version = "0.1"

[permissions]
buffer = "read"        # none | read | write
# filesystem = ["./.git"]   # project-root-relative whitelist (capability)
# network = false           # capability

[activation]
events = ["buffer-open", "buffer-change"]   # lazy — protects startup
```

Only declared capabilities are wired in, and only after the user approves. A
plugin that imports `fs`/`http` without the grant **fails to load** — declaration
is enforcement.

## 3. The code

```rust
wit_bindgen::generate!({ world: "plugin", path: "wit/onda" });

use exports::onda::plugin::guest::{Event, Guest};
use onda::plugin::{buffer, log};

struct Plugin;

impl Guest for Plugin {
    fn init() { log::debug("my-plugin activated"); }

    fn handle_event(ev: Event) {
        if let Event::BufferOpen(b) = ev {
            let n = buffer::line_count(b.buf).unwrap_or(0);
            log::notify(&format!("{} has {n} lines", b.path),
                        onda::plugin::types::NotifyLevel::Info);
        }
    }

    fn run_command(_id: u64, _args: Vec<String>) {}
    fn run_keymap(_id: u64) {}
}

export!(Plugin);
```

Import paths mirror the package: host interfaces are `onda::plugin::<iface>`,
the guest trait is `exports::onda::plugin::guest::Guest`.

## 4. Build & install

```sh
cargo build --release --target wasm32-wasip2
onda plugin install ./my-plugin            # local dir, or github:user/repo[@rev]
```

## Rules that matter (and the host enforces)

- **Never block.** Host calls are non-blocking; handlers run under a 5ms budget.
  Exceed it and you're trapped + demoted. Do heavy work incrementally.
- **All edits are transactions.** `buffer.apply` is one undo step; positions are
  char indices. There is no raw buffer access.
- **Selection is 1..N ranges** (ADR-006). Don't assume one cursor.
- **Decorations are batched.** Submit the whole set for a namespace per frame via
  `decorations.set`; don't call per item.

See `wit/README.md` for the full interface reference and `wit/onda/*.wit` for the
typed surface. The reference plugins under `plugins/` are working examples.
