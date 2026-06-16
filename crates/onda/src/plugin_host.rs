//! Binary-side WASM plugin host (replaces the old `onda-lua` integration).
//!
//! Wraps `onda_plugin::PluginEngine` + the installed plugins. Plugins are
//! instantiated at startup (their `init` registers commands), then driven by
//! editor events (buffer-open/change, cursor-hold) and `:`-commands. Effectful
//! calls come back as `PluginApiCall`s, which the editor applies between frames
//! (rule 2). The host owns the command registry so `:name` can dispatch.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use onda_plugin::{
    BufferSnapshot, GrantedCaps, Manifest, PluginApiCall, PluginEngine, PluginInstance,
    PluginManager,
};

/// Editor → plugin events (mirrors the subset of `guest.event` the editor fires).
pub enum PluginEvent {
    BufferOpen { buf: u64, path: String },
    BufferChange { buf: u64, path: String },
    CursorHold { buf: u64, pos: u32 },
}

impl PluginEvent {
    fn kind(&self) -> &'static str {
        match self {
            PluginEvent::BufferOpen { .. } => "buffer-open",
            PluginEvent::BufferChange { .. } => "buffer-change",
            PluginEvent::CursorHold { .. } => "cursor-hold",
        }
    }
    fn buf(&self) -> u64 {
        match self {
            PluginEvent::BufferOpen { buf, .. }
            | PluginEvent::BufferChange { buf, .. }
            | PluginEvent::CursorHold { buf, .. } => *buf,
        }
    }
}

struct Loaded {
    name: String,
    activation: Vec<String>,
    instance: PluginInstance,
    /// Set true once a handler traps (epoch budget / memory) — disabled for the session.
    disabled: bool,
}

/// The set of installed plugins, instantiated and ready.
pub struct PluginHost {
    // Engine must outlive instances (its watchdog ticks the epoch budget).
    _engine: PluginEngine,
    plugins: Vec<Loaded>,
    /// `:command` name → (plugin index, callback id), discovered from `init`.
    commands: HashMap<String, (usize, u64)>,
}

/// Bits reserved for the plugin index in a packed callback handle. A callback id
/// is small (per-plugin counter), so the high bits carry the owning plugin's
/// index — this lets `KeymapSet`/`UiPick` callbacks flow through the flat
/// `PluginApiCall` stream to the editor and back without losing attribution.
const HANDLE_PLUGIN_SHIFT: u64 = 40;

/// Pack a (plugin index, callback id) into a single opaque handle.
pub fn pack_handle(plugin_idx: usize, callback_id: u64) -> u64 {
    ((plugin_idx as u64) << HANDLE_PLUGIN_SHIFT) | (callback_id & ((1 << HANDLE_PLUGIN_SHIFT) - 1))
}

/// Inverse of [`pack_handle`].
pub fn unpack_handle(handle: u64) -> (usize, u64) {
    (
        (handle >> HANDLE_PLUGIN_SHIFT) as usize,
        handle & ((1 << HANDLE_PLUGIN_SHIFT) - 1),
    )
}

impl PluginHost {
    /// Discover + instantiate every installed plugin under the store dir.
    /// Returns the host and any startup calls emitted during `init`.
    pub fn discover(project_root: &Path) -> (Option<PluginHost>, Vec<PluginApiCall>) {
        let engine = match PluginEngine::new() {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("plugin engine init failed: {e}");
                return (None, Vec::new());
            }
        };

        let mut plugins = Vec::new();
        let mut commands = HashMap::new();
        let mut startup = Vec::new();

        let entries = match PluginManager::new(plugins_dir()) {
            Ok(m) => m.list().unwrap_or_default().into_iter().map(move |e| {
                let dir = m.plugin_dir(&e.name);
                (e.name, dir)
            }),
            Err(_) => {
                return (
                    Some(PluginHost {
                        _engine: engine,
                        plugins,
                        commands,
                    }),
                    startup,
                )
            }
        };

        for (name, dir) in entries {
            match load_one(&engine, &name, &dir, project_root) {
                Ok((inst, init_calls, activation)) => {
                    let idx = plugins.len();
                    startup.extend(route_calls(idx, init_calls, &mut commands));
                    plugins.push(Loaded {
                        name,
                        activation,
                        instance: inst,
                        disabled: false,
                    });
                }
                Err(e) => tracing::warn!("plugin '{name}' failed to load: {e}"),
            }
        }

