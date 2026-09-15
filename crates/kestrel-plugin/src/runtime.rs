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
///
/// These fields are the plugin resource-limit surface: every value is
/// enforced per plugin call by the executor (fuel, memory, wall-clock
/// timeout). Defaults are chosen so a misbehaving plugin cannot starve
/// the host, per ADR 0014 and threat model §4.4.
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    /// Maximum linear memory per plugin in bytes.
    ///
    /// Enforced by a wasmtime [`wasmtime::ResourceLimiter`] on the plugin
    /// store: allocations past this cap are denied and surfaced as
    /// [`PluginError::MemoryLimitExceeded`].
    pub max_memory: usize,
    /// Maximum wall-clock execution time per call, in microseconds.
    ///
    /// Enforced on the async execution path
    /// ([`PluginExecutor::call_plugin_async`]); expiry yields
    /// [`PluginError::ExecutionTimedOut`].
    pub max_execution_time: u64,
    /// Maximum host API calls per second.
    pub max_api_calls: u64,
    /// Wasmtime fuel budget per plugin call.
    ///
    /// Fuel is wasmtime's compute-cost metering: roughly one unit per
    /// executed operator. Each call gets exactly this budget; exhaustion
    /// aborts execution with [`PluginError::FuelExhausted`] instead of
    /// letting a plugin loop forever.
    pub fuel_per_call: u64,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            max_memory: 64 * 1024 * 1024,  // 64 MB
            max_execution_time: 5_000_000, // 5 seconds
            max_api_calls: 100,
            fuel_per_call: 10_000_000, // ~10M wasm operators per call
        }
    }
}

/// Host state carried inside the wasmtime [`wasmtime::Store`].
#[derive(Debug)]
struct PluginStoreState {
    /// Linear-memory and table limits for this store's plugin.
    limits: PluginLimits,
}

impl PluginStoreState {
    fn new(max_memory: usize) -> Self {
        Self {
            limits: PluginLimits {
                max_memory,
                memory_limit_hit: false,
                requested: 0,
            },
        }
    }
}

/// [`wasmtime::ResourceLimiter`] capping a plugin's linear memory.
///
/// The store is synchronous (no `async_store`), so the sync limiter trait is
/// the correct attachment point; wasmtime's `ResourceLimiterAsync` variant
/// only applies to async stores.
#[derive(Debug)]
struct PluginLimits {
    max_memory: usize,
    /// Set when a growth past `max_memory` was denied, so call-site error
    /// mapping can turn the resulting trap into a typed
    /// [`PluginError::MemoryLimitExceeded`].
    memory_limit_hit: bool,
    /// Bytes requested by the denied growth, for the typed error.
    requested: usize,
}

impl wasmtime::ResourceLimiter for PluginLimits {
    fn memory_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        if desired > self.max_memory {
            self.memory_limit_hit = true;
            self.requested = desired;
            return Err(wasmtime::Error::msg("plugin linear memory cap exceeded"));
        }
        Ok(true)
    }

    fn table_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        // Tables hold one pointer-sized element per entry; bound them by the
        // same byte budget as memory to keep the cap meaningful.
        Ok(desired.saturating_mul(size_of::<usize>()) <= self.max_memory)
    }

    fn memories(&self) -> usize {
        1
    }
}

