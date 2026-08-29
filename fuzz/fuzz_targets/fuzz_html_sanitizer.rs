//! Fuzz target: HTML sanitizer (threat model M14-M16 — remote content
//! stripped, scripts dropped, output is safe for the sandboxed viewport).

#![no_main]

use kestrel_core::sanitizer::{count_remote_refs, sanitize_html_body};
use libfuzzer_sys::fuzz_target;

/// Checks whether `needle` appears inside a tag (as an attribute),
/// not as text content. Text containing "onerror=" is safe — it's
/// not an event handler when it's between tags.
fn in_tag_context(haystack: &str, needle: &str) -> bool {
    let lower = haystack.to_lowercase();
    let mut search_from = 0;
    while let Some(pos) = lower[search_from..].find(needle) {
        let abs = search_from + pos;
        // Walk backwards to find the nearest '<' or '>' — if '<' comes
        // first, we're inside a tag.
        let before = lower[..abs].rfind(['<', '>']);
        match before {
            Some(idx) if lower.as_bytes()[idx] == b'<' => return true,
            _ => {
                search_from = abs + needle.len();
            }
        }
    }
    false
}

fuzz_target!(|data: &[u8]| {
    let html = String::from_utf8_lossy(data).into_owned();
    let sanitized = sanitize_html_body(&html);
    let output = sanitized.html.to_lowercase();

    // Invariants (asserted under fuzzing):

    // No <script> element survives.
    assert!(!output.contains("<script"), "script tag leaked: {output:.200}");

    // No event handler attribute (onerror= etc.) inside a tag.
    for handler in ["onerror=", "onload=", "onclick=", "onmouseover="] {
        assert!(
            !in_tag_context(&output, handler),
            "event handler {handler} leaked in tag: {output:.200}"
        );
    }

    // No javascript: URL inside an attribute.
    assert!(!in_tag_context(&output, "javascript:"), "javascript: URL leaked");

    // The remote-content counter must not panic.
    let _ = count_remote_refs(&html);
});
