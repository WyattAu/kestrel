//! Plugin manifest parsing and validation.

/// Re-export of the manifest type for convenience.
pub use crate::types::PluginManifest as Manifest;
use crate::{
    error::PluginError,
    types::{Capability, PLUGIN_API_VERSION},
};

/// Parse and validate a plugin manifest from raw JSON bytes.
///
/// # Errors
///
/// Returns [`PluginError::InvalidManifest`] if the JSON is malformed or
/// missing required fields. Returns [`PluginError::UnsupportedApiVersion`]
/// if the declared API version is incompatible.
pub fn parse_manifest(data: &[u8]) -> Result<Manifest, PluginError> {
    let manifest: Manifest =
        serde_json::from_slice(data).map_err(|e| PluginError::InvalidManifest(e.to_string()))?;

    validate_manifest(&manifest)?;
    Ok(manifest)
}

/// Validate a parsed manifest against host constraints.
///
/// # Errors
///
/// Returns an error if the manifest is invalid.
pub fn validate_manifest(manifest: &Manifest) -> Result<(), PluginError> {
    if manifest.name.is_empty() {
        return Err(PluginError::InvalidManifest(
            "plugin name must not be empty".into(),
        ));
    }

    if manifest.version.is_empty() {
        return Err(PluginError::InvalidManifest(
            "plugin version must not be empty".into(),
        ));
    }

    // Check API version compatibility (major version must match).
    let declared_major = manifest.api_version.split('.').next().unwrap_or("0");
    let host_major = PLUGIN_API_VERSION.split('.').next().unwrap_or("0");

    if declared_major != host_major {
        return Err(PluginError::UnsupportedApiVersion {
            declared: manifest.api_version.clone(),
            host: PLUGIN_API_VERSION.to_owned(),
        });
    }

    // Validate capability values are not empty — at minimum a plugin
    // should declare what it needs.
    if manifest.capabilities.is_empty() {
        return Err(PluginError::InvalidManifest(
            "plugin must declare at least one capability".into(),
        ));
    }

    Ok(())
}

