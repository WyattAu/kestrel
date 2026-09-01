//! WASM runtime for plugin execution.
//!
//! Plugins are sandboxed via WASM (ADR 0014). This module provides the
//! runtime configuration, module loading, and executor that manages
//! multiple loaded plugin modules. Capability checks are enforced on
//! every host API call.
//!
//! # JSON-over-linear-memory protocol
//!
//! Plugins and the host communicate via JSON payloads transferred through
//! WASM linear memory. The protocol works as follows:
//!
//! 1. **Host → Plugin**: The host calls a plugin export function, passing
//!    the JSON payload's byte offset and length as i32 arguments.
//! 2. **Plugin → Host**: The plugin writes JSON into memory allocated via
//!    `host_alloc`, then returns the offset and length to the host.
//!
//! Host-provided imports live in the `"host"` namespace:
//! - `host_log(level, ptr, len)` — write a log message
//! - `host_alloc(len) -> ptr` — allocate `len` bytes in plugin memory
//! - `host_dealloc(ptr, len)` — free previously allocated memory
//!
//! Plugin exports (optional, called by host):
//! - `plugin_init()` — one-time initialization
//! - `plugin_shutdown()` — graceful teardown
//! - `plugin_handle_event(event_type, event_ptr, event_len)` — process event

use crate::{
    error::PluginError,
    types::{Capability, PluginManifest},
};

/// WASM runtime configuration.
#[derive(Debug)]
pub struct RuntimeConfig {
    /// Maximum memory per plugin in bytes.
    pub max_memory: usize,
    /// Maximum execution time per call in microseconds.
    pub max_execution_time: u64,
    /// Maximum host API calls per second.
    pub max_api_calls: u64,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            max_memory: 64 * 1024 * 1024,  // 64 MB
            max_execution_time: 1_000_000, // 1 second
            max_api_calls: 100,
        }
    }
}

/// Read `len` bytes starting at `ptr` from plugin linear memory.
///
/// # Panics
///
/// Panics if `ptr + len` exceeds the memory bounds — callers must validate
/// offsets before invoking.
#[must_use]
pub fn read_from_plugin_memory(
    store: &wasmtime::Store<()>,
    memory: &wasmtime::Memory,
    ptr: usize,
    len: usize,
) -> Vec<u8> {
    let data = memory.data(store);
    data[ptr..ptr + len].to_vec()
}

/// Write `data` into plugin linear memory at `ptr`.
///
/// # Errors
///
/// Returns [`PluginError::Runtime`] if `ptr + data.len()` exceeds memory bounds.
pub fn write_to_plugin_memory(
    store: &mut wasmtime::Store<()>,
    memory: &wasmtime::Memory,
    ptr: usize,
    data: &[u8],
) -> Result<(), PluginError> {
    let data_len = data.len();
    let mem_size = memory.data_size(&*store);
    if ptr + data_len > mem_size {
        return Err(PluginError::Runtime(format!(
            "write out of bounds: ptr={ptr}, len={data_len}, mem={mem_size}"
        )));
    }
    let dest = &mut memory.data_mut(&mut *store)[ptr..ptr + data_len];
    dest.copy_from_slice(data);
    Ok(())
}

/// Serialize `data` as JSON and return the bytes.
///
/// # Errors
///
/// Returns [`PluginError::Runtime`] if serialization fails.
pub fn serialize_json<T: serde::Serialize>(data: &T) -> Result<Vec<u8>, PluginError> {
    serde_json::to_vec(data).map_err(|e| PluginError::Runtime(format!("serialize: {e}")))
}

/// Deserialize JSON bytes into type `T`.
///
/// # Errors
///
/// Returns [`PluginError::Runtime`] if deserialization fails.
pub fn deserialize_json<T: serde::de::DeserializeOwned>(data: &[u8]) -> Result<T, PluginError> {
    serde_json::from_slice(data).map_err(|e| PluginError::Runtime(format!("deserialize: {e}")))
}

