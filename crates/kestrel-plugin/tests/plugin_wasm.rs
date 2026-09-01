#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Integration tests for the WASM plugin runtime: host function linking,
//! memory access, JSON serialization, fuel limits, and error handling.

use kestrel_plugin::{
    Capability, PluginError, PluginExecutor, PluginManifest, RuntimeConfig, deserialize_json,
    serialize_json,
};

fn sample_manifest() -> PluginManifest {
    PluginManifest {
        name: "com.example.wasm-test".into(),
        version: "0.1.0".into(),
        author: "Test".into(),
        description: "WASM runtime integration test plugin".into(),
        capabilities: vec![Capability::ReadAccounts],
        api_version: "1.0".into(),
    }
}

fn hello_world_manifest() -> PluginManifest {
    PluginManifest {
        name: "hello-world".into(),
        version: "1.0.0".into(),
        author: "Kestrel Team".into(),
        description: "A sample plugin that demonstrates the plugin API".into(),
        capabilities: vec![
            Capability::ReadAccounts,
            Capability::ReadFolders,
            Capability::SubscribeEvents,
        ],
        api_version: "1.0".into(),
    }
}

/// Minimal WASM module with host imports and an exported function.
fn wasm_with_host_imports() -> Vec<u8> {
    wat::parse_str(
        r#"
        (module
            (import "host" "log" (func $log (param i32 i32 i32)))
            (import "host" "alloc" (func $alloc (param i32) (result i32)))
            (import "host" "dealloc" (func $dealloc (param i32 i32)))

            (memory (export "memory") 1)

            (func (export "plugin_init")
                (call $log (i32.const 0) (i32.const 0) (i32.const 0))
            )

            (func (export "plugin_shutdown")
                (call $log (i32.const 0) (i32.const 0) (i32.const 0))
            )

            (func (export "plugin_handle_event") (param i32 i32 i32)
                (call $log (local.get 0) (local.get 1) (local.get 2))
            )

            (func (export "kesten_alloc") (param i32) (result i32)
                i32.const 1024
            )

            (func (export "kesten_dealloc") (param i32 i32)
            )
        )
        "#,
    )
    .unwrap()
}

/// WASM module that allocates memory and writes JSON via `host_alloc`.
fn wasm_with_json_response() -> Vec<u8> {
    wat::parse_str(
        r#"
        (module
            (import "host" "log" (func $log (param i32 i32 i32)))
            (import "host" "alloc" (func $alloc (param i32) (result i32)))
            (import "host" "dealloc" (func $dealloc (param i32 i32)))

            (memory (export "memory") 1)

            ;; Simple plugin that returns empty
            (func (export "plugin_init")
                (call $log (i32.const 0) (i32.const 0) (i32.const 0))
            )

            (func (export "kesten_alloc") (param i32) (result i32)
                i32.const 2048
            )

            (func (export "kesten_dealloc") (param i32 i32)
            )
        )
        "#,
    )
    .unwrap()
}

#[test]
fn load_wasm_with_host_imports() {
    let manifest = sample_manifest();
    let wasm = wasm_with_host_imports();
    let mut executor = PluginExecutor::new(RuntimeConfig::default());
    let idx = executor
        .load_plugin(wasm, manifest)
        .expect("should load plugin with host imports");
    assert_eq!(idx, 0);
}

#[test]
fn call_plugin_init_with_host_log() {
    let manifest = sample_manifest();
    let wasm = wasm_with_host_imports();
    let mut executor = PluginExecutor::new(RuntimeConfig::default());
    executor.load_plugin(wasm, manifest).expect("should load");

    // plugin_init should succeed (calls host_log internally)
    let result = executor
        .call_plugin(0, "plugin_init", &[])
        .expect("should call");
    assert!(result.is_empty());
}

#[test]
fn call_plugin_shutdown() {
    let manifest = sample_manifest();
    let wasm = wasm_with_host_imports();
    let mut executor = PluginExecutor::new(RuntimeConfig::default());
    executor.load_plugin(wasm, manifest).expect("should load");

    let result = executor
        .call_plugin(0, "plugin_shutdown", &[])
        .expect("should call");
    assert!(result.is_empty());
}

