-- rainbow_brackets.lua
--
-- Toggles rainbow-bracket colouring on the current buffer.  The actual
-- decoration logic will be provided by the render layer in a later phase;
-- this plugin owns the user-facing toggle surface.
--
-- API used:
--   onda.notify(msg, level)
--   onda.keymap.set(mode, lhs, callback_id, opts)
--   onda.cmd.create(name, callback_id, opts)
--
-- Keybinding:  <Space>rb  (normal mode) — same as :RainbowToggle
-- Usage:       :RainbowToggle

_onda_callbacks = _onda_callbacks or {}

local RAINBOW_KEYMAP_ID = 1003
local RAINBOW_CMD_ID    = 1004

-- Module-level state.  Persists for the lifetime of the Lua VM.
local enabled = false

local function toggle()
    enabled = not enabled
    if enabled then
        onda.notify("Rainbow brackets: enabled", "info")
    else
        onda.notify("Rainbow brackets: disabled", "info")
    end
end

-- Register callbacks by their numeric IDs.
_onda_callbacks[RAINBOW_KEYMAP_ID] = toggle
_onda_callbacks[RAINBOW_CMD_ID]    = toggle

-- Normal-mode keybinding: <Space>rb
onda.keymap.set("n", "<Space>rb", RAINBOW_KEYMAP_ID, {
    noremap = true,
    silent  = true,
    desc    = "Toggle rainbow brackets",
})

-- Editor command for command-line access.
onda.cmd.create("RainbowToggle", RAINBOW_CMD_ID, {
    nargs = 0,
    desc  = "Toggle rainbow bracket colouring",
})
