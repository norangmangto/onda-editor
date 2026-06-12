-- word_count.lua
--
-- Counts words across every line in the current buffer and reports the
-- total via onda.notify.
--
-- API used:
--   onda.buf.get_lines(buf_id, start, end) -> string[]
--   onda.notify(msg, level)
--   onda.cmd.create(name, callback_id, opts)
--
-- Usage:  :WordCount

-- Callback registry.  onda.cmd.create expects a numeric callback_id; the
-- runtime calls _onda_callbacks[id]() when the command fires.
_onda_callbacks = _onda_callbacks or {}

local WORD_COUNT_CMD_ID = 1001

local function count_words()
    -- Buffer 0 is the current buffer; -1 means "to the last line".
    local lines = onda.buf.get_lines(0, 0, -1)

    local count = 0
    for _, line in ipairs(lines) do
        -- Split on any run of whitespace; each non-empty token is a word.
        for _ in line:gmatch("%S+") do
            count = count + 1
        end
    end

    onda.notify("Word count: " .. tostring(count), "info")
end

-- Register the Lua function so the runtime can call it back.
_onda_callbacks[WORD_COUNT_CMD_ID] = count_words

-- Register the editor command.
onda.cmd.create("WordCount", WORD_COUNT_CMD_ID, {
    nargs = 0,
    desc  = "Count words in the current buffer",
})
