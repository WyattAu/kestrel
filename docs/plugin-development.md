# Plugin Development Guide

How to write, build, and install plugins for Kestrel.

## Architecture overview

```
┌─────────────────────────────────────────────────────────┐
│                     Kestrel Host                        │
│  ┌───────────────────┐    ┌──────────────────────────┐  │
│  │  Engine / Sync    │◄───│  PluginHost (trait)      │  │
│  │                   │    │  - list_accounts()       │  │
│  │                   │    │  - list_folders()        │  │
│  │                   │    │  - list_messages()       │  │
│  │                   │    │  - get_message()         │  │
│  │                   │    │  - subscribe_events()    │  │
│  └───────────────────┘    └──────────┬───────────────┘  │
│                                      │ Capability gate  │
│                                      ▼                  │
│  ┌───────────────────────────────────────────────────┐  │
│  │              wasmtime WASM sandbox                │  │
│  │  ┌─────────────────────────────────────────────┐  │  │
│  │  │            Plugin module (.wasm)            │  │  │
│  │  │  - declare capabilities in manifest        │  │  │
│  │  │  - call host functions via FFI             │  │  │
│  │  │  - 64 MB memory limit                      │  │  │
│  │  │  - 1 s execution timeout per call          │  │  │
│  │  └─────────────────────────────────────────────┘  │  │
│  └───────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────┘
```

Plugins are sandboxed via WebAssembly (WASM) using `wasmtime`. The host
exposes a read-only API; plugins never access credentials, local files,
or the network directly. Each host API call is gated by a declared
capability that the user grants at install time.

See [ADR 0014](adr/0014-plugin-system-wasm.md) for the full rationale.

## Manifest format

Every plugin ships a `manifest.json` (or `manifest.toml`) describing its
identity, version, and required capabilities.

```json
{
    "name": "com.example.spam-filter",
    "version": "1.0.0",
    "author": "Your Name",
    "description": "Filters spam messages",
    "capabilities": ["read_messages", "read_message_bodies"],
    "api_version": "1.0"
}
```

### Fields

| Field           | Type       | Required | Description |
|-----------------|------------|----------|-------------|
| `name`          | `string`   | yes      | Unique plugin identifier (reverse-domain recommended). |
| `version`       | `string`   | yes      | SemVer version string. |
| `author`        | `string`   | yes      | Display name or organization. |
| `description`   | `string`   | yes      | Short human-readable description. |
| `capabilities`  | `string[]` | yes      | Capabilities the plugin requires (see below). |
| `api_version`   | `string`   | yes      | Plugin API version (must match host major version). |

### Validation rules

- `name` and `version` must not be empty.
- `capabilities` must contain at least one entry.
- `api_version` major number must match the host's `PLUGIN_API_VERSION`
  (currently `"1.0"`). Minor version mismatches are allowed.
- Unknown fields are silently ignored (forward compatibility).

## JSON-over-linear-memory protocol

Plugins and the host exchange data through JSON payloads transferred via
WASM linear memory. This is the only data channel — plugins have no access
to the filesystem, network, or any other IPC mechanism.

### How it works

```
Host                                Plugin
  │                                    │
  │  1. Serialize request as JSON      │
  │  2. Write JSON to plugin memory    │
  │     (via host_alloc)               │
  │  3. Call plugin function with      │
  │     (ptr, len) arguments           │
  │──────────────────────────────────►│
  │                                    │ 4. Read JSON from memory
  │                                    │ 5. Process request
  │                                    │ 6. Write JSON response
  │                                    │    (via host_alloc)
  │◄──────────────────────────────────│ 7. Return (ptr, len)
  │  8. Read response from plugin mem  │
  │  9. Deserialize JSON               │
```

### Host-provided imports

The host registers these functions in the `"host"` Wasmtime namespace:

| Function | Signature | Description |
|----------|-----------|-------------|
| `host_log` | `(level: i32, ptr: i32, len: i32)` | Write a log message. `level`: 0=debug, 1=info, 2=warn, 3=error. |
| `host_alloc` | `(len: i32) -> i32` | Allocate `len` bytes in plugin memory. Returns pointer. |
| `host_dealloc` | `(ptr: i32, len: i32)` | Free previously allocated memory. |

`host_alloc` and `host_dealloc` delegate to the plugin's exported
`kesten_alloc` and `kesten_dealloc` functions (see below).

### Plugin exports

Plugins may export these functions (all optional):

| Export | Signature | Description |
|--------|-----------|-------------|
| `plugin_init` | `() -> ()` | One-time initialization after load. |
| `plugin_shutdown` | `() -> ()` | Graceful teardown before unload. |
| `plugin_handle_event` | `(event_type: i32, event_ptr: i32, event_len: i32) -> ()` | Process an event from the host. |
| `kesten_alloc` | `(len: i32) -> i32` | Allocate memory (called by `host_alloc`). |
| `kesten_dealloc` | `(ptr: i32, len: i32) -> ()` | Free memory (called by `host_dealloc`). |

### Example: reading a request

```rust
// Host side (Rust)
let request = serde_json::json!({"action": "list_accounts"});
let json = serde_json::to_vec(&request)?;
// Write to plugin memory and call plugin_handle_event
```

```rust
// Plugin side (Rust)
extern "C" { fn host_log(level: i32, ptr: *const u8, len: usize); }

#[no_mangle]
pub extern "C" fn plugin_handle_event(event_type: u32, event_ptr: u32, event_len: u32) {
    // Read event JSON from memory at (event_ptr, event_len)
    // Process and optionally write response back
}
```

### Memory safety

