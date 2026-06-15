//! WASM host bindings + host-side state (W18, T18.1/T18.3).
//!
//! `bindgen!` generates the typed host trait surface from `wit/onda/*.wit`. The
//! editor's effectful calls are routed into the [`PluginApiCall`] queue (drained
//! by the main loop between frames — rule 2); reads are answered from a snapshot
//! of buffer state captured before the plugin's frame budget begins. No host
//! function blocks or holds an editor lock.
//!
//! Host imports are non-trappable (wasmtime default): each method returns the
//! bare WIT value, with `result<T, host-error>` mapping to `Result<T, HostError>`.

use std::collections::HashMap;
use std::path::PathBuf;

use wasmtime::component::ResourceTable;
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use crate::api::{DecorationBatch, Edit, NotifyLevel, PluginApiCall, Style};
use crate::permission::GrantedCaps;

pub mod bindings {
    wasmtime::component::bindgen!({
        world: "plugin",
        path: "../../wit/onda",
    });
}

use bindings::onda::plugin::buffer::Edit as WitEdit;
use bindings::onda::plugin::decorations::{Batch, Highlight, Sign, VirtText};
use bindings::onda::plugin::types::{
    HostError, Mode, NotifyLevel as WitLevel, Range, Selection, Style as WitStyle,
};

/// A read-only snapshot of one buffer, captured before a plugin handler runs.
#[derive(Debug, Clone, Default)]
pub struct BufferSnapshot {
    pub text: String,
    pub lines: Vec<String>,
}

impl BufferSnapshot {
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        let lines = text.split('\n').map(|s| s.to_string()).collect();
        Self { text, lines }
    }

    fn char_len(&self) -> u32 {
        self.text.chars().count() as u32
    }

    fn slice(&self, start: u32, end: u32) -> Option<String> {
        let len = self.char_len();
        if start > end || end > len {
            return None;
        }
        Some(
            self.text
                .chars()
                .skip(start as usize)
                .take((end - start) as usize)
                .collect(),
        )
    }
}

/// Per-instance host state stored in the wasmtime `Store`.
pub struct PluginHostState {
    wasi: WasiCtx,
    table: ResourceTable,
    pub(crate) limits: wasmtime::StoreLimits,
    buffers: HashMap<u64, BufferSnapshot>,
    calls: Vec<PluginApiCall>,
    caps: GrantedCaps,
    project_root: PathBuf,
}

impl PluginHostState {
    pub fn new(caps: GrantedCaps, project_root: PathBuf, limits: wasmtime::StoreLimits) -> Self {
        Self {
            wasi: WasiCtxBuilder::new().build(),
            table: ResourceTable::new(),
            limits,
            buffers: HashMap::new(),
            calls: Vec::new(),
            caps,
            project_root,
        }
    }

    /// Install/replace the read snapshot for a buffer.
    pub fn set_buffer_snapshot(&mut self, buf_id: u64, snap: BufferSnapshot) {
        self.buffers.insert(buf_id, snap);
    }

    /// Take all queued effectful calls (drained by the main loop per frame).
    pub fn take_calls(&mut self) -> Vec<PluginApiCall> {
        std::mem::take(&mut self.calls)
    }

    fn buf(&self, id: u64) -> Result<&BufferSnapshot, HostError> {
        self.buffers.get(&id).ok_or(HostError::InvalidHandle)
    }

    /// Resolve a plugin-supplied path against the project root (relative paths)
    /// and confirm it falls within a granted fs preopen.
    fn resolve_fs(&self, path: &str) -> Result<PathBuf, HostError> {
        let raw = PathBuf::from(path);
        let abs = if raw.is_absolute() {
            raw
        } else {
            self.project_root.join(raw)
        };
        if self.caps.fs_allows(&abs) {
            Ok(abs)
        } else {
            Err(HostError::PermissionDenied(path.to_string()))
        }
    }
}

impl WasiView for PluginHostState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

// ── conversions: WIT host types → crate::api types ──────────────────────────

