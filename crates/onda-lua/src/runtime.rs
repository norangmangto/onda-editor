/// Lua 5.4 runtime for the plugin system.
///
/// One `LuaRuntime` per session. Plugins run on the main thread between frames,
/// bounded by `LUA_FRAME_BUDGET_US`. All calls back into editor state go
/// through the `LuaApiCall` queue.
use std::sync::mpsc;
use std::time::Instant;

use mlua::{Lua, LuaOptions, StdLib};
use thiserror::Error;
use tracing::warn;

use crate::api::{inject, LuaApiCall};
use crate::sandbox;
use crate::LUA_FRAME_BUDGET_US;

#[derive(Debug, Error)]
pub enum LuaRuntimeError {
    #[error("Lua error: {0}")]
    Lua(#[from] mlua::Error),
    #[error("Runtime not initialized")]
    NotInitialized,
}

// ── LuaRuntime ────────────────────────────────────────────────────────────────

pub struct LuaRuntime {
    lua: Lua,
    /// Queue of API calls to apply on the next main-loop drain.
    call_rx: mpsc::Receiver<LuaApiCall>,
    #[allow(dead_code)]
    call_tx: mpsc::SyncSender<LuaApiCall>,
    /// Registered keybinding callbacks: callback_id → Lua function ref key.
    keybinding_callbacks: Vec<(u64, mlua::RegistryKey)>,
    /// Registered command callbacks: name → Lua function ref key.
    command_callbacks: Vec<(String, mlua::RegistryKey)>,
}

impl LuaRuntime {
    /// Create a new sandboxed Lua runtime.
    pub fn new() -> Result<Self, LuaRuntimeError> {
        // Only load safe stdlib modules
        let safe_libs =
            StdLib::STRING | StdLib::TABLE | StdLib::MATH | StdLib::UTF8 | StdLib::PACKAGE;
        let lua = Lua::new_with(safe_libs, LuaOptions::default())?;

        let (call_tx, call_rx) = mpsc::sync_channel(1024);

        // Apply sandbox restrictions
        sandbox::apply(&lua)?;

        // Inject onda.* API
        inject(&lua, call_tx.clone())?;

        Ok(Self {
            lua,
            call_rx,
            call_tx,
            keybinding_callbacks: Vec::new(),
            command_callbacks: Vec::new(),
        })
    }

    /// Execute a Lua source string (e.g. a plugin file).
    /// Errors are logged; they never panic the editor.
    pub fn exec(&self, source: &str, chunk_name: &str) -> Result<(), LuaRuntimeError> {
        match self.lua.load(source).set_name(chunk_name).exec() {
            Ok(_) => Ok(()),
            Err(e) => {
                warn!("Lua error in '{}': {}", chunk_name, e);
                Err(LuaRuntimeError::Lua(e))
            }
        }
    }

    /// Drain the API call queue, returning all pending calls.
    /// Called by the main loop once per frame.
    pub fn drain_calls(&self) -> Vec<LuaApiCall> {
        let mut calls = Vec::new();
        while let Ok(call) = self.call_rx.try_recv() {
            calls.push(call);
        }
        calls
    }

    /// Fire a Lua keybinding callback by its ID.
    pub fn fire_keybinding(&self, callback_id: u64) {
        if let Some((_, key)) = self
            .keybinding_callbacks
            .iter()
            .find(|(id, _)| *id == callback_id)
        {
            if let Ok(func) = self.lua.registry_value::<mlua::Function>(key) {
                let start = Instant::now();
                if let Err(e) = func.call::<()>(()) {
                    warn!("Lua keybinding callback error: {e}");
                }
                let elapsed_us = start.elapsed().as_micros() as u64;
                if elapsed_us > LUA_FRAME_BUDGET_US {
                    warn!(
                        "Lua keybinding exceeded budget: {}µs > {}µs",
                        elapsed_us, LUA_FRAME_BUDGET_US
                    );
                }
            }
        }
    }

    /// Fire a Lua command callback by name.
    pub fn fire_command(&self, name: &str, args: &[&str]) {
        if let Some((_, key)) = self.command_callbacks.iter().find(|(n, _)| n == name) {
            if let Ok(func) = self.lua.registry_value::<mlua::Function>(key) {
                let args_table = match self.lua.create_table() {
                    Ok(t) => {
                        for (i, a) in args.iter().enumerate() {
                            let _ = t.set(i + 1, *a);
                        }
                        mlua::Value::Table(t)
                    }
                    Err(_) => mlua::Value::Nil,
                };
                if let Err(e) = func.call::<()>(args_table) {
                    warn!("Lua command callback error: {e}");
                }
            }
        }
    }

    pub fn lua(&self) -> &Lua {
        &self.lua
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::LuaApiCall;

    #[test]
    fn new_succeeds() {
        let rt = LuaRuntime::new();
        assert!(
            rt.is_ok(),
            "LuaRuntime::new() should succeed: {:?}",
            rt.err()
        );
    }

    #[test]
    fn notify_enqueues_api_call() {
        let rt = LuaRuntime::new().expect("runtime");
        rt.exec(r#"onda.notify("hello from plugin", "info")"#, "test")
            .expect("exec");
        let calls = rt.drain_calls();
        assert_eq!(calls.len(), 1, "expected exactly one queued call");
        match &calls[0] {
            LuaApiCall::Notify { msg, .. } => {
                assert_eq!(msg, "hello from plugin");
            }
            other => panic!("expected Notify, got {:?}", other),
        }
    }

    #[test]
    fn syntax_error_returns_err_not_panic() {
        let rt = LuaRuntime::new().expect("runtime");
        let result = rt.exec("this is not valid lua @@@@", "bad_plugin");
        assert!(
            result.is_err(),
            "syntax error should return Err, not succeed"
        );
        // Runtime must still be usable after the error
        let calls = rt.drain_calls();
        assert!(calls.is_empty());
    }
}
