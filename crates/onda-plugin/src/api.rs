//! Host-call queue (rule 2): the effectful calls a plugin makes are enqueued as
//! `PluginApiCall` values and applied by the main loop between frames — a plugin
//! never reaches into editor state directly. This mirrors the `host.wit` write
//! surface (reads are answered synchronously from a pre-frame snapshot).
//!
//! This is the typed replacement for `onda_lua::api::LuaApiCall`; the binary's
//! `drain_lua_calls` becomes `drain_plugin_calls` over this enum in W18.

/// Severity for a notify call (mirrors `wit` `types.notify-level`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifyLevel {
    Info,
    Warn,
    Error,
}

/// Style for a decoration/highlight (mirrors `wit` `types.style`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Style {
    pub fg: Option<String>,
    pub bg: Option<String>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
}

/// One transactional edit over the pre-edit coordinate space (`host.buffer.edit`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub start: usize,
    pub end: usize,
    pub text: String,
}

/// A namespaced decoration batch (`host.decorations.batch`) — batch-only surface.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DecorationBatch {
    pub namespace: String,
    pub virt_texts: Vec<(usize, String, Style)>,
    pub signs: Vec<(usize, String, Style)>,
    pub highlights: Vec<(usize, usize, Style)>,
}

/// An effectful call from a plugin to the editor, drained once per frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginApiCall {
    Notify {
        msg: String,
        level: NotifyLevel,
    },
    /// Apply a batch of edits to a buffer as one transaction (one undo step).
    BufferApply {
        buf_id: u64,
        edits: Vec<Edit>,
    },
    SetCursor {
        win_id: u64,
        pos: usize,
    },
    SetSelection {
        buf_id: u64,
        ranges: Vec<(usize, usize)>,
        primary: u32,
    },
    KeymapSet {
        mode: String,
        lhs: String,
        callback_id: u64,
        desc: Option<String>,
    },
    CmdCreate {
        name: String,
        callback_id: u64,
        desc: Option<String>,
        nargs: u8,
    },
    UiFloat {
        title: String,
        lines: Vec<String>,
        width: u16,
        height: u16,
    },
    UiPick {
        title: String,
        items: Vec<(String, Option<String>)>,
        callback_id: u64,
    },
    StatuslineSegment {
        id: String,
        text: String,
        style: Style,
    },
    SetDecorations {
        buf_id: u64,
        batch: DecorationBatch,
    },
    ClearDecorations {
        buf_id: u64,
        namespace: String,
    },
    HighlightGroup {
        group: String,
        style: Style,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calls_are_comparable_for_assertions() {
        let a = PluginApiCall::Notify {
            msg: "hi".into(),
            level: NotifyLevel::Info,
        };
        let b = a.clone();
        assert_eq!(a, b);
    }
}
