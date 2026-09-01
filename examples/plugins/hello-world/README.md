# Hello World Plugin

A sample Kestrel plugin that demonstrates the plugin API. This plugin lists
accounts and folders, then subscribes to engine events.

## What it does

- Queries the host for configured accounts via `list_accounts`
- Lists folders for each account via `list_folders`
- Subscribes to engine events (new mail, flag changes, etc.) via `subscribe_events`

## Capabilities requested

| Capability           | Description                                  |
|----------------------|----------------------------------------------|
| `read_accounts`      | List accounts and their connection state     |
| `read_folders`       | List folders within an account               |
| `subscribe_events`   | Subscribe to engine events in real time      |

## Building

Plugins compile to WebAssembly (WASM). Any language that targets `wasm32-wasi`
works; this example uses Rust.

```bash
# Install the WASM target (one-time)
rustup target add wasm32-wasi

# Build the plugin
cargo build --target wasm32-wasi --release

# The output is at:
# target/wasm32-wasi/release/hello_world.wasm
```

## Installing

1. Copy `manifest.json` and the built `.wasm` file into your Kestrel plugin
   directory (default: `~/.config/kestrel/plugins/hello-world/`).
2. Restart Kestrel or reload plugins from the settings panel.
3. Grant the requested capabilities when prompted.

## Project structure

```
hello-world/
├── manifest.json   # Plugin manifest (name, version, capabilities)
├── README.md       # This file
└── src/
    └── lib.rs      # Plugin implementation (WASM entry points)
```

## Security

- The plugin runs in a WASM sandbox with no access to the filesystem,
  network, or credentials.
- All host API calls are gated by the declared capabilities.
- The host enforces a 64 MB memory limit and 1-second execution timeout
  per call.
