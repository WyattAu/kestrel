# Changelog

All notable changes to Kestrel are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versioning follows
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Security (hardening from plugin-runtime audit)

- **kestrel-plugin**: enforce a bounded wasmtime fuel budget per plugin call
  (`RuntimeConfig::fuel_per_call`, default 10,000,000 units). Previously the
  store was granted `u64::MAX` fuel, effectively removing the compute limit;
  exhaustion now aborts the call with a typed `PluginError::FuelExhausted`
  instead of leaking a wasmtime trap.
- **kestrel-plugin**: attach a `wasmtime::ResourceLimiter` to the plugin
  store, capping linear memory at `RuntimeConfig::max_memory` (64 MiB
  default). Growth past the cap is denied and surfaced as
  `PluginError::MemoryLimitExceeded`.
- **kestrel-plugin**: enforce a wall-clock per-call timeout on the async
  execution path (`PluginExecutor::call_plugin_async`), configurable via
  `RuntimeConfig::max_execution_time` (default 5 s; was previously
  documented as 1 s but never enforced). Expiry yields
  `PluginError::ExecutionTimedOut`.

### Changed — BREAKING plugin ABI

- **kestrel-plugin**: fix the plugin allocation export spelling
  `kesten_alloc` → `kestrel_alloc` (and `kesten_dealloc` →
  `kestrel_dealloc`) in the host runtime, test fixtures, and
  `docs/plugin-development.md`. There is no plugin-side SDK in this
  repository, so no in-repo guest code is affected — **but third-party
  plugins compiled against the old misspelled export names will no longer
  resolve `host_alloc`/`host_dealloc` delegation and must rename their
  exports.**

### Build

- Add `strip = true` to `[profile.release]` so shipped binaries exclude
  debug info and symbols. `lto = "thin"` / `codegen-units = 1` are
  unchanged, and the panic strategy stays at the default `unwind`
  (documented in `Cargo.toml`) because the GUI relies on unwinding-based
  panic isolation.
