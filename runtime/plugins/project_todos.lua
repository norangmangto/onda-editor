-- project_todos.lua
--
-- Stub plugin that will eventually open a picker showing all TODO comments
-- found in the project.  Full picker integration is planned for Phase 3;
-- until then the command notifies the user to use :grep as a workaround.
--
-- API used:
--   onda.notify(msg, level)
--   onda.cmd.create(name, callback_id, opts)
--
-- Usage:  :ProjectTodos

_onda_callbacks = _onda_callbacks or {}

local PROJECT_TODOS_CMD_ID = 1002

local function show_todos()
    -- Phase 3 will replace this body with a onda.ui.float-based picker that
    -- walks the workspace files and surfaces every TODO/FIXME/HACK comment.
    onda.notify("TODO picker: not yet supported - use :grep TODO", "info")
end

_onda_callbacks[PROJECT_TODOS_CMD_ID] = show_todos

onda.cmd.create("ProjectTodos", PROJECT_TODOS_CMD_ID, {
    nargs = 0,
    desc  = "Show project TODO comments (Phase 3 stub)",
})