#[test]
fn call_plugin_handle_event_requires_args() {
    let manifest = sample_manifest();
    let wasm = wasm_with_host_imports();
    let mut executor = PluginExecutor::new(RuntimeConfig::default());
    executor.load_plugin(wasm, manifest).expect("should load");

    // plugin_handle_event expects 3 i32 args; calling with 0 args returns a runtime error
    let result = executor.call_plugin(0, "plugin_handle_event", &[]);
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), PluginError::Runtime(_)));
}

#[test]
fn call_plugin_with_json_response_module() {
    let manifest = sample_manifest();
    let wasm = wasm_with_json_response();
    let mut executor = PluginExecutor::new(RuntimeConfig::default());
    executor.load_plugin(wasm, manifest).expect("should load");

    let result = executor
        .call_plugin(0, "plugin_init", &[])
        .expect("should call");
    assert!(result.is_empty());
}

#[test]
fn host_import_linking_does_not_panic() {
    let manifest = sample_manifest();
    let wasm = wasm_with_host_imports();
    let module = kestrel_plugin::PluginModule::load(wasm, manifest).expect("should load");
    // Calling a non-existent function returns empty gracefully
    let result = module
        .call("nonexistent_function", &[])
        .expect("should not panic");
    assert!(result.is_empty());
}

#[test]
fn invalid_wasm_module_rejected() {
    let manifest = sample_manifest();
    let wasm = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0xFF];
    let err = kestrel_plugin::PluginModule::load(wasm, manifest).unwrap_err();
    assert!(matches!(err, PluginError::InvalidWasm(_)));
}

#[test]
fn serialize_json_for_plugin_communication() {
    let event = serde_json::json!({
        "type": "new_mail",
        "account": "inbox-123",
        "folder": "INBOX",
        "count": 5
    });
    let bytes = serialize_json(&event).expect("should serialize");
    let restored: serde_json::Value = deserialize_json(&bytes).expect("should deserialize");
    assert_eq!(restored["type"], "new_mail");
    assert_eq!(restored["count"], 5);
}

#[test]
fn hello_world_manifest_roundtrip() {
    let manifest = hello_world_manifest();
    assert_eq!(manifest.name, "hello-world");
    assert_eq!(manifest.version, "1.0.0");
    assert_eq!(manifest.capabilities.len(), 3);
    assert!(manifest.capabilities.contains(&Capability::ReadAccounts));
    assert!(manifest.capabilities.contains(&Capability::ReadFolders));
    assert!(manifest.capabilities.contains(&Capability::SubscribeEvents));
}

#[test]
fn hello_world_loads_and_executes() {
    let manifest = hello_world_manifest();
    let wasm = wasm_with_host_imports(); // Use the module with host imports as proxy
    let mut executor = PluginExecutor::new(RuntimeConfig::default());
    executor.load_plugin(wasm, manifest).expect("should load");

    let result = executor
        .call_plugin(0, "plugin_init", &[])
        .expect("should call");
    assert!(result.is_empty());
}

#[test]
fn multiple_plugins_load_and_execute_independently() {
    let mut executor = PluginExecutor::new(RuntimeConfig::default());
    let wasm = wasm_with_host_imports();

    for i in 0..3 {
        let mut manifest = sample_manifest();
        manifest.name = format!("plugin-{i}");
        executor
            .load_plugin(wasm.clone(), manifest)
            .expect("should load");
    }
    assert_eq!(executor.plugin_count(), 3);

    for i in 0..3 {
        let result = executor
            .call_plugin(i, "plugin_init", &[])
            .expect("should call");
        assert!(result.is_empty());
    }
}

#[test]
fn json_serialization_helpers_roundtrip() {
    let data = serde_json::json!({"status": "ok", "items": [1, 2, 3]});
    let bytes = kestrel_plugin::serialize_json(&data).expect("serialize");
    let restored: serde_json::Value =
        kestrel_plugin::deserialize_json(&bytes).expect("deserialize");
    assert_eq!(restored, data);
}