        (
            Some(PluginHost {
                _engine: engine,
                plugins,
                commands,
            }),
            startup,
        )
    }

    /// Fire an event to every plugin subscribed to it; collect their calls.
    pub fn fire(&mut self, ev: PluginEvent, snap: BufferSnapshot) -> Vec<PluginApiCall> {
        let kind = ev.kind();
        let buf = ev.buf();
        // Drain per-plugin first (knowing the index), then route (which needs
        // `&mut self.commands`) once the per-plugin borrow is released.
        let mut drained: Vec<(usize, Vec<PluginApiCall>)> = Vec::new();
        for (idx, p) in self.plugins.iter_mut().enumerate() {
            if p.disabled || !p.activation.iter().any(|e| e == kind) {
                continue;
            }
            p.instance.set_buffer_snapshot(buf, snap.clone());
            let r = match &ev {
                PluginEvent::BufferOpen { buf, path } => p.instance.fire_buffer_open(*buf, path),
                PluginEvent::BufferChange { buf, path } => {
                    p.instance.fire_buffer_change(*buf, path)
                }
                PluginEvent::CursorHold { buf, pos } => p.instance.fire_cursor_hold(*buf, *pos),
            };
            match r {
                Ok(()) => drained.push((idx, p.instance.drain_calls())),
                Err(e) => {
                    tracing::warn!("plugin '{}' trapped, disabling: {e}", p.name);
                    p.disabled = true;
                }
            }
        }
        let mut out = Vec::new();
        for (idx, calls) in drained {
            out.extend(route_calls(idx, calls, &mut self.commands));
        }
        out
    }

    /// Dispatch a `:command`. Returns None if no plugin owns that name.
    pub fn run_command(
        &mut self,
        name: &str,
        args: Vec<String>,
        buf: u64,
        snap: BufferSnapshot,
    ) -> Option<Vec<PluginApiCall>> {
        let (idx, id) = *self.commands.get(name)?;
        let p = self.plugins.get_mut(idx)?;
        if p.disabled {
            return None;
        }
        p.instance.set_buffer_snapshot(buf, snap);
        match p.instance.run_command(id, args) {
            Ok(()) => {
                let calls = p.instance.drain_calls();
                Some(route_calls(idx, calls, &mut self.commands))
            }
            Err(e) => {
                tracing::warn!("plugin '{}' command trapped, disabling: {e}", p.name);
                p.disabled = true;
                None
            }
        }
    }

    /// Invoke a plugin callback by its packed handle (from a `KeymapSet`/`UiPick`
    /// the editor stored). `args` carries e.g. the picked item's value. Returns the
    /// callback's own effectful calls, routed (so a callback can register more).
    pub fn run_callback(
        &mut self,
        handle: u64,
        args: Vec<String>,
        buf: u64,
        snap: BufferSnapshot,
    ) -> Option<Vec<PluginApiCall>> {
        let (idx, id) = unpack_handle(handle);
        let p = self.plugins.get_mut(idx)?;
        if p.disabled {
            return None;
        }
        p.instance.set_buffer_snapshot(buf, snap);
        match p.instance.run_command(id, args) {
            Ok(()) => {
                let calls = p.instance.drain_calls();
                Some(route_calls(idx, calls, &mut self.commands))
            }
            Err(e) => {
                tracing::warn!("plugin '{}' callback trapped, disabling: {e}", p.name);
                p.disabled = true;
                None
            }
        }
    }

    pub fn command_names(&self) -> Vec<String> {
        self.commands.keys().cloned().collect()
    }
}

/// Route a plugin's drained calls: intercept `CmdCreate` into the command
/// registry (attributed to `idx`), rewrite `KeymapSet`/`UiPick` callback ids into
/// packed handles (so attribution survives the flat stream to the editor), and
/// pass everything else through unchanged.
fn route_calls(
    idx: usize,
    calls: Vec<PluginApiCall>,
    commands: &mut HashMap<String, (usize, u64)>,
) -> Vec<PluginApiCall> {
    let mut out = Vec::with_capacity(calls.len());
    for c in calls {
        match c {
            PluginApiCall::CmdCreate {
                name, callback_id, ..
            } => {
                commands.insert(name, (idx, callback_id));
            }
            PluginApiCall::KeymapSet {
                mode,
                lhs,
                callback_id,
                desc,
            } => out.push(PluginApiCall::KeymapSet {
                mode,
                lhs,
                callback_id: pack_handle(idx, callback_id),
                desc,
            }),
            PluginApiCall::UiPick {
                title,
                items,
                callback_id,
            } => out.push(PluginApiCall::UiPick {
                title,
                items,
                callback_id: pack_handle(idx, callback_id),
            }),
            other => out.push(other),
        }
    }
    out
}

