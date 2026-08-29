//! Sandboxed HTML viewport (threat model §4.4/§5): `wry` `WebView` with
//! the mandated CSP injected, JS disabled at engine level (no JS bridge
//! is ever registered), `file://` and all network schemes unreachable,
//! and the `kestrel-cid://` in-memory protocol for inline attachments.
//!
//! Security invariants (tested in `tests/viewport.rs`):
//! - CSP: `default-src 'none'; style-src 'unsafe-inline'; img-src cid:
//!   data:; script-src 'none';` on every load
//! - No JavaScript bridge registered (no `with_ipc_handler`)
//! - `kestrel-cid://` serves only the currently-loaded message's parts,
//!   MIME-type allowlisted, size ≤ 128 MiB, dropped on navigation

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

/// The mandated CSP (requirements §4.2).
pub const REQUIRED_CSP: &str =
    "default-src 'none'; style-src 'unsafe-inline'; img-src cid: data:; script-src 'none';";

/// A part served through `kestrel-cid://` (threat model §5).
#[derive(Clone, Debug)]
pub struct CidPart {
    /// The part id (opaque engine-issued).
    pub part_id: String,
    /// MIME type (allowlist enforced on serve).
    pub mime_type: String,
    /// Decoded bytes.
    pub data: Vec<u8>,
}

/// MIME allowlist for the cid: handler (threat model §5).
const ALLOWED_MIME: &[&str] = &[
    "image/png",
    "image/jpeg",
    "image/gif",
    "image/webp",
    "image/svg+xml",
    "image/bmp",
    "image/tiff",
    "text/plain",
    "text/html",
];

/// Maximum served part size (threat model §5: 128 MiB).
const MAX_PART_SIZE: usize = 128 * 1024 * 1024;

/// State: the parts of the single currently-loaded message.
/// Dropped on navigation.
#[derive(Default)]
pub struct ViewportState {
    parts: HashMap<String, CidPart>,
}

impl ViewportState {
    /// Loads parts for a message (replaces previous).
    pub fn load(&mut self, parts: Vec<CidPart>) {
        self.parts = parts.into_iter().map(|p| (p.part_id.clone(), p)).collect();
    }

    /// Drops all parts (navigation away).
    pub fn clear(&mut self) {
        self.parts.clear();
    }

    /// Serves a `kestrel-cid://part/<id>` request.
    ///
    /// Returns `(mime, body)` on success; `None` when the part is absent,
    /// the MIME type is not allowlisted, or the size exceeds the cap.
    #[must_use]
    pub fn serve(&self, url: &str) -> Option<(String, Vec<u8>)> {
        // Parse kestrel-cid://part/<PartId>
        let path = url.strip_prefix("kestrel-cid://part/")?;
        // No path traversal: engine-issued PartIds are opaque tokens;
        // reject anything containing '/' or '..'.
        if path.contains('/') || path.contains("..") || path.is_empty() {
            return None;
        }
        let part = self.parts.get(path)?;
        if !ALLOWED_MIME.contains(&part.mime_type.as_str()) {
            return None;
        }
        if part.data.len() > MAX_PART_SIZE {
            return None;
        }
        Some((part.mime_type.clone(), part.data.clone()))
    }
}

/// Wraps a sanitized HTML body into a full document with the CSP
/// injected as a `<meta>` tag (the webview enforces it on load).
///
/// # Errors
/// Never fails; the CSP is a static string.
#[must_use]
pub fn wrap_html_with_csp(sanitized_html: &str) -> String {
    format!(
        "<!DOCTYPE html>\n<html>\n<head>\n<meta charset=\"utf-8\">\n<meta http-equiv=\"Content-Security-Policy\" content=\"{REQUIRED_CSP}\">\n<style>body{{font-family:sans-serif;margin:0.5em;max-width:70em}}blockquote{{border-left:3px solid #999;margin:0.5em 0;padding-left:0.6em;color:#555}}pre{{background:#f4f4f4;padding:0.5em;overflow-x:auto}}img{{max-width:100%}}table{{border-collapse:collapse}}td,th{{border:1px solid #ccc;padding:0.3em 0.6em}}</style>\n</head>\n<body>\n{sanitized_html}\n</body>\n</html>"
    )
}

/// Shared viewport state handle.
pub type SharedViewportState = Arc<Mutex<ViewportState>>;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn csp_is_exact() {
        assert_eq!(
            REQUIRED_CSP,
            "default-src 'none'; style-src 'unsafe-inline'; img-src cid: data:; script-src 'none';"
        );
    }

    #[test]
    fn wrapped_html_contains_csp() {
        let html = wrap_html_with_csp("<p>hello</p>");
        assert!(html.contains("Content-Security-Policy"));
        assert!(html.contains("default-src 'none'"));
        assert!(html.contains("script-src 'none'"));
        assert!(html.contains("<p>hello</p>"));
    }

    #[test]
    fn cid_serves_allowlisted_parts() {
        let mut state = ViewportState::default();
        state.load(vec![CidPart {
            part_id: "p1".into(),
            mime_type: "image/png".into(),
            data: vec![0x89, 0x50, 0x4E, 0x47],
        }]);
        assert!(state.serve("kestrel-cid://part/p1").is_some());
        // Not loaded → 404 equivalent.
        assert!(state.serve("kestrel-cid://part/p2").is_none());
    }

    #[test]
    fn cid_rejects_disallowed_mime() {
        let mut state = ViewportState::default();
        state.load(vec![CidPart {
            part_id: "js".into(),
            mime_type: "application/javascript".into(),
            data: b"alert(1)".to_vec(),
        }]);
        assert!(state.serve("kestrel-cid://part/js").is_none());
    }

    #[test]
    fn cid_rejects_traversal() {
        let state = ViewportState::default();
        assert!(state.serve("kestrel-cid://part/../../etc/passwd").is_none());
        assert!(state.serve("kestrel-cid://part/a/b").is_none());
        assert!(state.serve("kestrel-cid://part/").is_none());
        assert!(state.serve("http://evil.example").is_none());
    }

    #[test]
    fn cid_rejects_oversized() {
        let mut state = ViewportState::default();
        state.load(vec![CidPart {
            part_id: "big".into(),
            mime_type: "image/png".into(),
            data: vec![0u8; MAX_PART_SIZE + 1],
        }]);
        assert!(state.serve("kestrel-cid://part/big").is_none());
    }

    #[test]
    fn navigation_clears_parts() {
        let mut state = ViewportState::default();
        state.load(vec![CidPart {
            part_id: "x".into(),
            mime_type: "text/plain".into(),
            data: b"ok".to_vec(),
        }]);
        assert!(state.serve("kestrel-cid://part/x").is_some());
        state.clear();
        assert!(state.serve("kestrel-cid://part/x").is_none());
    }
}