fn level_from(w: WitLevel) -> NotifyLevel {
    match w {
        WitLevel::Info => NotifyLevel::Info,
        WitLevel::Warn => NotifyLevel::Warn,
        WitLevel::Error => NotifyLevel::Error,
    }
}

fn style_from(s: WitStyle) -> Style {
    Style {
        fg: s.fg,
        bg: s.bg,
        bold: s.bold,
        italic: s.italic,
        underline: s.underline,
    }
}

fn batch_from(b: Batch) -> DecorationBatch {
    DecorationBatch {
        namespace: b.namespace,
        virt_texts: b
            .virt_texts
            .into_iter()
            .map(|v: VirtText| (v.at as usize, v.text, style_from(v.style)))
            .collect(),
        signs: b
            .signs
            .into_iter()
            .map(|s: Sign| (s.line as usize, s.text, style_from(s.style)))
            .collect(),
        highlights: b
            .highlights
            .into_iter()
            .map(|h: Highlight| {
                (
                    h.range.anchor as usize,
                    h.range.head as usize,
                    style_from(h.style),
                )
            })
            .collect(),
    }
}

// ── Host trait implementations (non-trappable: bare returns) ────────────────

impl bindings::onda::plugin::log::Host for PluginHostState {
    fn notify(&mut self, msg: String, level: WitLevel) {
        self.calls.push(PluginApiCall::Notify {
            msg,
            level: level_from(level),
        });
    }
    fn debug(&mut self, msg: String) {
        tracing::debug!(target: "onda_plugin", "{msg}");
    }
}

impl bindings::onda::plugin::buffer::Host for PluginHostState {
    fn current(&mut self) -> u64 {
        // The host installs the focused buffer's snapshot under the lowest id.
        self.buffers.keys().copied().min().unwrap_or(0)
    }
    fn len(&mut self, buf: u64) -> Result<u32, HostError> {
        self.buf(buf).map(|b| b.char_len())
    }
    fn line_count(&mut self, buf: u64) -> Result<u32, HostError> {
        self.buf(buf).map(|b| b.lines.len() as u32)
    }
    fn text(&mut self, buf: u64, range: Range) -> Result<String, HostError> {
        let b = self.buf(buf)?;
        b.slice(range.anchor, range.head)
            .ok_or(HostError::OutOfBounds)
    }
    fn lines(&mut self, buf: u64, start: u32, end: u32) -> Result<Vec<String>, HostError> {
        let b = self.buf(buf)?;
        if start > end || end as usize > b.lines.len() {
            Err(HostError::OutOfBounds)
        } else {
            Ok(b.lines[start as usize..end as usize].to_vec())
        }
    }
    fn apply(&mut self, buf: u64, edits: Vec<WitEdit>) -> Result<(), HostError> {
        if !self.caps.can_write_buffer() {
            return Err(HostError::PermissionDenied("buffer write".into()));
        }
        let edits = edits
            .into_iter()
            .map(|e| Edit {
                start: e.range.anchor as usize,
                end: e.range.head as usize,
                text: e.text,
            })
            .collect();
        self.calls
            .push(PluginApiCall::BufferApply { buf_id: buf, edits });
        Ok(())
    }
}

impl bindings::onda::plugin::selection::Host for PluginHostState {
    fn get(&mut self, _buf: u64) -> Result<Selection, HostError> {
        Ok(Selection {
            ranges: vec![Range { anchor: 0, head: 0 }],
            primary: 0,
        })
    }
    fn set(&mut self, buf: u64, sel: Selection) -> Result<(), HostError> {
        let ranges = sel
            .ranges
            .iter()
            .map(|r| (r.anchor as usize, r.head as usize))
            .collect();
        self.calls.push(PluginApiCall::SetSelection {
            buf_id: buf,
            ranges,
            primary: sel.primary,
        });
        Ok(())
    }
}

impl bindings::onda::plugin::editor::Host for PluginHostState {
    fn current_window(&mut self) -> u64 {
        0
    }
    fn current_mode(&mut self) -> Mode {
        Mode::Normal
    }
    fn cursor(&mut self, _win: u64) -> Result<u32, HostError> {
        Ok(0)
    }
    fn set_cursor(&mut self, win: u64, pos: u32) -> Result<(), HostError> {
        self.calls.push(PluginApiCall::SetCursor {
            win_id: win,
            pos: pos as usize,
        });
        Ok(())
    }
}

