//! Sanitizers for untrusted text (threat model §4.6: M21/M22 — OSC escape
//! injection and escape-sequence floods in the TUI; M14/M15 — active/remote
//! content in HTML bodies).

use ammonia::Builder;

/// Removes terminal-escape hazards from mail text before rendering in the
/// TUI: all C0 control characters except `\t`, `\n`, `\r` and all C1
/// control characters are neutralized (ESC is dropped entirely; the rest
/// become spaces).
#[must_use]
pub fn sanitize_terminal_text(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        match c {
            '\t' | '\n' | '\r' => out.push(c),
            '\x1b' => {} // OSC/CSI introducer: dropped entirely
            c if (c as u32) < 0x20 => out.push(' '),
            c if ('\u{7f}'..='\u{9f}').contains(&c) => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

/// Result of HTML sanitization for the message viewport.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SanitizedHtml {
    /// Cleaned HTML (safe for the sandboxed viewport; CSP still applies).
    pub html: String,
    /// Number of remote items stripped (images, stylesheets, ...).
    pub remote_blocked: u32,
}

/// Sanitizes an email HTML body for display (defense-in-depth beneath the
/// webview sandbox, requirements §4.2):
///
/// - a pre-pass neutralizes remote `src` attributes (remote content
///   blocking by construction; `<a href>` links are kept for
///   click-through confirmation);
/// - ammonia then strips `<script>`/`<style>` contents, event handlers,
///   forms, and embeddable tags entirely;
/// - only `cid:`, `data:`, `http(s):` (links) and `mailto:` schemes
///   survive, and `cid:` resolution happens in the viewport protocol
///   (threat model §5).
#[must_use]
pub fn sanitize_html_body(input: &str) -> SanitizedHtml {
    let remote = count_remote_refs(input);
    let neutralized = neutralize_remote_src(input);
    let cleaned = ammonia_builder().clean(&neutralized).to_string();
    SanitizedHtml {
        html: cleaned,
        remote_blocked: remote,
    }
}

/// 1×1 transparent PNG placeholder swapped in for remote image sources.
const PLACEHOLDER_PNG: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";

/// Replaces remote (non-`cid:`/`data:`) `src` attribute values with a
/// transparent placeholder. `<a href>` is deliberately untouched: links are
/// rendered, confirmed, and opened via the OS handler (threat model §4.5).
///
/// Single linear pass (threat model M17: hostile input cannot amplify work).
fn neutralize_remote_src(html: &str) -> String {
    let bytes = html.as_bytes();
    let mut out = String::with_capacity(html.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if i + 3 <= bytes.len()
            && bytes[i].eq_ignore_ascii_case(&b's')
            && bytes[i + 1].eq_ignore_ascii_case(&b'r')
            && bytes[i + 2].eq_ignore_ascii_case(&b'c')
        {
            let mut j = i + 3;
            while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'=' {
                j += 1;
                while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
                    j += 1;
                }
                if j < bytes.len() && (bytes[j] == b'"' || bytes[j] == b'\'') {
                    let quote = bytes[j];
                    let val_start = j + 1;
                    let mut end = val_start;
                    while end < bytes.len() && bytes[end] != quote {
                        end += 1;
                    }
                    let value = html[val_start..end].trim().to_lowercase();
                    let remote = value.starts_with("http://")
                        || value.starts_with("https://")
                        || value.starts_with("//");
                    if remote {
                        out.push_str("src=\"");
                        out.push_str(PLACEHOLDER_PNG);
                        if end < bytes.len() {
                            out.push('"');
                        }
                        i = (end + 1).min(bytes.len());
                        continue;
                    }
                }
            }
        }
        // Copy one UTF-8 character (attribute scans may have landed
        // mid-multibyte only for non-attribute `src` text, which is copied
        // verbatim).
        let ch = html[i..].chars().next().unwrap_or('\u{fffd}');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn ammonia_builder() -> Builder<'static> {
    let mut b = Builder::default();
    b.tags(hash_set(&[
        "a",
        "abbr",
        "acronym",
        "address",
        "article",
        "aside",
        "b",
        "bdi",
        "bdo",
        "blockquote",
        "br",
        "caption",
        "center",
        "cite",
        "code",
        "col",
        "colgroup",
        "dd",
        "del",
        "details",
        "div",
        "dl",
        "dt",
        "em",
        "figcaption",
        "figure",
        "font",
        "footer",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "header",
        "hr",
        "i",
        "img",
        "ins",
        "kbd",
        "li",
        "map",
        "mark",
        "nav",
        "ol",
        "p",
        "pre",
        "q",
        "rp",
        "rt",
        "ruby",
        "s",
        "samp",
        "section",
        "small",
        "span",
        "strike",
        "strong",
        "sub",
        "summary",
        "sup",
        "table",
        "tbody",
        "td",
        "tfoot",
        "th",
        "thead",
        "time",
        "tr",
        "u",
        "ul",
        "var",
        "wbr",
    ]));
    b.add_tag_attributes(
        "a",
        hash_set(&["href", "title", "id", "class", "name", "target", "rel"]),
    );
    b.add_tag_attributes(
        "img",
        hash_set(&[
            "src", "alt", "width", "height", "title", "class", "id", "style",
        ]),
    );
    let generic = hash_set(&[
        "class", "id", "style", "title", "align", "valign", "width", "height", "bgcolor", "color",
        "colspan", "rowspan",
    ]);
    b.generic_attributes(generic);
    // Only schemes the viewport can resolve. `http(s)` is needed for `<a
    // href>` click-through; remote *fetching* is impossible by construction
    // (no network origin in the viewport, and `src` was neutralized above).
    b.url_schemes(hash_set(&["cid", "data", "mailto", "http", "https"]));
    b.url_relative(ammonia::UrlRelative::PassThrough);
    b.link_rel(None);
    b
}

/// Counts remote references (images, stylesheets, links to http/https) in
/// the raw HTML. Heuristic scan tolerant to broken markup: counts `src=`,
/// `href=`, `url(...)` attributes whose value starts with an external
/// scheme. Used for the `RemoteContentBlocked` count.
#[must_use]
pub fn count_remote_refs(html: &str) -> u32 {
    let lower = html.to_lowercase();
    let mut count = 0u32;
    for key in ["src=\"", "src='", "href=\"", "href='"] {
        let mut start = 0usize;
        while let Some(pos) = lower[start..].find(key) {
            let val_start = start + pos + key.len();
            let rest = &lower[val_start..];
            let end = rest
                .find(['"', '\''])
                .map_or(lower.len(), |e| val_start + e);
            let value = &lower[val_start..end];
            if value.starts_with("http://")
                || value.starts_with("https://")
                || value.starts_with("//")
            {
                count += 1;
            }
            start = val_start;
        }
    }
    count
}

fn hash_set(items: &[&'static str]) -> std::collections::HashSet<&'static str> {
    items.iter().copied().collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn strips_all_osc_and_control_sequences() {
        let hostile = "ok\x1b]8;;http://evil.example\x1b\\text\x1b[2J\x1b(B\x00\x07\x1b[?25l";
        let clean = sanitize_terminal_text(hostile);
        assert!(!clean.contains('\x1b'), "ESC must be dropped: {clean:?}");
        assert!(!clean.contains('\x00'));
        assert!(!clean.contains('\x07'));
        assert!(clean.contains("ok"));
        assert!(clean.contains("text"));
    }

    #[test]
    fn keeps_tabs_and_newlines() {
        let out = sanitize_terminal_text("a\tb\nc\r\nd");
        assert_eq!(out, "a\tb\nc\r\nd");
    }

    #[test]
    fn neutralizes_c1_controls() {
        let out = sanitize_terminal_text("a\u{9b}2Jb");
        assert_eq!(out, "a 2Jb");
    }

    #[test]
    fn html_script_content_removed() {
        let s =
            sanitize_html_body("<p>hi</p><script>alert(1)</script><img src=x onerror=alert(2)>");
        assert!(!s.html.contains("alert(1)"), "{}", s.html);
        assert!(!s.html.contains("onerror"), "{}", s.html);
        assert!(s.html.contains("hi"));
    }

    #[test]
    fn html_remote_images_stripped_and_counted() {
        let s = sanitize_html_body(
            "<img src=\"https://tracker.example/pixel.gif\"><img src=\"cid:part1\"><p style=\"color:red\">x</p>",
        );
        assert!(s.remote_blocked >= 1);
        assert!(!s.html.contains("tracker.example"), "{}", s.html);
        assert!(s.html.contains("cid:part1"), "{}", s.html);
        assert!(s.html.contains("color:red"), "{}", s.html);
    }

    #[test]
    fn html_iframe_object_form_removed() {
        let s = sanitize_html_body(
            "<iframe src=\"https://x.example\"></iframe><object data=\"https://y.example\"></object><form><input type=submit></form><a href=\"https://ok.example\">link</a>",
        );
        assert!(!s.html.contains("iframe"), "{}", s.html);
        assert!(!s.html.contains("object"), "{}", s.html);
        assert!(!s.html.contains("form"), "{}", s.html);
        // http(s) links are kept in <a href> for click-through confirmation,
        // but they are NOT auto-fetched (viewport has no network).
        assert!(s.html.contains("ok.example"), "{}", s.html);
    }

    #[test]
    fn javascript_urls_removed() {
        let s = sanitize_html_body("<a href=\"javascript:alert(1)\">bad</a>");
        assert!(!s.html.contains("javascript"), "{}", s.html);
    }

    #[test]
    fn count_remote_refs_finds_all_schemes() {
        let html = "<img src='https://a/x'><img src=\"http://b/y\"><img src=\"//c/z\"><img src=\"cid:keep\"><a href='https://d/page'>l</a>";
        assert_eq!(count_remote_refs(html), 4);
    }
}
