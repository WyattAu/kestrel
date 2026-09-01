#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, missing_docs))]

//! `kestrel-plugin` — plugin system for Kestrel.
//!
//! Defines the plugin manifest, capability model, and host API that plugins
//! call into. Plugins are sandboxed via WASM (ADR 0014); this crate provides
//! the type-level contracts and manifest parsing before the `wasmtime`
//! runtime integration lands.
//!
//! # Security
//!
//! - Plugins can never access credentials, local files, or the network.
//! - Each host API call is gated by a declared [`Capability`]; the host
//!   enforces this at the FFI boundary.
//! - Plugin manifests are validated at load time; malformed manifests are
//!   rejected with typed errors.
//!
//! # JSON-over-linear-memory protocol
//!
//! Host ↔ plugin data exchange uses JSON payloads written to WASM linear
//! memory. See the [`runtime`] module-level docs for the full protocol.

pub mod error;
pub mod host;
pub mod manifest;
pub mod runtime;
pub mod types;

pub use error::PluginError;
pub use host::PluginHost;
pub use manifest::{Manifest, parse_manifest};
pub use runtime::{
    PluginExecutor, PluginModule, RuntimeConfig, deserialize_json, read_from_plugin_memory,
    serialize_json, write_to_plugin_memory,
};
pub use types::{Capability, PLUGIN_API_VERSION, PluginInfo, PluginManifest};