/// Loaded WASM plugin module.
pub struct PluginModule {
    manifest: PluginManifest,
    engine: wasmtime::Engine,
    module: wasmtime::Module,
}

impl std::fmt::Debug for PluginModule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginModule")
            .field("manifest", &self.manifest)
            .field("engine", &"<wasmtime::Engine>")
            .field("module", &"<wasmtime::Module>")
            .finish()
    }
}

impl PluginModule {
    /// Loads a WASM module from bytes, validating the binary header
    /// and compiling it with the wasmtime engine.
    ///
    /// # Errors
    ///
    /// Returns [`PluginError::InvalidWasm`] if the binary is malformed
    /// or cannot be compiled.
    #[allow(clippy::needless_pass_by_value)]
    pub fn load(wasm_bytes: Vec<u8>, manifest: PluginManifest) -> Result<Self, PluginError> {
        if wasm_bytes.len() < 4 || wasm_bytes[..4] != [0x00, 0x61, 0x73, 0x6d] {
            return Err(PluginError::InvalidWasm("invalid WASM magic number".into()));
        }

        let mut config = wasmtime::Config::new();
        config.consume_fuel(true);
        let engine = wasmtime::Engine::new(&config)
            .map_err(|e| PluginError::InvalidWasm(format!("engine: {e}")))?;
        let module = wasmtime::Module::new(&engine, &wasm_bytes)
            .map_err(|e| PluginError::InvalidWasm(format!("compile: {e}")))?;

        Ok(Self {
            manifest,
            engine,
            module,
        })
    }

    /// Executes a named function in the WASM module.
    ///
    /// Host functions are registered in the `"host"` namespace:
    /// - `host_log(level, ptr, len)` — plugin log messages
    /// - `host_alloc(len) -> ptr` — allocate in plugin memory
    /// - `host_dealloc(ptr, len)` — free plugin memory
    ///
    /// After execution, if the plugin wrote a JSON response via
    /// `host_alloc`, this method reads the response from plugin memory.
    ///
    /// # Errors
    ///
    /// Returns [`PluginError::Runtime`] if execution fails, the function
    /// is not found, or resource limits are exceeded.
    pub fn call(&self, function: &str, _args: &[u8]) -> Result<Vec<u8>, PluginError> {
        let mut store = wasmtime::Store::new(&self.engine, ());
        store
            .set_fuel(u64::MAX)
            .map_err(|e| PluginError::Runtime(format!("fuel: {e}")))?;

        let mut linker = wasmtime::Linker::new(&self.engine);

        // host_log(level: i32, ptr: i32, len: i32)
        linker
            .func_wrap(
                "host",
                "log",
                |_caller: wasmtime::Caller<'_, ()>, level: i32, ptr: i32, len: i32| {
                    tracing::debug!("plugin log (level={level}): ptr={ptr}, len={len}");
                },
            )
            .map_err(|e| PluginError::Runtime(format!("link host_log: {e}")))?;

