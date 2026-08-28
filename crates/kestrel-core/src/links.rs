//! Link classification (threat model §4.5: M19 punycode/IDN homographs,
//! M20 display-text/`href` mismatches).
//!
//! Every click on a flagged link must show a confirmation with the resolved
//! target; links never auto-open (`SuspiciousLink` events back this up).

use url::Url;

/// Risk verdict for a hyperlink.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkRisk {
    /// No heuristic tripped; normal handling.
    Safe,
    /// Host contains `xn--` (ACE) labels — punycode/IDN; confirm.
    Punycode,
    /// Host mixes scripts (e.g. Latin + Cyrillic) — homograph; confirm.
    MixedScript,
    /// Display text looks like a URL pointing somewhere else; confirm.
    DisplayMismatch,
}

/// Classifies a hyperlink from its `href` and (optional) display text.
#[must_use]
pub fn classify_link(href: &str, display_text: &str) -> LinkRisk {
    let Some(url) = parse_url(href) else {
        // Unparseable hrefs are handled by the frontend as opaque; not a
        // phishing heuristic.
        return LinkRisk::Safe;
    };
    let Some(host) = url.host_str() else {
        return LinkRisk::Safe;
    };

    // The URL parser normalizes non-ASCII domains to punycode (IDNA
    // toAscii), so homograph analysis runs on the *decoded* host: a host
    // that is mixed-script after decoding is the dangerous case; a clean
    // single-script IDN is merely "confirm punycode".
    if let Some(decoded) = idna_decode_host(host)
        && host_is_mixed_script(&decoded)
    {
        return LinkRisk::MixedScript;
    }
    if has_punycode_label(host) {
        return LinkRisk::Punycode;
    }
    if !display_text.trim().is_empty()
        && let Some(text_url) = looks_like_url(display_text.trim())
    {
        let text_host = text_url.host_str().unwrap_or_default();
        let href_host = normalize_host(host);
        if !text_host.is_empty()
            && normalize_host(text_host) != href_host
            && !is_subdomain_of(&normalize_host(text_host), &href_host)
        {
            return LinkRisk::DisplayMismatch;
        }
    }
    LinkRisk::Safe
}

/// IDNA-decodes a (possibly punycoded) host for script analysis.
fn idna_decode_host(host: &str) -> Option<String> {
    let ascii_only = host.is_ascii();
    if ascii_only && !has_punycode_label(host) {
        return None;
    }
    let (domain, _errors) = idna::domain_to_unicode(host);
    Some(domain)
}

fn parse_url(s: &str) -> Option<Url> {
    // Add a scheme when the text looks scheme-less so `example.com` parses;
    // browsers do the same, and phishers rely on it.
    if s.contains("://") || s.starts_with("mailto:") || s.starts_with("cid:") {
        Url::parse(s).ok()
    } else {
        Url::parse(&format!("http://{s}")).ok()
    }
}

fn looks_like_url(text: &str) -> Option<Url> {
    if text.contains("://") {
        return Url::parse(text).ok();
    }
    // Heuristic: contains a dot and no whitespace → treat as URL.
    if text.contains('.') && !text.contains(char::is_whitespace) {
        return Url::parse(&format!("http://{text}")).ok();
    }
    None
}

fn has_punycode_label(host: &str) -> bool {
    host.split('.').any(|label| {
        label
            .strip_prefix("xn--")
            .or_else(|| label.strip_prefix("XN--"))
            .is_some()
    })
}

fn normalize_host(host: &str) -> String {
    host.trim_end_matches('.').to_lowercase()
}

fn is_subdomain_of(child: &str, parent: &str) -> bool {
    child != parent
        && (child.ends_with(&format!(".{parent}")) || parent.ends_with(&format!(".{child}")))
}

/// Simple script classification of the common confusable set (Latin,
/// Cyrillic, Greek, Han) via code-point ranges. A host mixing two of these
/// scripts is a homograph candidate. ASCII letters count as Latin — that is
/// exactly the classic "Cyrillic а inside paypal" attack.
fn host_is_mixed_script(host: &str) -> bool {
    let mut scripts = Vec::new();
    for c in host.chars() {
        if c.is_ascii_digit() || c == '.' || c == '-' || c == '_' {
            continue;
        }
        let script = if c.is_ascii_alphabetic() {
            Script::Latin
        } else {
            match c {
                '\u{0400}'..='\u{04FF}' => Script::Cyrillic,
                '\u{0370}'..='\u{03FF}' | '\u{1F00}'..='\u{1FFF}' => Script::Greek,
                '\u{3040}'..='\u{30FF}' | '\u{4E00}'..='\u{9FFF}' | '\u{3400}'..='\u{4DBF}' => {
                    Script::Han
                }
                // Latin-1 supplement + Latin Extended are the same *script*
                // as ASCII Latin (Unicode property Common/Latin); folding
                // them avoids false positives on e.g. German umlauts.
                '\u{00C0}'..='\u{024F}' => Script::Latin,
                _ => Script::Other,
            }
        };
        if !scripts.contains(&script) {
            scripts.push(script);
        }
    }
    scripts.len() > 1
}

#[derive(Clone, Copy, PartialEq)]
enum Script {
    Latin,
    Cyrillic,
    Greek,
    Han,
    Other,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_urls_are_safe() {
        assert_eq!(
            classify_link("https://example.org/page", "click here"),
            LinkRisk::Safe
        );
        assert_eq!(
            classify_link("https://example.org", "https://example.org"),
            LinkRisk::Safe
        );
        assert_eq!(
            classify_link("mailto:a@b.example", "a@b.example"),
            LinkRisk::Safe
        );
    }

    #[test]
    fn punycode_hosts_flagged() {
        // Single-script IDN (bücher) — confirm-punycode, not mixed-script.
        assert_eq!(
            classify_link("https://xn--bcher-kva.example/shop", "books"),
            LinkRisk::Punycode
        );
        // The classic Cyrillic-apple homograph is mixed-script (stronger).
        assert_eq!(
            classify_link("https://xn--80ak6aa92e.com/login", "apple"),
            LinkRisk::MixedScript
        );
    }

    #[test]
    fn mixed_script_homograph_flagged() {
        // Cyrillic 'а' + Latin rest (fake "paypal").
        assert_eq!(
            classify_link("https://pаypal.example/", "paypal"),
            LinkRisk::MixedScript
        );
    }

    #[test]
    fn display_mismatch_flagged() {
        assert_eq!(
            classify_link("https://evil.example/login", "https://bank.example/login"),
            LinkRisk::DisplayMismatch
        );
    }

    #[test]
    fn subdomain_mismatch_not_flagged() {
        assert_eq!(
            classify_link("https://mail.example.org/inbox", "mail.example.org"),
            LinkRisk::Safe
        );
        assert_eq!(
            classify_link("https://example.org", "www.example.org"),
            LinkRisk::Safe
        );
    }

    #[test]
    fn garbage_input_never_panics() {
        for candidate in [
            "",
            "::::",
            "http://",
            "a b c",
            "\u{1f600}",
            "xn--",
            "https://\u{202e}",
        ] {
            let _ = classify_link(candidate, candidate);
        }
    }
}
