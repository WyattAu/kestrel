#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Integration tests for the plugin lifecycle: load, capability check,
//! execute, and unload.

use kestrel_plugin::{
    Capability, PluginError, PluginExecutor, PluginManifest, RuntimeConfig, parse_manifest,
};

fn hello_world_manifest_json() -> &'static str {
    r#"{
        "name": "hello-world",
        "version": "1.0.0",
        "author": "Kestrel Team",
        "description": "A sample plugin that demonstrates the plugin API",
        "capabilities": ["read_accounts", "read_folders", "subscribe_events"],
        "api_version": "1.0"
    }"#
}

fn valid_wasm_bytes() -> Vec<u8> {
    vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]
}

#[test]
fn load_plugin_from_manifest_and_wasm() {
    let manifest =
        parse_manifest(hello_world_manifest_json().as_bytes()).expect("should parse manifest");
    let mut executor = PluginExecutor::new(RuntimeConfig::default());
    let idx = executor
        .load_plugin(valid_wasm_bytes(), manifest)
        .expect("should load plugin");
    assert_eq!(idx, 0);
    assert_eq!(executor.plugin_count(), 1);
}

#[test]
fn capability_check_passes_for_hello_world() {
    let manifest =
        parse_manifest(hello_world_manifest_json().as_bytes()).expect("should parse manifest");
    let granted = vec![
        Capability::ReadAccounts,
        Capability::ReadFolders,
        Capability::SubscribeEvents,
    ];
    kestrel_plugin::manifest::check_capabilities(&manifest, &granted)
        .expect("all capabilities should be granted");
}

#[test]
fn capability_check_fails_when_missing() {
    let manifest =
        parse_manifest(hello_world_manifest_json().as_bytes()).expect("should parse manifest");
    let granted = vec![Capability::ReadAccounts, Capability::ReadFolders];
    let err = kestrel_plugin::manifest::check_capabilities(&manifest, &granted).unwrap_err();
    assert!(matches!(
        err,
        PluginError::CapabilityDenied(Capability::SubscribeEvents)
    ));
}

#[test]
fn plugin_execution_returns_placeholder() {
    let manifest =
        parse_manifest(hello_world_manifest_json().as_bytes()).expect("should parse manifest");
    let mut executor = PluginExecutor::new(RuntimeConfig::default());
    executor
        .load_plugin(valid_wasm_bytes(), manifest)
        .expect("should load");

    let result = executor.call_plugin(0, "run", &[]).expect("should call");
    assert!(result.is_empty());
}

#[test]
fn plugin_unload_by_dropping_executor() {
    let manifest =
        parse_manifest(hello_world_manifest_json().as_bytes()).expect("should parse manifest");
    let mut executor = PluginExecutor::new(RuntimeConfig::default());
    executor
        .load_plugin(valid_wasm_bytes(), manifest)
        .expect("should load");
    assert_eq!(executor.plugin_count(), 1);

    drop(executor);
}

#[test]
fn multiple_plugins_load_and_execute() {
    let mut executor = PluginExecutor::new(RuntimeConfig::default());

    for i in 0..3 {
        let mut manifest: PluginManifest =
            parse_manifest(hello_world_manifest_json().as_bytes()).expect("should parse");
        manifest.name = format!("plugin-{i}");
        let idx = executor
            .load_plugin(valid_wasm_bytes(), manifest)
            .expect("should load");
        assert_eq!(idx, i);
    }
    assert_eq!(executor.plugin_count(), 3);

    for i in 0..3 {
        let result = executor.call_plugin(i, "run", &[]).expect("should call");
        assert!(result.is_empty());
    }
}
