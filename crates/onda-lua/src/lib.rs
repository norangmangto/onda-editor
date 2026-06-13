pub mod api;
pub mod loader;
pub mod runtime;
pub mod sandbox;

pub use api::LuaApiCall;
pub use loader::PluginLoader;
pub use runtime::{LuaRuntime, LuaRuntimeError};

/// Per-frame budget for Lua execution (in microseconds).
/// Lua runs between frames only; if a plugin exceeds this budget it is aborted.
pub const LUA_FRAME_BUDGET_US: u64 = 500;