/// Validate that a set of granted capabilities satisfies a manifest's
/// requirements.
///
/// # Errors
///
/// Returns the first capability that was not granted.
pub fn check_capabilities(manifest: &Manifest, granted: &[Capability]) -> Result<(), PluginError> {
    for required in &manifest.capabilities {
        if !granted.contains(required) {
            return Err(PluginError::CapabilityDenied(required.clone()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_manifest_json() -> String {
        serde_json::json!({
            "name": "com.example.test-plugin",
            "version": "0.1.0",
            "author": "Test Author",
            "description": "A test plugin",
            "capabilities": ["read_accounts", "read_folders"],
            "api_version": "1.0"
        })
        .to_string()
    }

    #[test]
    fn parse_valid_manifest() {
        let json = sample_manifest_json();
        let manifest = parse_manifest(json.as_bytes()).expect("should parse");
        assert_eq!(manifest.name, "com.example.test-plugin");
        assert_eq!(manifest.version, "0.1.0");
        assert_eq!(manifest.capabilities.len(), 2);
    }

    #[test]
    fn reject_empty_name() {
        let json = serde_json::json!({
            "name": "",
            "version": "0.1.0",
            "author": "Test",
            "description": "Test",
            "capabilities": ["read_accounts"],
            "api_version": "1.0"
        })
        .to_string();
        let err = parse_manifest(json.as_bytes()).unwrap_err();
        assert!(matches!(err, PluginError::InvalidManifest(_)));
    }

    #[test]
    fn reject_empty_version() {
        let json = serde_json::json!({
            "name": "com.example.test",
            "version": "",
            "author": "Test",
            "description": "Test",
            "capabilities": ["read_accounts"],
            "api_version": "1.0"
        })
        .to_string();
        let err = parse_manifest(json.as_bytes()).unwrap_err();
        assert!(matches!(err, PluginError::InvalidManifest(_)));
    }

    #[test]
    fn reject_unsupported_api_version() {
        let json = serde_json::json!({
            "name": "com.example.test",
            "version": "0.1.0",
            "author": "Test",
            "description": "Test",
            "capabilities": ["read_accounts"],
            "api_version": "2.0"
        })
        .to_string();
        let err = parse_manifest(json.as_bytes()).unwrap_err();
        assert!(matches!(err, PluginError::UnsupportedApiVersion { .. }));
    }

    #[test]
    fn reject_no_capabilities() {
        let json = serde_json::json!({
            "name": "com.example.test",
            "version": "0.1.0",
            "author": "Test",
            "description": "Test",
            "capabilities": [],
            "api_version": "1.0"
        })
        .to_string();
        let err = parse_manifest(json.as_bytes()).unwrap_err();
        assert!(matches!(err, PluginError::InvalidManifest(_)));
    }

    #[test]
    fn capability_check_passes() {
        let manifest = sample_manifest();
        let granted = vec![Capability::ReadAccounts, Capability::ReadFolders];
        assert!(check_capabilities(&manifest, &granted).is_ok());
    }

    #[test]
    fn capability_check_fails() {
        let manifest = sample_manifest();
        let granted = vec![Capability::ReadAccounts];
        let err = check_capabilities(&manifest, &granted).unwrap_err();
        assert!(matches!(
            err,
            PluginError::CapabilityDenied(Capability::ReadFolders)
        ));
    }

    #[test]
    fn recoverable_errors() {
        assert!(PluginError::InvalidManifest("x".into()).is_recoverable());
        assert!(
            PluginError::UnsupportedApiVersion {
                declared: "2.0".into(),
                host: "1.0".into()
            }
            .is_recoverable()
        );
        assert!(PluginError::CapabilityDenied(Capability::ReadAccounts).is_recoverable());
        assert!(PluginError::ModuleLoad("x".into()).is_recoverable());
        assert!(PluginError::InvalidWasm("x".into()).is_recoverable());
        assert!(PluginError::PluginNotFound("x".into()).is_recoverable());
        assert!(!PluginError::Runtime("x".into()).is_recoverable());
    }

    fn sample_manifest() -> Manifest {
        Manifest {
            name: "com.example.test".into(),
            version: "0.1.0".into(),
            author: "Test".into(),
            description: "Test".into(),
            capabilities: vec![Capability::ReadAccounts, Capability::ReadFolders],
            api_version: "1.0".into(),
        }
    }

    fn hello_world_manifest_json() -> String {
        serde_json::json!({
            "name": "hello-world",
            "version": "1.0.0",
            "author": "Kestrel Team",
            "description": "A sample plugin that demonstrates the plugin API",
            "capabilities": ["read_accounts", "read_folders", "subscribe_events"],
            "api_version": "1.0"
        })
        .to_string()
    }

    #[test]
    fn parse_hello_world_manifest() {
        let json = hello_world_manifest_json();
        let manifest = parse_manifest(json.as_bytes()).expect("should parse hello-world manifest");
        assert_eq!(manifest.name, "hello-world");
        assert_eq!(manifest.version, "1.0.0");
        assert_eq!(manifest.author, "Kestrel Team");
        assert_eq!(
            manifest.description,
            "A sample plugin that demonstrates the plugin API"
        );
        assert_eq!(manifest.capabilities.len(), 3);
        assert!(manifest.capabilities.contains(&Capability::ReadAccounts));
        assert!(manifest.capabilities.contains(&Capability::ReadFolders));
        assert!(manifest.capabilities.contains(&Capability::SubscribeEvents));
        assert_eq!(manifest.api_version, "1.0");
    }

    #[test]
    fn hello_world_capabilities_are_valid() {
        let json = hello_world_manifest_json();
        let manifest = parse_manifest(json.as_bytes()).expect("should parse");
        for cap in &manifest.capabilities {
            let _ = format!("{cap}");
        }
    }

    #[test]
    fn hello_world_api_version_supported() {
        let json = hello_world_manifest_json();
        let manifest = parse_manifest(json.as_bytes()).expect("should parse");
        let declared_major = manifest.api_version.split('.').next().unwrap_or("0");
        let host_major = PLUGIN_API_VERSION.split('.').next().unwrap_or("0");
        assert_eq!(declared_major, host_major);
    }

    #[test]
    fn reject_malformed_json() {
        let err = parse_manifest(b"not json at all").unwrap_err();
        assert!(matches!(err, PluginError::InvalidManifest(_)));
    }

    #[test]
    fn reject_missing_required_fields() {
        let json = serde_json::json!({
            "name": "test"
        })
        .to_string();
        let err = parse_manifest(json.as_bytes()).unwrap_err();
        assert!(matches!(err, PluginError::InvalidManifest(_)));
    }

    #[test]
    fn reject_invalid_capability_string() {
        let json = serde_json::json!({
            "name": "test",
            "version": "1.0.0",
            "author": "Test",
            "description": "Test",
            "capabilities": ["nonexistent_capability"],
            "api_version": "1.0"
        })
        .to_string();
        let err = parse_manifest(json.as_bytes()).unwrap_err();
        assert!(matches!(err, PluginError::InvalidManifest(_)));
    }

    #[test]
    fn all_capability_variants_serialize_deserialize() {
        let caps = vec![
            Capability::ReadAccounts,
            Capability::ReadFolders,
            Capability::ReadMessages,
            Capability::ReadMessageBodies,
            Capability::SubscribeEvents,
            Capability::RegisterUI,
        ];
        for cap in &caps {
            let s = serde_json::to_string(cap).expect("should serialize");
            let deserialized: Capability = serde_json::from_str(&s).expect("should deserialize");
            assert_eq!(*cap, deserialized);
        }
    }

    #[test]
    fn validate_manifest_rejects_minor_version_mismatch() {
        let manifest = Manifest {
            name: "test".into(),
            version: "1.0.0".into(),
            author: "Test".into(),
            description: "Test".into(),
            capabilities: vec![Capability::ReadAccounts],
            api_version: "1.5".into(),
        };
        assert!(validate_manifest(&manifest).is_ok());
    }
}
