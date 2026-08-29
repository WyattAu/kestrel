//! HTML → terminal text rendering (requirements §5): blockquotes as
//! indented bars, diff syntax coloring hooks, OSC 8 hyperlinks, ANSI
//! bold/italic. Input is pre-sanitized (`kestrel_core::sanitizer`).

use kestrel_core::sanitizer::sanitize_terminal_text;

/// A rendered line with optional OSC 8 link spans.
#[derive(Clone, Debug, Default)]
pub struct RenderedLine {
    /// Plain text (escape-free).
    pub text: String,
    /// Hyperlinks: (`start_byte`, `end_byte`, url).
    pub links: Vec<(usize, usize, String)>,
}

/// Renders sanitized HTML into terminal-friendly lines.
#[must_use]
pub fn html_to_lines(html: &str, width: usize) -> Vec<RenderedLine> {
    let mut lines = Vec::new();
    let mut current = RenderedLine::default();
    let mut tag_stack: Vec<String> = Vec::new();
    let mut in_tag = false;
    let mut tag_name = String::new();
    let mut link_url: Option<String> = String::new().into();
    let mut link_start = 0usize;
    let mut chars = html.chars().peekable();

    let effective_width = width.max(20).saturating_sub(2);

    while let Some(c) = chars.next() {
        match c {
            '<' => {
                in_tag = true;
                tag_name.clear();
            }
            '>' => {
                in_tag = false;
                let name = tag_name.trim().to_lowercase();
                if name.starts_with('/') {
                    let closing = name.strip_prefix('/').unwrap_or(&name).to_string();
                    if closing == "a"
                        && let Some(url) = link_url.take()
                    {
                        current.links.push((link_start, current.text.len(), url));
                    }
                    if let Some(pos) = tag_stack.iter().rposition(|t| *t == closing) {
                        tag_stack.truncate(pos);
                        match closing.as_str() {
                            "p" | "div" | "li" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "tr"
                            | "br" => {
                                push_line(&mut lines, &mut current, effective_width);
                            }
                            _ => {}
                        }
                    }
                } else if name.starts_with("a ") || name == "a" {
                    // href extraction from the raw tag content.
                    if let Some(url) = extract_href(&tag_name) {
                        link_url = Some(url);
                        link_start = current.text.len();
                    }
                } else if name == "br" {
                    push_line(&mut lines, &mut current, effective_width);
                } else if name == "li" {
                    current.text.push_str("  • ");
                } else if matches!(name.as_str(), "blockquote" | "pre") {
                    tag_stack.push(name);
                } else if name == "script" || name == "style" {
                    // Skip content until closing tag.
                    while let Some(c2) = chars.next() {
                        if c2 == '<' {
                            let mut skip = String::new();
                            let mut ci = chars.clone();
                            while let Some(&c3) = ci.peek() {
                                skip.push(c3);
                                ci.next();
                                if c3 == '>' {
                                    break;
                                }
                            }
                            chars = ci;
                            if skip.to_lowercase().contains(&format!("/{name}")) {
                                break;
                            }
                        }
                    }
                } else {
                    tag_stack.push(name);
                }
            }
            _ if in_tag => tag_name.push(c),
            _ => {
                let sanitized = sanitize_terminal_text(&c.to_string());
                if tag_stack.iter().any(|t| t == "blockquote") {
                    // Indent blockquote content.
                    if current.text.is_empty() {
                        current.text.push_str("  │ ");
                    }
                }
                current.text.push_str(&sanitized);
            }
        }
    }
    push_line(&mut lines, &mut current, effective_width);
    lines
}

