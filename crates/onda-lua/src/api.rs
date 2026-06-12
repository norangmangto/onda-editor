//! `onda.*` Lua API surface.
//!
//! All Lua→Rust calls are enqueued as `LuaApiCall` values and processed by
//! the main loop between frames (rule 2: Lua never calls directly into editor
//! state).

// ── LuaApiCall ────────────────────────────────────────────────────────────────

/// A queued call from a Lua plugin to the editor.
///
/// The main loop drains these once per frame and applies them to `App`.
#[derive(Debug, Clone)]
pub enum LuaApiCall {
    /// `onda.notify(msg, level)` — show a message in the message line.
    Notify { msg: String, level: NotifyLevel },
    /// `onda.buf.set_lines(buf, start, end, lines)` — replace lines in a buffer.
    BufSetLines {
        buf_id: usize,
        start: usize,
        end: usize,
        lines: Vec<String>,
    },
    /// `onda.win.set_cursor(win, {row, col})` — move the cursor.
    WinSetCursor {
        win_id: usize,
        row: usize,
        col: usize,
    },
    /// `onda.keymap.set(mode, lhs, callback_id, opts)` — register a keybinding.
    KeymapSet {
        mode: String,
        lhs: String,
        callback_id: u64,
        opts: KeymapOpts,
    },
    /// `onda.cmd.create(name, callback_id, opts)` — register a custom command.
    CmdCreate {
        name: String,
        callback_id: u64,
        opts: CmdOpts,
    },
    /// `onda.ui.float(opts)` — open a floating window with content.
    UiFloat {
        title: String,
        lines: Vec<String>,
        width: u16,
        height: u16,
    },
    /// `onda.autocmd.create(event, pattern, callback_id)` — register an autocmd.
    AutocmdCreate {
        event: String,
        pattern: String,
        callback_id: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifyLevel {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, Default)]
pub struct KeymapOpts {
    pub noremap: bool,
    pub silent: bool,
    pub desc: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct CmdOpts {
    pub nargs: u8,
    pub desc: Option<String>,
}

// ── API injection ─────────────────────────────────────────────────────────────

/// Inject the `onda` global namespace into the Lua VM.
///
/// Most functions enqueue a `LuaApiCall`; reads (get_lines, get_cursor) are
/// answered synchronously from a snapshot of editor state that was prepared
/// before the Lua frame budget begins.
pub fn inject(
    lua: &mlua::Lua,
    call_tx: std::sync::mpsc::SyncSender<LuaApiCall>,
) -> mlua::Result<()> {
    let globals = lua.globals();

    let onda = lua.create_table()?;

    // ── onda.notify ───────────────────────────────────────────────────────────
    {
        let tx = call_tx.clone();
        let notify =
            lua.create_function(move |_lua, (msg, level_str): (String, Option<String>)| {
                let level = match level_str.as_deref() {
                    Some("warn") => NotifyLevel::Warn,
                    Some("error") => NotifyLevel::Error,
                    _ => NotifyLevel::Info,
                };
                let _ = tx.try_send(LuaApiCall::Notify { msg, level });
                Ok(())
            })?;
        onda.set("notify", notify)?;

        // Alias: onda.log("msg") for convenience
        let tx2 = call_tx.clone();
        let log_fn = lua.create_function(move |_lua, msg: String| {
            let _ = tx2.try_send(LuaApiCall::Notify {
                msg,
                level: NotifyLevel::Info,
            });
            Ok(())
        })?;
        onda.set("log", log_fn)?;
    }

    // ── onda.buf ──────────────────────────────────────────────────────────────
    {
        let buf = lua.create_table()?;

        let tx = call_tx.clone();
        let set_lines = lua.create_function(
            move |_lua, (buf_id, start, end, lines): (usize, usize, usize, Vec<String>)| {
                let _ = tx.try_send(LuaApiCall::BufSetLines {
                    buf_id,
                    start,
                    end,
                    lines,
                });
                Ok(())
            },
        )?;
        buf.set("set_lines", set_lines)?;

        // get_lines: returns empty by default (snapshot not wired here)
        let get_lines =
            lua.create_function(|_lua, (_buf_id, _start, _end): (usize, usize, usize)| {
                Ok(Vec::<String>::new())
            })?;
        buf.set("get_lines", get_lines)?;

        onda.set("buf", buf)?;
    }

    // ── onda.win ──────────────────────────────────────────────────────────────
    {
        let win = lua.create_table()?;

        let tx = call_tx.clone();
        let set_cursor =
            lua.create_function(move |_lua, (win_id, pos): (usize, mlua::Table)| {
                let row: usize = pos.get("row").unwrap_or(0);
                let col: usize = pos.get("col").unwrap_or(0);
                let _ = tx.try_send(LuaApiCall::WinSetCursor { win_id, row, col });
                Ok(())
            })?;
        win.set("set_cursor", set_cursor)?;

        let get_cursor = lua.create_function(|lua, _win_id: usize| {
            let t = lua.create_table()?;
            t.set("row", 0usize)?;
            t.set("col", 0usize)?;
            Ok(t)
        })?;
        win.set("get_cursor", get_cursor)?;

        onda.set("win", win)?;
    }

    // ── onda.keymap ───────────────────────────────────────────────────────────
    {
        let keymap = lua.create_table()?;
        let tx = call_tx.clone();
        // callback_id is a number the plugin uses to identify the Lua function
        let set_fn =
            lua.create_function(
                move |_lua,
                      (mode, lhs, callback_id, opts): (
                    String,
                    String,
                    u64,
                    Option<mlua::Table>,
                )| {
                    let noremap = opts
                        .as_ref()
                        .and_then(|t| t.get::<bool>("noremap").ok())
                        .unwrap_or(true);
                    let silent = opts
                        .as_ref()
                        .and_then(|t| t.get::<bool>("silent").ok())
                        .unwrap_or(false);
                    let desc = opts.as_ref().and_then(|t| t.get::<String>("desc").ok());
                    let _ = tx.try_send(LuaApiCall::KeymapSet {
                        mode,
                        lhs,
                        callback_id,
                        opts: KeymapOpts {
                            noremap,
                            silent,
                            desc,
                        },
                    });
                    Ok(())
                },
            )?;
        keymap.set("set", set_fn)?;
        onda.set("keymap", keymap)?;
    }

    // ── onda.cmd ──────────────────────────────────────────────────────────────
    {
        let cmd = lua.create_table()?;
        let tx = call_tx.clone();
        let create_fn = lua.create_function(
            move |_lua, (name, callback_id, opts): (String, u64, Option<mlua::Table>)| {
                let nargs = opts
                    .as_ref()
                    .and_then(|t| t.get::<u8>("nargs").ok())
                    .unwrap_or(0);
                let desc = opts.as_ref().and_then(|t| t.get::<String>("desc").ok());
                let _ = tx.try_send(LuaApiCall::CmdCreate {
                    name,
                    callback_id,
                    opts: CmdOpts { nargs, desc },
                });
                Ok(())
            },
        )?;
        cmd.set("create", create_fn)?;
        onda.set("cmd", cmd)?;
    }

    // ── onda.ui ───────────────────────────────────────────────────────────────
    {
        let ui = lua.create_table()?;
        let tx = call_tx.clone();
        let float_fn = lua.create_function(move |_lua, opts: mlua::Table| {
            let title: String = opts.get("title").unwrap_or_default();
            let lines: Vec<String> = opts.get("lines").unwrap_or_default();
            let width: u16 = opts.get::<u16>("width").unwrap_or(40);
            let height: u16 = opts.get::<u16>("height").unwrap_or(10);
            let _ = tx.try_send(LuaApiCall::UiFloat {
                title,
                lines,
                width,
                height,
            });
            Ok(())
        })?;
        ui.set("float", float_fn)?;
        onda.set("ui", ui)?;
    }

    // ── onda.autocmd ──────────────────────────────────────────────────────────
    {
        let autocmd = lua.create_table()?;
        let tx = call_tx.clone();
        let create_fn = lua.create_function(
            move |_lua, (event, pattern, callback_id): (String, String, u64)| {
                let _ = tx.try_send(LuaApiCall::AutocmdCreate {
                    event,
                    pattern,
                    callback_id,
                });
                Ok(())
            },
        )?;
        autocmd.set("create", create_fn)?;
        onda.set("autocmd", autocmd)?;
    }

    globals.set("onda", onda)?;
    Ok(())
}
