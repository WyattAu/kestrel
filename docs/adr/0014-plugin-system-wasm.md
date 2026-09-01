# ADR 0014: Plugin System — WASM via wasmtime

- **Status:** Proposed
- **Date:** 2026-08-31
- **Deciders:** Kestrel team

## Context

Kestrel needs an extension mechanism for power users. Plugins must be sandboxed
for security (threat model §4.4 — WASM is memory-safe by construction, no
native code execution). The host API must expose only read/subscribe capabilities;
plugins must never access credentials, local files, or the network directly.

## Decision

Use WebAssembly (WASM) via `wasmtime` runtime for sandboxed plugin execution.
Plugins declare required capabilities in a manifest. User grants capabilities
at install time. The plugin API is stable and versioned independently of the
core protocol version.

## Consequences

- **Memory-safe plugin execution** — WASM modules cannot escape their sandbox
  (no `unsafe` on the plugin side, no native code paths).
- **Capability-based security model** — each API call is gated by a declared
  capability; the host enforces this, plugins cannot escalate.
- **Language-agnostic development** — any language that compiles to WASM (Rust,
  C, Go, Zig, etc.) can write plugins.
- **Dependency cost** — `wasmtime` adds ~15 MB to binary size; deferred until
  plugin types/interfaces are stable.
- **API stability** — plugin API versioned separately from core; breaking
  changes require a new major version + ADR.

## Alternatives Considered

- **WASM via `wasmer`** — lighter than wasmtime but less mature component
  model; wasmtime chosen for WASI support and ongoing WebAssembly workgroup
  alignment.
- **Native dynamic libraries (libloading)** — rejected: no sandbox, no
  capability model, platform-specific, security-critical (threat model §4.4).
- **Scripting languages (Lua/JS)** — embedding a full runtime has similar
  binary-size cost with weaker isolation guarantees.
