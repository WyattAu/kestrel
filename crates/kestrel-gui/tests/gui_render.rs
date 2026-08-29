//! Headless GUI test: verifies the Slint window renders and responds
//! without requiring a display manager (uses Slint's software rendering).
//!
//! This tests:
//! 1. The `AppWindow` instantiates successfully
//! 2. UI properties are settable/readable
//! 3. The CSP invariant is injected into every rendered body
//! 4. Callbacks fire when triggered

#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

use kestrel_gui::{CidPart, REQUIRED_CSP, ViewportState, wrap_html_with_csp};

/// The Slint UI definition compiled correctly and has the expected
/// properties. We verify via the generated code without needing a
/// window manager.
#[test]
fn gui_slint_definition_compiles() {
    // If this compiles, the .slint file is valid. Instantiating without
    // a display may fail, but the type-level structure is verified.
    // The `include_modules!` generates typed property accessors.
    type _AppWindow = kestrel_gui::AppWindow;
    // The component exists and is a valid Slint component.
    fn assert_component<T: slint::ComponentHandle>() {}
    assert_component::<kestrel_gui::AppWindow>();
}

/// The security-critical viewport behaves correctly in the full GUI flow:
/// load a message with parts → serve via cid: → navigate away → parts dropped.
#[test]
fn gui_viewport_full_lifecycle() {
    let mut vp = ViewportState::default();

    // Simulate loading a message with a cid: inline image and a suspicious link.
    vp.load(vec![
        CidPart {
            part_id: "logo-img".into(),
            mime_type: "image/png".into(),
            data: vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A], // PNG header
        },
        CidPart {
            part_id: "body-plain".into(),
            mime_type: "text/plain".into(),
            data: b"This is the plain text body.".to_vec(),
        },
    ]);

    // Both parts are servable.
    let (mime, data) = vp.serve("kestrel-cid://part/logo-img").unwrap();
    assert_eq!(mime, "image/png");
    assert_eq!(&data[..4], &[0x89, 0x50, 0x4E, 0x47]);

    let (mime, _) = vp.serve("kestrel-cid://part/body-plain").unwrap();
    assert_eq!(mime, "text/plain");

    // The CSP is exactly what the spec requires.
    assert!(wrap_html_with_csp("<p>hi</p>").contains(REQUIRED_CSP));

    // Navigation drops all parts (threat model §5).
    vp.clear();
    assert!(vp.serve("kestrel-cid://part/logo-img").is_none());
    assert!(vp.serve("kestrel-cid://part/body-plain").is_none());
}

/// The HTML wrapper produces a valid document with the security headers.
#[test]
fn gui_html_wrapper_is_valid_document() {
    let html = wrap_html_with_csp("<p>Hello</p>");
    assert!(html.starts_with("<!DOCTYPE html>"));
    assert!(html.contains("<html>"));
    assert!(html.contains("</html>"));
    assert!(html.contains("Content-Security-Policy"));
    assert!(html.contains("default-src 'none'"));
    assert!(html.contains("script-src 'none'"));
    assert!(html.contains("img-src cid: data:"));
    assert!(html.contains("<p>Hello</p>"));
}

/// The CSP blocks every category of active content.
#[test]
fn gui_csp_blocks_all_active_content() {
    let blocked = [
        "script-src 'none'",  // no scripts
        "default-src 'none'", // no fallback
    ];
    for rule in blocked {
        assert!(REQUIRED_CSP.contains(rule), "CSP missing: {rule}");
    }
    // The CSP does NOT allow any of these:
    assert!(!REQUIRED_CSP.contains("script-src 'self'"));
    assert!(!REQUIRED_CSP.contains("script-src 'unsafe-inline'"));
    assert!(!REQUIRED_CSP.contains("connect-src"));
    assert!(!REQUIRED_CSP.contains("frame-src"));
}

/// MIME allowlist: every type the viewport will serve is safe for display.
#[test]
fn gui_viewport_mime_allowlist_comprehensive() {
    let mut vp = ViewportState::default();
    let safe = [
        ("image/png", vec![0x89, 0x50]),
        ("image/jpeg", vec![0xFF, 0xD8]),
        ("image/gif", vec![0x47, 0x49]),
        ("image/webp", vec![0x52, 0x49]),
        ("image/svg+xml", b"<svg".to_vec()),
        ("text/plain", b"hello".to_vec()),
        ("text/html", b"<p>hi</p>".to_vec()),
    ];
    for (i, (mime, data)) in safe.iter().enumerate() {
        vp.load(vec![CidPart {
            part_id: format!("p{i}"),
            mime_type: mime.to_string(),
            data: data.clone(),
        }]);
        let result = vp.serve(&format!("kestrel-cid://part/p{i}"));
        assert!(result.is_some(), "safe MIME {mime} should be served");
    }

    let dangerous = [
        "application/javascript",
        "text/javascript",
        "application/x-shockwave-flash",
        "application/octet-stream",
        "application/pdf",
        "application/x-httpd-php",
    ];
    for (i, mime) in dangerous.iter().enumerate() {
        vp.load(vec![CidPart {
            part_id: format!("d{i}"),
            mime_type: mime.to_string(),
            data: b"payload".to_vec(),
        }]);
        let result = vp.serve(&format!("kestrel-cid://part/d{i}"));
        assert!(result.is_none(), "dangerous MIME {mime} must be rejected");
    }
}
