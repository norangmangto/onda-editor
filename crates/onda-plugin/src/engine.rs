//! WASM Component Model engine + instance lifecycle (W18, T18.1/T18.2/T18.3).
//!
//! - `wasmtime::Engine` with the component model + epoch interruption; a watchdog
//!   thread ticks the epoch so a runaway handler is trapped within the budget.
//! - Per-instance `Store<PluginHostState>` with a memory limit (memory-bomb →
//!   trap) and a snapshot of buffer state for reads.
//! - Capability interfaces (`fs`/`http`) are added to the linker **only when
//!   granted** — an ungranted plugin that imports them fails to instantiate
//!   (link-time enforcement, T17.3).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use wasmtime::component::{Component, HasSelf, Linker};
use wasmtime::{Config, Engine, Store, StoreLimitsBuilder};

use crate::api::PluginApiCall;
use crate::host::bindings::exports::onda::plugin::guest::{BufferEvent, CursorEvent, Event};
use crate::host::bindings::onda::plugin as wit;
use crate::host::bindings::Plugin as PluginBindings;
use crate::host::{BufferSnapshot, PluginHostState};
use crate::permission::GrantedCaps;

/// Default per-call time budget for a synchronous plugin handler (T18.2).
pub const HANDLER_BUDGET_US: u64 = 5_000;

/// The watchdog ticks the epoch every millisecond; the deadline is in ticks.
const EPOCH_TICK: Duration = Duration::from_millis(1);
const BUDGET_TICKS: u64 = HANDLER_BUDGET_US / 1_000;

/// Default per-plugin linear-memory cap (T18.1). Generous for decoration work,
/// small enough that a memory-bomb plugin trips it quickly.
const DEFAULT_MEMORY_LIMIT: usize = 64 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("wasmtime: {0}")]
    Wasm(#[from] wasmtime::Error),
}

/// A shared WASM engine + epoch watchdog. One per editor session.
pub struct PluginEngine {
    engine: Engine,
    stop: Arc<AtomicBool>,
    watchdog: Option<std::thread::JoinHandle<()>>,
}

impl PluginEngine {
    pub fn new() -> Result<Self, EngineError> {
        let mut config = Config::new();
        config.wasm_component_model(true);
        config.epoch_interruption(true);
        let engine = Engine::new(&config)?;

        // Watchdog: tick the epoch so handlers that exceed their budget trap.
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let engine2 = engine.clone();
        let watchdog = std::thread::Builder::new()
            .name("onda-plugin-epoch".into())
            .spawn(move || {
                while !stop2.load(Ordering::Relaxed) {
                    std::thread::sleep(EPOCH_TICK);
                    engine2.increment_epoch();
                }
            })
            .ok();

        Ok(Self {
            engine,
            stop,
            watchdog,
        })
    }

    /// Instantiate a plugin component, wiring only the granted capabilities, and
    /// run its `init` export under the handler budget.
    pub fn instantiate(
        &self,
        wasm: &[u8],
        caps: GrantedCaps,
        project_root: PathBuf,
        snapshots: Vec<(u64, BufferSnapshot)>,
    ) -> Result<PluginInstance, EngineError> {
        let component = Component::from_binary(&self.engine, wasm)?;

        let limits = StoreLimitsBuilder::new()
            .memory_size(DEFAULT_MEMORY_LIMIT)
            .build();
        let mut state = PluginHostState::new(caps.clone(), project_root, limits);
        for (id, snap) in snapshots {
            state.set_buffer_snapshot(id, snap);
        }

        let mut store = Store::new(&self.engine, state);
        store.limiter(|s| &mut s.limits);

        let mut linker = Linker::<PluginHostState>::new(&self.engine);
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker)?;

        // Core (always-available) host interfaces.
        wit::log::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;
        wit::buffer::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;
        wit::selection::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;
        wit::editor::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;
        wit::commands::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;
        wit::keymap::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;
        wit::decorations::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;
        wit::ui::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;
        wit::config::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;

        // Capability interfaces — linked only when granted (link-time enforcement).
        if !caps.fs_roots().is_empty() {
            wit::fs::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;
        }
        if caps.network() {
            wit::http::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;
        }

        store.set_epoch_deadline(BUDGET_TICKS);
        let plugin = PluginBindings::instantiate(&mut store, &component, &linker)?;

        let mut inst = PluginInstance { store, plugin };
        inst.call_init()?;
        Ok(inst)
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }
}

impl Drop for PluginEngine {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.watchdog.take() {
            let _ = h.join();
        }
    }
}

/// A live plugin instance. Each guest call runs under a fresh epoch budget.
pub struct PluginInstance {
    store: Store<PluginHostState>,
    plugin: PluginBindings,
}

impl PluginInstance {
    fn arm_budget(&mut self) {
        self.store.set_epoch_deadline(BUDGET_TICKS);
    }

    fn call_init(&mut self) -> Result<(), EngineError> {
        self.arm_budget();
        self.plugin.onda_plugin_guest().call_init(&mut self.store)?;
        Ok(())
    }

    /// Refresh a buffer snapshot before firing events that read it.
    pub fn set_buffer_snapshot(&mut self, buf_id: u64, snap: BufferSnapshot) {
        self.store.data_mut().set_buffer_snapshot(buf_id, snap);
    }

    pub fn fire_buffer_open(&mut self, buf: u64, path: &str) -> Result<(), EngineError> {
        self.fire_event(Event::BufferOpen(BufferEvent {
            buf,
            path: path.to_string(),
        }))
    }

    pub fn fire_buffer_change(&mut self, buf: u64, path: &str) -> Result<(), EngineError> {
        self.fire_event(Event::BufferChange(BufferEvent {
            buf,
            path: path.to_string(),
        }))
    }

    pub fn fire_cursor_hold(&mut self, buf: u64, pos: u32) -> Result<(), EngineError> {
        self.fire_event(Event::CursorHold(CursorEvent { buf, pos }))
    }

    pub fn fire_event(&mut self, ev: Event) -> Result<(), EngineError> {
        self.arm_budget();
        self.plugin
            .onda_plugin_guest()
            .call_handle_event(&mut self.store, &ev)?;
        Ok(())
    }

    pub fn run_command(&mut self, id: u64, args: Vec<String>) -> Result<(), EngineError> {
        self.arm_budget();
        self.plugin
            .onda_plugin_guest()
            .call_run_command(&mut self.store, id, &args)?;
        Ok(())
    }

    /// Drain the effectful calls the plugin made since the last drain.
    pub fn drain_calls(&mut self) -> Vec<PluginApiCall> {
        self.store.data_mut().take_calls()
    }
}