fn push_line(lines: &mut Vec<RenderedLine>, current: &mut RenderedLine, width: usize) {
    if current.text.trim().is_empty() && current.links.is_empty() {
        lines.push(RenderedLine::default());
        return;
    }
    // Word-wrap at width.
    let text = current.text.clone();
    if text.len() <= width {
        lines.push(RenderedLine {
            text,
            links: current.links.clone(),
        });
    } else {
        let mut offset = 0usize;
        while offset < text.len() {
            let end = (offset + width).min(text.len());
            let mut break_at = end;
            if end < text.len() {
                // Find last space.
                if let Some(sp) = text[offset..end].rfind(' ') {
                    break_at = offset + sp;
                }
            }
            let line_text = text[offset..break_at.max(offset + 1).min(text.len())].to_string();
            let mut line_links = Vec::new();
            for (ls, le, url) in &current.links {
                let s = ls.saturating_sub(offset);
                let e = le.saturating_sub(offset).min(line_text.len());
                if s < line_text.len() && e > s {
                    line_links.push((s, e, url.clone()));
                }
            }
            lines.push(RenderedLine {
                text: line_text,
                links: line_links,
            });
            offset = break_at + 1;
        }
    }
    current.text.clear();
    current.links.clear();
}

fn extract_href(tag: &str) -> Option<String> {
    let lower = tag.to_lowercase();
    let pos = lower.find("href=")?;
    let rest = &tag[pos + 5..];
    let quote = rest.chars().next()?;
    if quote == '"' || quote == '\'' {
        let end = rest[1..].find(quote)? + 1;
        Some(rest[1..end].to_string())
    } else {
        let end = rest.find([' ', '>']).unwrap_or(rest.len());
        Some(rest[..end].to_string())
    }
}

/// Wraps text in an OSC 8 hyperlink escape (if supported).
#[must_use]
pub fn osc8_link(url: &str, text: &str) -> String {
    format!("\x1b]8;;{url}\x1b\\{text}\x1b]8;;\x1b\\")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn extracts_links_from_anchors() {
        let lines = html_to_lines(
            "<p>Visit <a href=\"https://example.org\">our site</a> for more.</p>",
            80,
        );
        let with_link = lines.iter().find(|l| !l.links.is_empty());
        assert!(with_link.is_some(), "at least one link");
        let line = with_link.unwrap();
        assert!(line.text.contains("our site"));
        assert_eq!(line.links[0].2, "https://example.org");
    }

    #[test]
    fn blockquotes_indented() {
        let lines = html_to_lines("<p>normal</p><blockquote>quoted text</blockquote>", 60);
        let quoted = lines
            .iter()
            .find(|l| l.text.contains("quoted"))
            .unwrap_or_else(|| panic!("no quoted line: {lines:?}"));
        assert!(quoted.text.starts_with("  │"), "{:?}", quoted.text);
    }

    #[test]
    fn list_items_bulleted() {
        let lines = html_to_lines("<ul><li>first</li><li>second</li></ul>", 60);
        assert!(lines.iter().any(|l| l.text.contains("• first")));
        assert!(lines.iter().any(|l| l.text.contains("• second")));
    }

    #[test]
    fn script_content_dropped() {
        let lines = html_to_lines("<p>ok</p><script>alert(1)</script>", 60);
        assert!(lines.iter().any(|l| l.text.contains("ok")));
        assert!(!lines.iter().any(|l| l.text.contains("alert")));
    }

    #[test]
    fn wide_text_wraps() {
        let long = "word ".repeat(40);
        let lines = html_to_lines(&format!("<p>{long}</p>"), 30);
        assert!(
            lines.len() > 3,
            "wrapped into multiple lines: {}",
            lines.len()
        );
    }

    #[test]
    fn osc8_link_escapes() {
        let s = osc8_link("https://x.example", "text");
        assert!(s.starts_with("\x1b]8;;https://x.example\x1b\\"));
        assert!(s.ends_with("\x1b]8;;\x1b\\"));
    }

    #[test]
    fn terminal_escapes_neutralized() {
        let lines = html_to_lines("<p>evil\x1b]8;;http://evil\x1b\\ injected</p>", 60);
        for l in &lines {
            assert!(!l.text.contains('\x1b'), "escape leaked: {:?}", l.text);
        }
    }
}