fn load_one(
    engine: &PluginEngine,
    name: &str,
    dir: &Path,
    project_root: &Path,
) -> Result<(PluginInstance, Vec<PluginApiCall>, Vec<String>), String> {
    let manifest_src =
        std::fs::read_to_string(dir.join("onda-plugin.toml")).map_err(|e| e.to_string())?;
    let manifest = Manifest::parse(&manifest_src).map_err(|e| e.to_string())?;
    let wasm = std::fs::read(dir.join(&manifest.plugin.entry)).map_err(|e| e.to_string())?;

    // Auto-grant the declared capabilities. (The interactive approval UI is a
    // follow-up; fs is still scoped to the manifest whitelist and `..`-rejected,
    // and ungranted-but-imported capabilities still fail to link.)
    let caps = GrantedCaps::resolve(&manifest.permissions, project_root, |_| true);

    let mut inst = engine
        .instantiate(&wasm, caps, project_root.to_path_buf(), Vec::new())
        .map_err(|e| e.to_string())?;
    let init_calls = inst.drain_calls();
    tracing::debug!("loaded plugin '{name}'");
    Ok((inst, init_calls, manifest.activation.events))
}

/// The plugin store directory: `~/.config/onda/plugins`.
pub fn plugins_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join(".config/onda/plugins")
}

/// `onda plugin <install|list|remove> …` CLI. Returns a process exit code.
pub fn cli(args: &[String]) -> i32 {
    let mgr = match PluginManager::new(plugins_dir()) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("onda plugin: {e}");
            return 1;
        }
    };
    match args.first().map(|s| s.as_str()) {
        Some("install") => match args.get(1) {
            Some(spec) => match mgr.install(spec) {
                Ok(entry) => {
                    println!("installed {} {}", entry.name, entry.version);
                    0
                }
                Err(e) => {
                    eprintln!("install failed: {e}");
                    1
                }
            },
            None => {
                eprintln!("usage: onda plugin install <github:user/repo | url | path>");
                1
            }
        },
        Some("list") => match mgr.list() {
            Ok(list) if list.is_empty() => {
                println!("no plugins installed");
                0
            }
            Ok(list) => {
                for e in list {
                    println!("{:<24} {:<10} {}", e.name, e.version, e.source);
                }
                0
            }
            Err(e) => {
                eprintln!("{e}");
                1
            }
        },
        Some("remove") => match args.get(1) {
            Some(name) => match mgr.remove(name) {
                Ok(()) => {
                    println!("removed {name}");
                    0
                }
                Err(e) => {
                    eprintln!("remove failed: {e}");
                    1
                }
            },
            None => {
                eprintln!("usage: onda plugin remove <name>");
                1
            }
        },
        _ => {
            eprintln!("usage: onda plugin <install|list|remove> …");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use onda_plugin::PluginApiCall;

    #[test]
    fn handle_pack_unpack_roundtrips() {
        for (idx, id) in [(0usize, 0u64), (3, 7), (255, 1_000_000)] {
            let h = pack_handle(idx, id);
            assert_eq!(unpack_handle(h), (idx, id));
        }
    }

    #[test]
    fn route_intercepts_cmd_and_packs_callbacks() {
        let mut commands = HashMap::new();
        let calls = vec![
            PluginApiCall::CmdCreate {
                name: "Hello".into(),
                callback_id: 1,
                desc: None,
                nargs: 0,
            },
            PluginApiCall::KeymapSet {
                mode: "normal".into(),
                lhs: "<C-h>".into(),
                callback_id: 2,
                desc: None,
            },
            PluginApiCall::UiPick {
                title: "pick".into(),
                items: vec![("a".into(), None)],
                callback_id: 3,
            },
            PluginApiCall::Notify {
                msg: "hi".into(),
                level: onda_plugin::NotifyLevel::Info,
            },
        ];
        let out = route_calls(5, calls, &mut commands);
        // CmdCreate consumed into the registry.
        assert_eq!(commands.get("Hello"), Some(&(5usize, 1u64)));
        // KeymapSet + UiPick rewritten with packed handles; Notify passes through.
        assert_eq!(out.len(), 3);
        match &out[0] {
            PluginApiCall::KeymapSet { callback_id, .. } => {
                assert_eq!(unpack_handle(*callback_id), (5, 2));
            }
            other => panic!("expected KeymapSet, got {other:?}"),
        }
        match &out[1] {
            PluginApiCall::UiPick { callback_id, .. } => {
                assert_eq!(unpack_handle(*callback_id), (5, 3));
            }
            other => panic!("expected UiPick, got {other:?}"),
        }
        assert!(matches!(out[2], PluginApiCall::Notify { .. }));
    }
}
