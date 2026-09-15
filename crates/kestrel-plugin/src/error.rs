//! Error types for the plugin system (ADR 0007 taxonomy).

/// Errors specific to plugin loading and execution.
#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    /// The manifest JSON is invalid or missing required fields.
    #[error("invalid manifest: {0}")]
    InvalidManifest(String),

    /// The manifest requests an unsupported plugin API version.
    #[error("unsupported API version: declared {declared}, host supports {host}")]
    UnsupportedApiVersion {
        /// Version declared by the plugin.
        declared: String,
        /// Version supported by the host.
        host: String,
    },

    /// The manifest requests a capability not in the host allowlist.
    #[error("capability not available: {0}")]
    CapabilityDenied(Capability),

    /// The WASM module failed to load or instantiate.
    #[error("module load failed: {0}")]
    ModuleLoad(String),

    /// The WASM binary is invalid (bad magic number, truncated, etc.).
    #[error("invalid WASM: {0}")]
    InvalidWasm(String),

    /// No plugin is loaded at the requested index.
    #[error("plugin not found: {0}")]
    PluginNotFound(String),

    /// The plugin exhausted its configured fuel budget within a single call.
    ///
    /// Fuel bounds compute time per invocation (see [`crate::RuntimeConfig::
    /// fuel_per_call`]); the call is aborted and reported here instead of
    /// leaking a wasmtime trap.
    #[error("fuel exhausted: plugin exceeded its per-call fuel budget of {budget}")]
    FuelExhausted {
        /// Configured fuel budget for the call that was aborted.
        budget: u64,
    },

    /// The plugin attempted to grow linear memory past its configured cap.
    ///
    /// The growth is denied and surfaced as this error rather than a generic
    /// wasmtime limit error.
    #[error(
        "memory limit exceeded: {requested} bytes requested exceeds the {limit}-byte plugin memory cap"
    )]
    MemoryLimitExceeded {
        /// Total bytes the plugin attempted to map at the moment of denial.
        requested: usize,
        /// Configured linear-memory cap in bytes.
        limit: usize,
    },

    /// A plugin call exceeded its wall-clock timeout and was aborted.
    ///
    /// Enforced on the async execution path (see [`crate::PluginExecutor::
    /// call_plugin_async`]); the underlying worker may keep burning fuel in
    /// the background until its fuel budget is spent.
    #[error("execution timed out: call exceeded the {timeout_ms} ms plugin timeout")]
    ExecutionTimedOut {
        /// Configured timeout for the call that was aborted.
        timeout_ms: u64,
    },

    /// The plugin runtime encountered an error.
    #[error("runtime error: {0}")]
    Runtime(String),
}

/// Re-export for use in error variants without pulling the whole type.
use crate::types::Capability;

impl PluginError {
    /// Returns `true` if this error is recoverable (plugin can be disabled
    /// without affecting the engine).
    #[must_use]
    pub fn is_recoverable(&self) -> bool {
        matches!(
            self,
            Self::InvalidManifest(_)
                | Self::UnsupportedApiVersion { .. }
                | Self::CapabilityDenied(_)
                | Self::ModuleLoad(_)
                | Self::InvalidWasm(_)
                | Self::PluginNotFound(_)
                | Self::FuelExhausted { .. }
                | Self::MemoryLimitExceeded { .. }
                | Self::ExecutionTimedOut { .. }
        )
    }
}
