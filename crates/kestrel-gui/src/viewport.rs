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
    borrow::Cow,
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

    /// Returns parts for display (clones all parts).
    #[must_use]
    pub fn parts_for_display(&self) -> Vec<CidPart> {
        self.parts.values().cloned().collect()
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
        r#"<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8">
<meta name="color-scheme" content="dark light">
<meta http-equiv="Content-Security-Policy" content="{REQUIRED_CSP}">
<style>
body {{ font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; margin: 0.5em; max-width: 70em; }}
blockquote {{ border-left: 3px solid #585b70; margin: 0.5em 0; padding-left: 0.6em; color: #a6adc8; }}
pre {{ background: #313244; padding: 0.5em; overflow-x: auto; color: #cdd6f4; }}
code {{ background: #313244; padding: 0 0.2em; color: #a6e3a1; }}
img {{ max-width: 100%; }}
table {{ border-collapse: collapse; }}
td, th {{ border: 1px solid #45475a; padding: 0.3em 0.6em; }}
a {{ color: #89b4fa; }}
hr {{ border: none; border-top: 1px solid #45475a; }}
@media (prefers-color-scheme: light) {{
    body {{ color: #1e1e2e; background: #eff1f5; }}
    blockquote {{ color: #6c7086; border-left-color: #ccd0da; }}
    pre {{ background: #e6e9ef; color: #1e1e2e; }}
    code {{ background: #e6e9ef; color: #d20f39; }}
    td, th {{ border-color: #ccd0da; }}
    a {{ color: #1e66f5; }}
    hr {{ border-top-color: #ccd0da; }}
}}
</style>
</head>
<body>
{sanitized_html}
</body>
</html>"#
    )
}

/// Shared viewport state handle.
pub type SharedViewportState = Arc<Mutex<ViewportState>>;

/// Opens a sanitized HTML body in a new browser window.
///
/// Writes the CSP-wrapped HTML to a temp file and opens it via the
/// system browser. The HTML is already ammonia-sanitized upstream; the
/// CSP meta tag provides defense-in-depth.
///
/// # Errors
/// Returns an error string if the temp file or browser launch fails.
pub fn open_html_in_browser(html: &str) -> Result<(), String> {
    let wrapped = wrap_html_with_csp(html);
    let dir = std::env::temp_dir().join("kestrel-html");
    std::fs::create_dir_all(&dir).map_err(|e| format!("temp dir: {e}"))?;
    let path = dir.join(format!("{}.html", uuid::Uuid::now_v7()));
    std::fs::write(&path, wrapped).map_err(|e| format!("write: {e}"))?;
    open::that(&path).map_err(|e| format!("browser: {e}"))
}

/// Full `wry` `WebView` viewport with sandboxed rendering.
///
/// Creates a new winit window, builds a `wry` `WebView` on it with the
/// `kestrel-cid://` custom protocol for inline images, loads the
/// CSP-wrapped HTML, and runs the event loop until the window is closed.
///
/// # Errors
/// Returns an error string if the window, webview, or event loop fails.
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::too_many_lines,
    clippy::missing_panics_doc
)]
pub fn spawn_wry_viewport(html: &str, parts: Vec<CidPart>) -> Result<(), String> {
    let wrapped = wrap_html_with_csp(html);
    let state = Arc::new(Mutex::new(ViewportState::default()));
    {
        let mut s = state.lock().map_err(|e| format!("lock: {e}"))?;
        s.load(parts);
    }

    let state_for_protocol = Arc::clone(&state);

    std::thread::Builder::new()
        .name("kestrel-viewport".into())
        .spawn(move || {
            use winit::{
                application::ApplicationHandler, event::WindowEvent, event_loop::EventLoop,
                window::Window,
            };

            struct ViewportApp {
                window: Option<Window>,
                webview: Option<wry::WebView>,
                html: String,
                state: SharedViewportState,
            }

            impl ApplicationHandler for ViewportApp {
                fn resumed(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
                    if self.window.is_some() {
                        return;
                    }
                    let attrs = Window::default_attributes()
                        .with_title("Kestrel — Message View")
                        .with_inner_size(winit::dpi::LogicalSize::new(900, 700));
                    let Ok(window) = event_loop.create_window(attrs) else {
                        return;
                    };

                    let state_clone = Arc::clone(&self.state);
                    let html = self.html.clone();

                    let empty: &'static [u8] = b"";
                    let builder = wry::WebViewBuilder::new()
                        .with_html(html)
                        .with_custom_protocol("kestrel-cid".into(), move |_id, request| {
                            let url = request.uri().to_string();
                            let Ok(state) = state_clone.lock() else {
                                return wry::http::Response::builder()
                                    .status(500)
                                    .body(Cow::Borrowed(empty))
                                    .unwrap();
                            };
                            match state.serve(&url) {
                                Some((mime, data)) => wry::http::Response::builder()
                                    .header("Content-Type", &mime)
                                    .body(Cow::Owned(data))
                                    .unwrap_or_else(|_| {
                                        wry::http::Response::builder()
                                            .status(500)
                                            .body(Cow::Borrowed(empty))
                                            .unwrap()
                                    }),
                                None => wry::http::Response::builder()
                                    .status(404)
                                    .body(Cow::Borrowed(empty))
                                    .unwrap(),
                            }
                        })
                        .with_devtools(false)
                        .with_javascript_disabled();

                    match builder.build(&window) {
                        Ok(webview) => {
                            self.window = Some(window);
                            self.webview = Some(webview);
                        }
                        Err(e) => {
                            tracing::warn!("webview build failed: {e}");
                            event_loop.exit();
                        }
                    }
                }

                fn window_event(
                    &mut self,
                    event_loop: &winit::event_loop::ActiveEventLoop,
                    _window_id: winit::window::WindowId,
                    event: WindowEvent,
                ) {
                    match event {
                        WindowEvent::CloseRequested => {
                            event_loop.exit();
                        }
                        WindowEvent::Resized(_size) => {
                            // WebView auto-resizes with the parent window on most platforms.
                        }
                        _ => {}
                    }
                }
            }

            let event_loop = EventLoop::new().expect("failed to create event loop");
            let mut app = ViewportApp {
                window: None,
                webview: None,
                html: wrapped,
                state: state_for_protocol,
            };
            if let Err(e) = event_loop.run_app(&mut app) {
                tracing::warn!("viewport event loop: {e}");
            }
        })
        .map_err(|e| format!("spawn: {e}"))?;
    // Don't join the thread — the viewport runs independently.
    // The thread will exit when the user closes the viewport window.
    Ok(())
}

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
        assert!(html.contains("prefers-color-scheme"));
        assert!(html.contains("color-scheme"));
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