- The host validates all pointer/length arguments before calling plugin
  functions. Out-of-bounds accesses are rejected at the host boundary.
- Plugins must not cache pointers across calls — the host may reallocate
  memory between invocations.
- All data exchange is serialized JSON; no raw struct pointers cross the
  WASM boundary.

## Capability model

Capabilities are the security boundary between plugins and the host.
Each capability maps to a method on the `PluginHost` trait:

| Capability            | Host method         | Description |
|-----------------------|---------------------|-------------|
| `read_accounts`       | `list_accounts()`   | List accounts and their connection state. |
| `read_folders`        | `list_folders()`    | List folders within an account. |
| `read_messages`       | `list_messages()`   | List message summaries in a folder. |
| `read_message_bodies` | `get_message()`     | Read full message bodies (text, HTML, attachments). |
| `subscribe_events`    | `subscribe_events()`| Subscribe to engine events (new mail, flag changes, etc.). |
| `register_ui`         | *(not yet implemented)* | Register UI components (sidebar panels, toolbar buttons). |

Plugins declare which capabilities they need in their manifest. The host
enforces that every required capability is granted before the plugin can
call the corresponding API method. A plugin that tries to call a method
without the matching capability receives a `CapabilityDenied` error.

### Principle of least privilege

Request only the capabilities your plugin actually needs. A spam filter
that only reads message subjects needs `read_messages` — it does not need
`read_message_bodies` or `read_accounts`.

## Host API surface

The host API is defined by the `PluginHost` trait in
`crates/kestrel-plugin/src/host.rs`. All methods are read-only and
infallible at the trait level (errors return empty/default results so
plugins cannot probe internal error details).

```rust
pub trait PluginHost {
    fn list_accounts(&self) -> Vec<AccountSummary>;
    fn list_folders(&self, account: AccountId) -> Vec<FolderSummary>;
    fn list_messages(&self, folder: FolderId, window: Window) -> Vec<MessageSummary>;
    fn get_message(&self, message: MessageId) -> Option<MessageView>;
    fn subscribe_events(&self) -> tokio::sync::mpsc::Receiver<EngineEvent>;
}
```

Plugins call these functions through the WASM FFI boundary. The host
validates the required capability on every call and returns an empty
result if the capability is not granted.

## Building plugins

### Prerequisites

- Rust toolchain with `wasm32-wasi` target
- `wasmtime` (for local testing, optional)

```bash
rustup target add wasm32-wasi
```

### Compiling to WASM

```bash
cargo build --target wasm32-wasi --release
```

This produces a `.wasm` file in `target/wasm32-wasi/release/`.

### Validating the WASM binary

The host checks for the WASM magic number (`\0asm`) at load time. You can
verify your binary:

```bash
xxd your_plugin.wasm | head -1
# Should start with: 0061 736d 0100 0000
```

### Local testing

```bash
cargo build --target wasm32-wasi --release
# Copy the .wasm and manifest.json to your plugin directory
cp target/wasm32-wasi/release/your_plugin.wasm \
   ~/.config/kestrel/plugins/your-plugin/
cp manifest.json ~/.config/kestrel/plugins/your-plugin/
```

Restart Kestrel or reload plugins from the settings panel.

## Example plugin walkthrough

See `examples/plugins/hello-world/` for a complete sample plugin with
manifest, README, and project structure. The hello-world plugin demonstrates:

- Declaring capabilities (`read_accounts`, `read_folders`, `subscribe_events`)
- Importing host functions (`host_log`)
- Exporting plugin functions (`plugin_init`, `plugin_handle_event`, `plugin_shutdown`)
- Following the recommended project layout

### Project structure

```
hello-world/
├── manifest.json   # Plugin manifest (name, version, capabilities)
├── Cargo.toml      # crate-type = ["cdylib"] for WASM output
├── README.md       # Documentation
└── src/
    └── lib.rs      # Plugin implementation (WASM entry points)
```

## Error handling

Plugin errors follow the [error taxonomy](error-taxonomy.md) and are
classified as recoverable or non-recoverable:

| Error                      | Recoverable | Description |
|----------------------------|-------------|-------------|
| `InvalidManifest`          | yes         | Manifest JSON is malformed. |
| `UnsupportedApiVersion`    | yes         | API version mismatch. |
| `CapabilityDenied`         | yes         | Missing required capability. |
| `ModuleLoad`               | yes         | WASM module failed to load. |
| `InvalidWasm`              | yes         | WASM binary is invalid. |
| `PluginNotFound`           | yes         | Plugin index out of range. |
| `Runtime`                  | no          | Plugin runtime error (crashes plugin). |

Recoverable errors disable the plugin without affecting the engine.
Non-recoverable errors may require a restart.

## Security model

### What plugins can do

- Call host API methods that match their declared capabilities.
- Execute WASM code within the sandbox.
- Exchange JSON data with the host via linear memory.

### What plugins cannot do

- Access credentials, local files, or the network.
- Execute native code (WASM only).
- Exceed memory limits (64 MB default).
- Exceed execution time limits (1 second per call).
- Exceed API call rate limits (100 calls/second default).
- Escape the WASM sandbox (no `unsafe` on the plugin side).

### Enforcement points

1. **Manifest validation** — malformed manifests are rejected at load time.
2. **Capability gate** — each host API call checks the required capability
   before dispatching.
3. **WASM sandbox** — `wasmtime` isolates plugin memory and execution.
4. **Resource limits** — memory, execution time, and call rate are enforced
   by the runtime configuration.
5. **Memory bounds** — all pointer/length arguments are validated before
   accessing plugin memory.