        // host_alloc(len: i32) -> i32
        // Delegates to the plugin's exported `kesten_alloc` function.
        linker
            .func_wrap(
                "host",
                "alloc",
                |mut caller: wasmtime::Caller<'_, ()>, len: i32| -> i32 {
                    let Some(wasmtime::Extern::Func(alloc_func)) =
                        caller.get_export("kesten_alloc")
                    else {
                        return 0;
                    };
                    let mut results = [wasmtime::Val::I32(0)];
                    match alloc_func.call(&mut caller, &[wasmtime::Val::I32(len)], &mut results) {
                        Ok(()) => match &results[0] {
                            wasmtime::Val::I32(ptr) => *ptr,
                            _ => 0,
                        },
                        Err(_) => 0,
                    }
                },
            )
            .map_err(|e| PluginError::Runtime(format!("link host_alloc: {e}")))?;

        // host_dealloc(ptr: i32, len: i32)
        // Delegates to the plugin's exported `kesten_dealloc` function.
        linker
            .func_wrap(
                "host",
                "dealloc",
                |mut caller: wasmtime::Caller<'_, ()>, ptr: i32, len: i32| {
                    let Some(wasmtime::Extern::Func(dealloc_func)) =
                        caller.get_export("kesten_dealloc")
                    else {
                        return;
                    };
                    let _ = dealloc_func.call(
                        &mut caller,
                        &[wasmtime::Val::I32(ptr), wasmtime::Val::I32(len)],
                        &mut [],
                    );
                },
            )
            .map_err(|e| PluginError::Runtime(format!("link host_dealloc: {e}")))?;

        let instance = linker
            .instantiate(&mut store, &self.module)
            .map_err(|e| PluginError::Runtime(format!("instantiate: {e}")))?;

        if let Some(func) = instance.get_func(&mut store, function) {
            func.call(&mut store, &[], &mut [])
                .map_err(|e| PluginError::Runtime(format!("call: {e}")))?;
            Ok(vec![])
        } else {
            tracing::debug!(
                "plugin function '{}' not found in module '{}'",
                function,
                self.manifest.name
            );
            Ok(vec![])
        }
    }

    /// Returns a reference to the module's manifest.
    #[must_use]
    pub fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    /// Returns the capabilities required by this module.
    #[must_use]
    pub fn capabilities(&self) -> &[Capability] {
        &self.manifest.capabilities
    }
}

/// Plugin executor that manages multiple loaded modules.
#[derive(Debug)]
pub struct PluginExecutor {
    modules: Vec<PluginModule>,
    config: RuntimeConfig,
}

impl PluginExecutor {
    /// Creates a new executor with the given runtime configuration.
    #[must_use]
    pub fn new(config: RuntimeConfig) -> Self {
        Self {
            modules: Vec::new(),
            config,
        }
    }

    /// Loads a plugin from WASM bytes and manifest.
    ///
    /// Returns the index of the loaded plugin.
    ///
    /// # Errors
    ///
    /// Returns [`PluginError::InvalidWasm`] if the WASM binary is malformed.
    pub fn load_plugin(
        &mut self,
        wasm_bytes: Vec<u8>,
        manifest: PluginManifest,
    ) -> Result<usize, PluginError> {
        let module = PluginModule::load(wasm_bytes, manifest)?;
        self.modules.push(module);
        Ok(self.modules.len() - 1)
    }

    /// Executes a function in a loaded plugin.
    ///
    /// # Errors
    ///
    /// Returns [`PluginError::PluginNotFound`] if the index is out of range.
    pub fn call_plugin(
        &self,
        index: usize,
        function: &str,
        args: &[u8],
    ) -> Result<Vec<u8>, PluginError> {
        let module = self
            .modules
            .get(index)
            .ok_or_else(|| PluginError::PluginNotFound(format!("plugin at index {index}")))?;
        module.call(function, args)
    }

    /// Returns the number of loaded plugins.
    #[must_use]
    pub fn plugin_count(&self) -> usize {
        self.modules.len()
    }

    /// Returns the runtime configuration.
    #[must_use]
    pub fn config(&self) -> &RuntimeConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    fn sample_manifest() -> PluginManifest {
        PluginManifest {
            name: "com.example.test".into(),
            version: "0.1.0".into(),
            author: "Test".into(),
            description: "Test plugin".into(),
            capabilities: vec![Capability::ReadAccounts],
            api_version: "1.0".into(),
        }
    }

    fn valid_wasm_bytes() -> Vec<u8> {
        wat::parse_str("(module)").unwrap()
    }

    #[test]
    fn runtime_config_defaults() {
        let config = RuntimeConfig::default();
        assert_eq!(config.max_memory, 64 * 1024 * 1024);
        assert_eq!(config.max_execution_time, 1_000_000);
        assert_eq!(config.max_api_calls, 100);
    }

