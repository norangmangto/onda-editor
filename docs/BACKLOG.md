# Backlog

Agent follow-up notes — items deferred from the current phase.

## Completed in Phase 1

Items that were Phase 0 deferrals and landed during Phase 1:

- **Visual-block mode** (`Ctrl-v`): implemented in T6.5 (block selection as multicursor,
  `I`/`A` block insert, `d`/`y`/`p` blockwise).
- **Named registers** (`"a`..`"z`, `"0`..`"9`): implemented in T6.2.
- **Tree-sitter syntax highlighting**: implemented in W5 (T5.1–T5.5).
- **Config file loading** (TOML): implemented in T8.2 (`~/.config/onda/config.toml` +
  project overlay).
- **Undo tree** (non-linear): implemented in T6.1 (branch-on-diverge, `g-`/`g+`).
- **`/` search** and substitute: implemented in T7.1 (incremental regex search) and
  T7.2 (`:[range]s/pat/rep/`).
- **Marks** (`m`, `` ` ``, `'`): implemented in T6.5.
- **Macros** (`q`, `@`): implemented in T6.3.
- **Fuzzy file picker**: implemented in T7.3 (`<space>f`, `<space>b`, `nucleo-matcher`).
- **Window splits**: implemented in T8.1 (`:sp`/`:vsp`, `Ctrl-w` navigation).
- **Jumplist** (`Ctrl-o`/`Ctrl-i`): implemented in T6.5.

## Phase 0 deferrals still open

Items from Phase 0 not yet addressed:

- **Mouse support**: not implemented in Phase 0 or Phase 1. Scheduled for T12.3 (Phase 2).
- **`:e` with completion**: bare path only through Phase 1. T8.3 command completion did
  not land. Scheduled for T12.4 (Phase 2) as a quick-win polish item.
- **Kitty keyboard protocol** full suite: basic crossterm support only. Deferred
  indefinitely — revisit when crossterm adds first-class support.
- **Async progressive file loading** (true streaming): T4.2 has basic async loading;
  true progressive streaming deferred to Phase 3.
- **LSP integration**: deferred to Phase 2 (W10, T10.x).
- **Plugin system**: deferred to Phase 2 (W13, T13.x) for Lua foundation.

## Phase 1 deferrals

Items identified during Phase 1 that were explicitly deferred:

- **Soft/word wrap**: no soft wrap through Phase 1. Horizontal scroll only (T8.3 decision
  recorded in `docs/DESIGN.md` changelog). Scheduled for T12.2 (Phase 2).
- **dylib grammar loading hardening**: T5.1 loads grammars via `libloading` but the
  fresh-machine build experience is rough (no auto-fetch on first open). Partial fix in
  T12.4 (Phase 2 grammar auto-fetch); full prebuilt artifact bundling deferred to Phase 5.
- **Clipboard on Linux/Wayland without a clipboard manager** (`arboard` silent failure):
  identified as friction item F-18 in `docs/DOGFOOD.md`. Threading workaround deferred;
  fix targeted in T12.4 hardening.
- **tree-sitter `textobjects.scm` queries**: T6.4 supports `if/af`, `ic/ac`, `ia/aa` when
  a grammar is loaded but the `.scm` query files were not populated for all bundled
  languages. Remaining query files deferred — add per language as they are validated.
- **File tree (T7.4) full feature set**: T7.4 delivered create/delete/open; rename from
  the tree, multi-select operations, and git status badges deferred to Phase 3.
- **Undo-tree visualization picker**: T6.1 implements the undo tree data structure and
  `g-`/`g+` navigation. A visual picker overlay (like `undotree.vim`) is deferred to
  Phase 3 as a BACKLOG item.
- **`:e` command completion and command-name completion** (T8.3 partial): the picker
  matcher is wired but file-path `<Tab>` completion in the command line did not land.
  Scheduled for T12.4 (Phase 2).
- **Hot-reload config on file change**: `:config-reload` works; automatic watch-and-reload
  via `notify` explicitly deferred. Revisit in Phase 3.

## Notes from Phase 0

<!-- Agents: append here as you work. Format: `- [T0.x] Note about friction/decision.` -->

## Notes from Phase 1

<!-- Agents: append here as you work. Format: `- [T5.x–T9.x] Note about friction/decision.` -->
