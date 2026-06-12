# Backlog

Agent follow-up notes — items deferred from the current phase.

## Phase 0 deferrals

- **Visual-block mode** (`Ctrl-v`): deferred to Phase 1. Note in T2.4.
- **Soft/word wrap**: no soft wrap in Phase 0. Horizontal scroll only. Note in T3.3.
- **Named registers** (`"a`..`"z`, `"0`..`"9`): single unnamed register in Phase 0. Note in T2.3.
- **Tree-sitter syntax highlighting**: deferred to Phase 1.
- **LSP integration**: deferred to Phase 1.
- **Plugin system**: deferred to Phase 2+.
- **Config file loading** (TOML): onda-config is a stub; keymaps are static tables in Phase 0. Note in T2.1.
- **Undo tree** (non-linear): linear stack in Phase 0, trait-based so Phase 1 can swap it. Note in T1.4.
- **Kitty keyboard protocol** full suite: basic crossterm support in Phase 0. Note in T3.1.
- **Async progressive file loading** (show first screen before full rope build): T4.2 has basic async loading; true progressive streaming deferred.
- **Mouse support**: not implemented in Phase 0.
- **`:e` with completion**: bare path only in Phase 0.
- **`/` search**: deferred to Phase 1.
- **Marks** (`m`, `` ` ``, `'`): deferred to Phase 1.
- **Macros** (`q`, `@`): deferred to Phase 1.

## Notes from Phase 0

<!-- Agents: append here as you work. Format: `- [T0.x] Note about friction/decision.` -->