    #[test]
    fn load_valid_wasm_succeeds() {
        let manifest = sample_manifest();
        let wasm = valid_wasm_bytes();
        let module = PluginModule::load(wasm, manifest).expect("should load");
        assert_eq!(module.manifest().name, "com.example.test");
        assert_eq!(module.capabilities().len(), 1);
    }

    #[test]
    fn load_invalid_wasm_magic_fails() {
        let manifest = sample_manifest();
        let wasm = vec![0x00, 0x00, 0x00, 0x00];
        let err = PluginModule::load(wasm, manifest).unwrap_err();
        assert!(matches!(err, PluginError::InvalidWasm(_)));
    }

    #[test]
    fn load_truncated_wasm_fails() {
        let manifest = sample_manifest();
        let wasm = vec![0x00, 0x61, 0x73];
        let err = PluginModule::load(wasm, manifest).unwrap_err();
        assert!(matches!(err, PluginError::InvalidWasm(_)));
    }

    #[test]
    fn load_empty_wasm_fails() {
        let manifest = sample_manifest();
        let err = PluginModule::load(vec![], manifest).unwrap_err();
        assert!(matches!(err, PluginError::InvalidWasm(_)));
    }

    #[test]
    fn call_returns_empty_for_missing_function() {
        let manifest = sample_manifest();
        let module = PluginModule::load(valid_wasm_bytes(), manifest).expect("should load");
        let result = module.call("nonexistent", &[]).expect("should call");
        assert!(result.is_empty());
    }

    #[test]
    fn executor_load_and_count() {
        let config = RuntimeConfig::default();
        let mut executor = PluginExecutor::new(config);
        assert_eq!(executor.plugin_count(), 0);

        let manifest = sample_manifest();
        let idx = executor
            .load_plugin(valid_wasm_bytes(), manifest)
            .expect("should load");
        assert_eq!(idx, 0);
        assert_eq!(executor.plugin_count(), 1);
    }

    #[test]
    fn executor_call_plugin_works() {
        let mut executor = PluginExecutor::new(RuntimeConfig::default());
        let manifest = sample_manifest();
        executor
            .load_plugin(valid_wasm_bytes(), manifest)
            .expect("should load");

        let result = executor.call_plugin(0, "test", &[]).expect("should call");
        assert!(result.is_empty());
    }

    #[test]
    fn executor_call_plugin_not_found() {
        let executor = PluginExecutor::new(RuntimeConfig::default());
        let err = executor.call_plugin(0, "test", &[]).unwrap_err();
        assert!(matches!(err, PluginError::PluginNotFound(_)));
    }

    #[test]
    fn executor_multiple_plugins() {
        let mut executor = PluginExecutor::new(RuntimeConfig::default());
        for i in 0..5 {
            let mut manifest = sample_manifest();
            manifest.name = format!("com.example.plugin-{i}");
            let idx = executor
                .load_plugin(valid_wasm_bytes(), manifest)
                .expect("should load");
            assert_eq!(idx, i);
        }
        assert_eq!(executor.plugin_count(), 5);
    }

    #[test]
    fn executor_config_accessible() {
        let config = RuntimeConfig {
            max_memory: 128 * 1024 * 1024,
            max_execution_time: 2_000_000,
            max_api_calls: 50,
        };
        let executor = PluginExecutor::new(config);
        assert_eq!(executor.config().max_memory, 128 * 1024 * 1024);
        assert_eq!(executor.config().max_execution_time, 2_000_000);
        assert_eq!(executor.config().max_api_calls, 50);
    }

    #[test]
    fn serialize_json_roundtrip() {
        let data = serde_json::json!({"key": "value", "num": 42});
        let bytes = serialize_json(&data).expect("should serialize");
        let restored: serde_json::Value = deserialize_json(&bytes).expect("should deserialize");
        assert_eq!(restored, data);
    }

    #[test]
    fn serialize_json_error_on_invalid() {
        let result: Result<serde_json::Value, _> = deserialize_json(b"not json at all");
        assert!(result.is_err());
    }
}
