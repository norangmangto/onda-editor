# Themes (onda)

onda ships three built-in themes and supports user themes + live reload (T18.1).

## Switching themes

- `:theme` — show the active theme and the list of built-ins.
- `:theme <name>` — switch live (full re-render, < 5ms). Built-ins:
  `onda-dark` (default), `onda-light`, `onda-contrast` (WCAG AA high-contrast),
  `onda-wave` (the brand ocean theme).
- `config.toml` `theme = "onda-light"` sets the default at startup.

## Theme files

Themes are TOML. Keys are scope names; values are style tables:

```toml
"ui.text"        = { fg = "#c0c0c0", bg = "#101010" }
"ui.statusline"  = { fg = "white", bg = "darkgray" }
"syntax.keyword" = { fg = "#ff79c6", bold = true }
"diff.add"       = { fg = "green" }
```

Colors are `#rrggbb` hex or basic ANSI names (`red`, `lightcyan`, `reset`, …).
Style flags: `bold`, `italic`, `underline` (all default `false`).

A theme may inherit a built-in and override only the scopes it cares about:

```toml
inherits = "onda-dark"
"ui.text" = { fg = "#d6e9f0", bg = "#0b1e2d" }
```

### Resolution order

`:theme <name>` and the startup `theme` setting look for, in order:

1. `~/.config/onda/themes/<name>.toml`
2. `runtime/themes/<name>.toml` (repo checkout / install tree)
3. the embedded built-in named `<name>`
4. `onda-dark` (fallback)

When a theme resolves to an on-disk file it is **watched**: edits hot-reload within
~100ms (debounced), and a malformed file shows an error on the message line instead
of crashing.

### Recognized scopes

`ui.text`, `ui.cursor`, `ui.cursor.insert`, `ui.selection`, `ui.linenr`,
`ui.linenr.current`, `ui.statusline`, `ui.statusline.{normal,insert,visual,terminal}`,
`ui.message.error`, `ui.message.info`, `ui.menu`, `ui.menu.selected`, `ui.float`,
`ui.float.border`, `diagnostic.{error,warning,info}`, `gutter.{error,warning}`,
`diff.{add,delete,change}`, `syntax.{keyword,type,function,string,number,comment,constant,operator}`.

Any scope a theme omits falls back to a built-in dark default, so partial themes are fine.

## Plugins: custom highlight groups

WASM plugins (ADR-002) can define or override any highlight group through the
`decorations.set-group` host call; overrides persist across theme switches
(re-applied on top of every newly-loaded theme). A `style` is
`{ fg, bg, bold, italic, underline }` where `fg`/`bg` are `#rrggbb` or ANSI names
and the flags are booleans:

```rust
use onda::plugin::decorations;
use onda::plugin::types::Style;

decorations::set_group("syntax.keyword", &Style {
    fg: Some("#ff0000".into()), bg: None,
    bold: true, italic: false, underline: false,
});
decorations::set_group("ui.statusline", &Style {
    fg: Some("black".into()), bg: Some("#88c0d0".into()),
    bold: false, italic: false, underline: false,
});
```

See `docs/PLUGIN_API.md` and `wit/onda/*.wit` for the full plugin surface.
