//! `onda-plugin` — WASM Component Model plugin host (ADR-002, DESIGN §5.5).
//!
//! Replaces `onda-lua`. The plugin runtime is WASM (wasmtime + Component Model),
//! chosen for sandbox safety, multi-language support, and crash isolation —
//! ADR-002 explicitly rejects Lua embedding.
//!
//! Layout (PHASE3 plan):
//! - [`manifest`] — `onda-plugin.toml` schema + API-version gating (T17.3).
//! - [`permission`] — capability/permission model; capability interfaces are
//!   wired only when granted (T17.3 / T18.3).
//! - [`api`] — the host-call queue drained by the main loop (rule 2).
//! - [`engine`] — the wasmtime host (W18, placeholder).
//!
//! The host API surface itself is defined in WIT under `wit/onda/*.wit`.

pub mod api;
pub mod engine;
pub mod host;
pub mod manager;
pub mod manifest;
pub mod permission;

pub use api::{DecorationBatch, Edit, NotifyLevel, PluginApiCall, Style};
pub use engine::{PluginEngine, PluginInstance};
pub use host::BufferSnapshot;
pub use manager::{LockEntry, ManagerError, PluginManager, Source};
pub use manifest::{ApiVersion, BufferAccess, Manifest, ManifestError, HOST_API_VERSION};
pub use permission::{Capability, Decision, GrantedCaps};