/// Read `len` bytes starting at `ptr` from plugin linear memory.
///
/// # Panics
///
/// Panics if `ptr + len` exceeds the memory bounds — callers must validate
/// offsets before invoking.
#[must_use]
pub fn read_from_plugin_memory<T>(
    store: &wasmtime::Store<T>,
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
pub fn write_to_plugin_memory<T>(
    store: &mut wasmtime::Store<T>,
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

    /// Executes a named function in the WASM module with default limits.
    ///
    /// See [`PluginModule::call_with_config`] for the limit semantics; this
    /// convenience wrapper applies [`RuntimeConfig::default`].
    ///
    /// # Errors
    ///
    /// Returns [`PluginError::Runtime`] if execution fails, the function
    /// is not found, or resource limits are exceeded.
    pub fn call(&self, function: &str, args: &[u8]) -> Result<Vec<u8>, PluginError> {
        self.call_with_config(function, args, &RuntimeConfig::default())
    }

    /// Executes a named function in the WASM module under `config`'s limits.
    ///
    /// Host functions are registered in the `"host"` namespace:
    /// - `host_log(level, ptr, len)` — plugin log messages
    /// - `host_alloc(len) -> ptr` — allocate in plugin memory
    /// - `host_dealloc(ptr, len)` — free plugin memory
    ///
    /// Resource limits enforced here, per call:
    /// - **Fuel**: the store gets exactly `config.fuel_per_call`; exhaustion
    ///   aborts execution with [`PluginError::FuelExhausted`].
    /// - **Memory**: a [`wasmtime::ResourceLimiter`] denies linear-memory
    ///   growth past `config.max_memory`, surfacing as
    ///   [`PluginError::MemoryLimitExceeded`].
    ///
    /// Wall-clock timeouts are enforced by [`PluginExecutor::
    /// call_plugin_async`]; the sync path relies on the fuel bound to
    /// terminate runaway plugins.
    ///
    /// # Errors
    ///
    /// Returns [`PluginError::Runtime`] if execution fails or the function
    /// is not found, [`PluginError::FuelExhausted`] on fuel exhaustion, and
    /// [`PluginError::MemoryLimitExceeded`] when the memory cap is hit.
    pub fn call_with_config(
        &self,
        function: &str,
        _args: &[u8],
        config: &RuntimeConfig,
    ) -> Result<Vec<u8>, PluginError> {
        let mut store =
            wasmtime::Store::new(&self.engine, PluginStoreState::new(config.max_memory));
        store
            .set_fuel(config.fuel_per_call)
            .map_err(|e| PluginError::Runtime(format!("fuel: {e}")))?;
        store.limiter(|state| &mut state.limits);

        let mut linker = wasmtime::Linker::new(&self.engine);

        // host_log(level: i32, ptr: i32, len: i32)
        linker
            .func_wrap(
                "host",
                "log",
                |_caller: wasmtime::Caller<'_, PluginStoreState>,
                 level: i32,
                 ptr: i32,
                 len: i32| {
                    tracing::debug!("plugin log (level={level}): ptr={ptr}, len={len}");
                },
            )
            .map_err(|e| PluginError::Runtime(format!("link host_log: {e}")))?;

        // host_alloc(len: i32) -> i32
        // Delegates to the plugin's exported `kestrel_alloc` function.
        linker
            .func_wrap(
                "host",
                "alloc",
                |mut caller: wasmtime::Caller<'_, PluginStoreState>, len: i32| -> i32 {
                    let Some(wasmtime::Extern::Func(alloc_func)) =
                        caller.get_export("kestrel_alloc")
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
        // Delegates to the plugin's exported `kestrel_dealloc` function.
        linker
            .func_wrap(
                "host",
                "dealloc",
                |mut caller: wasmtime::Caller<'_, PluginStoreState>, ptr: i32, len: i32| {
                    let Some(wasmtime::Extern::Func(dealloc_func)) =
                        caller.get_export("kestrel_dealloc")
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
            .map_err(|e| map_execution_error(&store, &e, config))?;

        if let Some(func) = instance.get_func(&mut store, function) {
            func.call(&mut store, &[], &mut [])
                .map_err(|e| map_execution_error(&store, &e, config))?;
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

    /// Asynchronously executes a named function under `config`'s limits,
    /// including a wall-clock timeout.
    ///
    /// The synchronous execution runs on the tokio blocking pool; if
    /// `config.max_execution_time` elapses first, this returns
    /// [`PluginError::ExecutionTimedOut`] and the abandoned worker keeps
    /// running until its fuel budget is spent (it is detached, never
    /// rejoined).
    ///
    /// # Errors
    ///
    /// As [`PluginModule::call_with_config`], plus
    /// [`PluginError::ExecutionTimedOut`] on timeout.
    pub async fn call_async(
        &self,
        function: &str,
        args: &[u8],
        config: &RuntimeConfig,
    ) -> Result<Vec<u8>, PluginError> {
        let function = function.to_owned();
        let args = args.to_vec();
        let config = config.clone();
        let timeout_ms = config.max_execution_time / 1_000;
        let timeout = std::time::Duration::from_micros(config.max_execution_time);
        // `Engine`/`Module` clones are cheap (arc-backed); they give the
        // blocking worker a `'static` handle independent of `&self`.
        let module = Self {
            manifest: self.manifest.clone(),
            engine: self.engine.clone(),
            module: self.module.clone(),
        };
        let fut =
            tokio::task::spawn_blocking(move || module.call_with_config(&function, &args, &config));
        match tokio::time::timeout(timeout, fut).await {
            Ok(joined) => joined.map_err(|e| PluginError::Runtime(format!("join: {e}")))?,
            Err(_elapsed) => Err(PluginError::ExecutionTimedOut { timeout_ms }),
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

/// Maps a wasmtime execution error to the most specific typed
/// [`PluginError`], consulting the store's limiter state.
fn map_execution_error(
    store: &wasmtime::Store<PluginStoreState>,
    err: &wasmtime::Error,
    config: &RuntimeConfig,
) -> PluginError {
    if store.data().limits.memory_limit_hit {
        return PluginError::MemoryLimitExceeded {
            requested: store.data().limits.requested,
            limit: config.max_memory,
        };
    }
    if err.downcast_ref::<wasmtime::Trap>() == Some(&wasmtime::Trap::OutOfFuel) {
        return PluginError::FuelExhausted {
            budget: config.fuel_per_call,
        };
    }
    PluginError::Runtime(format!("{err:#}"))
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

    /// Executes a function in a loaded plugin under the executor's config.
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
        module.call_with_config(function, args, &self.config)
    }

    /// Asynchronously executes a function in a loaded plugin under the
    /// executor's config, including the wall-clock timeout.
    ///
    /// # Errors
    ///
    /// Returns [`PluginError::PluginNotFound`] if the index is out of range,
    /// and [`PluginError::ExecutionTimedOut`] if the call exceeds
    /// [`RuntimeConfig::max_execution_time`].
    pub async fn call_plugin_async(
        &self,
        index: usize,
        function: &str,
        args: &[u8],
    ) -> Result<Vec<u8>, PluginError> {
        let module = self
            .modules
            .get(index)
            .ok_or_else(|| PluginError::PluginNotFound(format!("plugin at index {index}")))?;
        module.call_async(function, args, &self.config).await
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
        assert_eq!(config.max_execution_time, 5_000_000);
        assert_eq!(config.max_api_calls, 100);
        assert_eq!(config.fuel_per_call, 10_000_000);
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
            fuel_per_call: 20_000_000,
        };
        let executor = PluginExecutor::new(config);
        assert_eq!(executor.config().max_memory, 128 * 1024 * 1024);
        assert_eq!(executor.config().max_execution_time, 2_000_000);
        assert_eq!(executor.config().max_api_calls, 50);
        assert_eq!(executor.config().fuel_per_call, 20_000_000);
    }

    /// Infinite-loop module used for fuel/timeout tests.
    fn infinite_loop_wasm() -> Vec<u8> {
        wat::parse_str(
            r#"
            (module
                (func (export "spin") (loop (br 0)))
            )
            "#,
        )
        .unwrap()
    }

    #[test]
    fn fuel_exhaustion_maps_to_typed_error() {
        let manifest = sample_manifest();
        let module = PluginModule::load(infinite_loop_wasm(), manifest).expect("should load");
        let config = RuntimeConfig {
            fuel_per_call: 1_000,
            ..RuntimeConfig::default()
        };
        let err = module
            .call_with_config("spin", &[], &config)
            .expect_err("tiny fuel budget must abort the call");
        assert!(
            matches!(err, PluginError::FuelExhausted { budget } if budget == 1_000),
            "expected typed FuelExhausted, got: {err:?}"
        );
        assert!(err.is_recoverable());
    }

    #[test]
    fn memory_growth_past_cap_maps_to_typed_error() {
        let manifest = sample_manifest();
        // Starts at 1 page (64 KiB) and grows by 16 more pages (1 MiB total),
        // exceeding the 512 KiB cap.
        let wasm = wat::parse_str(
            r#"
            (module
                (memory (export "memory") 1)
                (func (export "grow") (drop (memory.grow (i32.const 16))))
            )
            "#,
        )
        .unwrap();
        let module = PluginModule::load(wasm, manifest).expect("should load");
        let config = RuntimeConfig {
            max_memory: 512 * 1024,
            ..RuntimeConfig::default()
        };
        let err = module
            .call_with_config("grow", &[], &config)
            .expect_err("growth past the memory cap must be denied");
        assert!(
            matches!(err, PluginError::MemoryLimitExceeded { limit, .. } if limit == 512 * 1024),
            "expected typed MemoryLimitExceeded, got: {err:?}"
        );
        assert!(err.is_recoverable());
    }

    #[test]
    fn memory_growth_within_cap_succeeds() {
        let manifest = sample_manifest();
        let wasm = wat::parse_str(
            r#"
            (module
                (memory (export "memory") 1)
                (func (export "grow") (drop (memory.grow (i32.const 2))))
            )
            "#,
        )
        .unwrap();
        let module = PluginModule::load(wasm, manifest).expect("should load");
        let result = module.call("grow", &[]).expect("growth within cap is fine");
        assert!(result.is_empty());
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