impl bindings::onda::plugin::commands::Host for PluginHostState {
    fn create(&mut self, name: String, id: u64, desc: Option<String>, nargs: u8) {
        self.calls.push(PluginApiCall::CmdCreate {
            name,
            callback_id: id,
            desc,
            nargs,
        });
    }
}

impl bindings::onda::plugin::keymap::Host for PluginHostState {
    fn set(&mut self, mode: String, lhs: String, id: u64, desc: Option<String>) {
        self.calls.push(PluginApiCall::KeymapSet {
            mode,
            lhs,
            callback_id: id,
            desc,
        });
    }
}

impl bindings::onda::plugin::decorations::Host for PluginHostState {
    fn set(&mut self, buf: u64, batch: Batch) -> Result<(), HostError> {
        self.calls.push(PluginApiCall::SetDecorations {
            buf_id: buf,
            batch: batch_from(batch),
        });
        Ok(())
    }
    fn clear(&mut self, buf: u64, namespace: String) {
        self.calls.push(PluginApiCall::ClearDecorations {
            buf_id: buf,
            namespace,
        });
    }
    fn set_group(&mut self, group: String, style: WitStyle) {
        self.calls.push(PluginApiCall::HighlightGroup {
            group,
            style: style_from(style),
        });
    }
}

impl bindings::onda::plugin::ui::Host for PluginHostState {
    fn float(&mut self, title: String, lines: Vec<String>, width: u16, height: u16) {
        self.calls.push(PluginApiCall::UiFloat {
            title,
            lines,
            width,
            height,
        });
    }
    fn pick(&mut self, title: String, items: Vec<bindings::onda::plugin::ui::PickerItem>, id: u64) {
        self.calls.push(PluginApiCall::UiPick {
            title,
            items: items.into_iter().map(|i| (i.label, i.detail)).collect(),
            callback_id: id,
        });
    }
    fn statusline_segment(&mut self, id: String, text: String, style: WitStyle) {
        self.calls.push(PluginApiCall::StatuslineSegment {
            id,
            text,
            style: style_from(style),
        });
    }
}

impl bindings::onda::plugin::config::Host for PluginHostState {
    fn get_string(&mut self, _key: String) -> Option<String> {
        None
    }
    fn get_bool(&mut self, _key: String) -> Option<bool> {
        None
    }
    fn get_int(&mut self, _key: String) -> Option<i64> {
        None
    }
}

impl bindings::onda::plugin::fs::Host for PluginHostState {
    fn read(&mut self, path: String) -> Result<Vec<u8>, HostError> {
        let p = self.resolve_fs(&path)?;
        std::fs::read(&p).map_err(|e| HostError::Rejected(e.to_string()))
    }
    fn write(&mut self, path: String, data: Vec<u8>) -> Result<(), HostError> {
        let p = self.resolve_fs(&path)?;
        std::fs::write(&p, data).map_err(|e| HostError::Rejected(e.to_string()))
    }
    fn read_dir(&mut self, dir: String) -> Result<Vec<String>, HostError> {
        let p = self.resolve_fs(&dir)?;
        let rd = std::fs::read_dir(&p).map_err(|e| HostError::Rejected(e.to_string()))?;
        Ok(rd
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect())
    }
}

impl bindings::onda::plugin::http::Host for PluginHostState {
    fn get(&mut self, url: String) -> Result<bindings::onda::plugin::http::Response, HostError> {
        if !self.caps.network() {
            return Err(HostError::PermissionDenied(url));
        }
        Err(HostError::Rejected("http not implemented in v0".into()))
    }
    fn post(
        &mut self,
        url: String,
        _body: Vec<u8>,
        _content_type: String,
    ) -> Result<bindings::onda::plugin::http::Response, HostError> {
        if !self.caps.network() {
            return Err(HostError::PermissionDenied(url));
        }
        Err(HostError::Rejected("http not implemented in v0".into()))
    }
}
