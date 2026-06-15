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
                    for c in init_calls {
                        match c {
                            PluginApiCall::CmdCreate {
                                name, callback_id, ..
                            } => {
                                commands.insert(name, (idx, callback_id));
                            }
                            other => startup.push(other),
                        }
                    }
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
        let mut out = Vec::new();
        for p in self.plugins.iter_mut() {
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
                Ok(()) => out.extend(p.instance.drain_calls()),
                Err(e) => {
                    tracing::warn!("plugin '{}' trapped, disabling: {e}", p.name);
                    p.disabled = true;
                }
            }
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
            Ok(()) => Some(p.instance.drain_calls()),
            Err(e) => {
                tracing::warn!("plugin '{}' command trapped, disabling: {e}", p.name);
                p.disabled = true;
                None
            }
        }
    }

    pub fn command_names(&self) -> Vec<String> {
        self.commands.keys().cloned().collect()
    }
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
