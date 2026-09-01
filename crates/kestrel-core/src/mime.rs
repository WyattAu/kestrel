//! MIME parsing (ADR 0002): Stalwart `mail-parser` behind the core
//! [`MimeParser`] trait, with the hard limits of threat model §4.2 enforced
//! by the adapter regardless of upstream quality.
//!
//! Limits (all violations are typed `ParseLimit` errors — the message stays
//! listable with a degraded view):
//!
//! | Limit | Value |
//! |-------|-------|
//! | Nesting depth | 64 |
//! | Single decoded part | 128 MiB |
//! | Total decoded message | 512 MiB |
//! | Header count | 1024 |
//! | Single header line | 64 KiB |
//! | Decoded/encoded ratio | 100× |

use mail_parser::{
    Address as MpAddress, HeaderValue, Message as MpMessage, MessageParser, MessagePart,
    MimeHeaders, PartType,
};

use crate::{
    clock::UnixMillis,
    error::{KestrelError, LimitKind},
    protocol::Address,
};

/// Maximum MIME tree nesting depth (threat model §4.2).
pub const MAX_NESTING_DEPTH: usize = 64;
/// Maximum size of a single decoded part.
pub const MAX_PART_BYTES: u64 = 128 * 1024 * 1024;
/// Maximum total decoded size of a message.
pub const MAX_TOTAL_BYTES: u64 = 512 * 1024 * 1024;
/// Maximum number of header fields (per message or nested message).
pub const MAX_HEADER_COUNT: usize = 1024;
/// Maximum bytes of a single (unfolded) header line.
pub const MAX_HEADER_LINE_BYTES: usize = 64 * 1024;
/// Maximum decoded/encoded expansion ratio for transfer encodings.
pub const DECOMPRESSION_RATIO: u64 = 100;

/// The swappable parser boundary (ADR 0002).
pub trait MimeParser {
    /// Fully-parsed, owned view of a message.
    type Output;
    /// Parses raw RFC 5322 bytes. Never panics on hostile input; degradation
    /// is surfaced as warnings or typed `ParseLimit` errors.
    ///
    /// # Errors
    /// `ParseMalformed` when the buffer is not parseable at all;
    /// `ParseLimit` when a hard limit trips (threat model §4.2).
    fn parse(raw: &[u8]) -> Result<Self::Output, KestrelError>;
}

/// A fully parsed message, owned (no borrows into the raw buffer survive).
#[derive(Clone, Debug, Default)]
pub struct ParsedMessage {
    /// Normalized `Message-ID` (angle brackets stripped).
    pub message_id: Option<String>,
    /// Normalized `In-Reply-To`.
    pub in_reply_to: Option<String>,
    /// `References` chain, in order.
    pub references: Vec<String>,
    /// Decoded `Subject`.
    pub subject: Option<String>,
    /// `From` addresses.
    pub from: Vec<Address>,
    /// `To` addresses.
    pub to: Vec<Address>,
    /// `Cc` addresses.
    pub cc: Vec<Address>,
    /// `Bcc` addresses (present in drafts).
    pub bcc: Vec<Address>,
    /// `Date` as unix ms, if parseable.
    pub date: Option<UnixMillis>,
    /// Flattened MIME parts in traversal order (`seq` indexes this).
    pub parts: Vec<ParsedPart>,
    /// Best-effort assembled `text/plain` body.
    pub text_body: Option<String>,
    /// First `text/html` body.
    pub html_body: Option<String>,
    /// Non-fatal degradation notes (ADR 0002: map to warnings, never panic).
    pub warnings: Vec<String>,
}

/// One flattened MIME part.
#[derive(Clone, Debug)]
pub struct ParsedPart {
    /// Traversal order.
    pub seq: u32,
    /// Lowercased `type/subtype`.
    pub mime_type: String,
    /// `Content-ID` without angle brackets.
    pub content_id: Option<String>,
    /// `inline` / `attachment` / `None`.
    pub disposition: Option<String>,
    /// Suggested filename.
    pub filename: Option<String>,
    /// Transfer encoding (`base64`, `quoted-printable`, `none`, ...).
    pub encoding: String,
    /// Encoded (wire) size in bytes.
    pub encoded_size: u64,
    /// Decoded size in bytes.
    pub decoded_size: u64,
    /// Owned content.
    pub content: PartContent,
}

/// Owned content of a part.
#[derive(Clone, Debug)]
pub enum PartContent {
    /// Decoded, charset-transcoded UTF-8 text (covers `text/*`).
    Text(String),
    /// Decoded binary bytes.
    Binary(Vec<u8>),
    /// Nested `message/rfc822` payload.
    Nested(Box<ParsedMessage>),
}

/// Concrete adapter over Stalwart `mail-parser` (ADR 0002).
pub struct StalwartParser;

impl MimeParser for StalwartParser {
    type Output = ParsedMessage;

    fn parse(raw: &[u8]) -> Result<Self::Output, KestrelError> {
        scan_header_limits(raw)?;
        // The parser is upstream-fuzzed and claims best-effort semantics;
        // we treat total failure as malformed, everything else as warnings.
        let Some(message) = MessageParser::default().parse(raw) else {
            return Err(KestrelError::ParseMalformed {
                detail: "parser returned nothing".to_string(),
            });
        };
        let mut out = ParsedMessage::default();
        let mut total_decoded: u64 = 0;
        convert_message(&message, &mut out, &mut total_decoded)?;
        Ok(out)
    }
}

/// Fast pre-parse scan of the top-level header block; rejects absurd header
/// counts/sizes before handing bytes to the tree parser (threat model §4.2).
fn scan_header_limits(raw: &[u8]) -> Result<(), KestrelError> {
    let mut line_start = 0usize;
    let mut count = 0usize;
    let mut i = 0usize;
    while i < raw.len() {
        let b = raw[i];
        if b == b'\n' {
            let line_end = i;
            let line_len = line_end.saturating_sub(line_start);
            if line_len > MAX_HEADER_LINE_BYTES {
                return Err(KestrelError::ParseLimit {
                    kind: LimitKind::HeaderSize,
                    actual: format!("{line_len} bytes"),
                });
            }
            let blank = matches!(raw.get(line_end.wrapping_sub(1)), Some(b'\r')) && line_len == 1
                || line_len == 0;
            count += 1;
            if blank {
                return Ok(());
            }
            if count > MAX_HEADER_COUNT {
                return Err(KestrelError::ParseLimit {
                    kind: LimitKind::HeaderCount,
                    actual: format!("{count} headers"),
                });
            }
            line_start = i + 1;
        }
        i += 1;
    }
    Ok(())
}

fn convert_message(
    message: &MpMessage<'_>,
    out: &mut ParsedMessage,
    total_decoded: &mut u64,
) -> Result<(), KestrelError> {
    if let Some(mid) = message.message_id() {
        out.message_id = Some(strip_angle(mid));
    }
    out.in_reply_to = first_message_id(message.in_reply_to()).map(strip_angle);
    if let HeaderValue::TextList(ids) = message.references() {
        out.references = ids.iter().map(|s| strip_angle(s)).collect::<Vec<_>>();
    }
    out.subject = message.subject().map(str::to_owned);
    out.from = convert_addr(message.from());
    out.to = convert_addr(message.to());
    out.cc = convert_addr(message.cc());
    out.bcc = convert_addr(message.bcc());
    if let Some(date) = message.date() {
        out.date = Some(date.to_timestamp() * 1000);
    }
    if message.root_part().is_encoding_problem {
        out.warnings
            .push("root part had encoding problems; best-effort decode".to_string());
    }
    let root = message.root_part();
    walk_part(message, root, 0, out, total_decoded)?;
    Ok(())
}

/// Depth-first walk of the part tree. Returns the chosen text/html bodies via
/// `out`.
fn walk_part(
    message: &MpMessage<'_>,
    part: &MessagePart<'_>,
    depth: usize,
    out: &mut ParsedMessage,
    total_decoded: &mut u64,
) -> Result<(), KestrelError> {
    if depth > MAX_NESTING_DEPTH {
        return Err(KestrelError::ParseLimit {
            kind: LimitKind::NestingDepth,
            actual: format!("depth {depth}"),
        });
    }

    let seq = u32::try_from(out.parts.len()).map_err(|_| KestrelError::Bug {
        detail: "more than 2^32 parts".to_string(),
    })?;

    let ct = part.content_type();
    let mime_type = match ct {
        Some(c) => match &c.c_subtype {
            Some(sub) => format!("{}/{}", c.c_type, sub).to_lowercase(),
            None => c.c_type.to_lowercase(),
        },
        None => "text/plain".to_string(),
    };

    let disposition = part.content_disposition().map(|d| d.c_type.to_lowercase());
    let filename = part
        .content_disposition()
        .and_then(|d| attr(d.attributes.as_deref(), "filename"))
        .or_else(|| ct.and_then(|c| attr(c.attributes.as_deref(), "name")))
        .map(str::to_owned);
    let content_id = part.content_id().map(strip_angle);

    let encoded_size = u64::from(part.offset_end.saturating_sub(part.offset_body));
    let encoding = format!("{:?}", part.encoding).to_lowercase();

    if part.is_encoding_problem {
        out.warnings.push(format!(
            "part {seq} had encoding problems; best-effort decode"
        ));
    }

    match &part.body {
        PartType::Multipart(children) => {
            for &child_id in children {
                if let Some(child) = message.part(child_id) {
                    walk_part(message, child, depth + 1, out, total_decoded)?;
                }
            }
            Ok(())
        }
        PartType::Message(nested) => {
            let mut nested_out = ParsedMessage::default();
            convert_message(nested, &mut nested_out, total_decoded)?;
            let decoded_size = u64::try_from(nested.raw_message.len()).unwrap_or(u64::MAX);
            check_sizes(seq, encoded_size, decoded_size, total_decoded)?;
            out.parts.push(ParsedPart {
                seq,
                mime_type,
                content_id,
                disposition,
                filename,
                encoding,
                encoded_size,
                decoded_size,
                content: PartContent::Nested(Box::new(nested_out)),
            });
            Ok(())
        }
        PartType::Text(text) | PartType::Html(text) => {
            let is_html_part = matches!(part.body, PartType::Html(_));
            let decoded_size = u64::try_from(text.len()).unwrap_or(u64::MAX);
            check_sizes(seq, encoded_size, decoded_size, total_decoded)?;
            let owned = text.clone().into_owned();
            let non_attachment = disposition.as_deref() != Some("attachment");
            if is_html_part {
                if out.html_body.is_none() && non_attachment {
                    out.html_body = Some(owned.clone());
                }
            } else if out.text_body.is_none() && non_attachment {
                out.text_body = Some(owned.clone());
            }
            out.parts.push(ParsedPart {
                seq,
                mime_type,
                content_id,
                disposition,
                filename,
                encoding,
                encoded_size,
                decoded_size,
                content: PartContent::Text(owned),
            });
            Ok(())
        }
        PartType::Binary(bytes) | PartType::InlineBinary(bytes) => push_binary(
            out,
            seq,
            LeafMeta {
                mime_type,
                content_id,
                disposition,
                filename,
                encoding,
                encoded_size,
            },
            bytes,
            total_decoded,
        ),
    }
}

/// Enforces part/total/ratio limits (threat model §4.2).
fn check_sizes(
    seq: u32,
    encoded_size: u64,
    decoded_size: u64,
    total_decoded: &mut u64,
) -> Result<(), KestrelError> {
    if decoded_size > MAX_PART_BYTES {
        return Err(KestrelError::ParseLimit {
            kind: LimitKind::PartSize,
            actual: format!("part {seq}: {decoded_size} bytes"),
        });
    }
    *total_decoded += decoded_size;
    if *total_decoded > MAX_TOTAL_BYTES {
        return Err(KestrelError::ParseLimit {
            kind: LimitKind::TotalSize,
            actual: format!("total {total_decoded} bytes"),
        });
    }
    if encoded_size > 0 && decoded_size / encoded_size.max(1) > DECOMPRESSION_RATIO {
        return Err(KestrelError::ParseLimit {
            kind: LimitKind::DecompressionRatio,
            actual: format!("part {seq}: {decoded_size}/{encoded_size}"),
        });
    }
    Ok(())
}

/// Shared leaf-part metadata gathered before dispatch.
struct LeafMeta {
    mime_type: String,
    content_id: Option<String>,
    disposition: Option<String>,
    filename: Option<String>,
    encoding: String,
    encoded_size: u64,
}

/// Pushes a decoded binary leaf after enforcing size limits.
fn push_binary(
    out: &mut ParsedMessage,
    seq: u32,
    meta: LeafMeta,
    bytes: &[u8],
    total_decoded: &mut u64,
) -> Result<(), KestrelError> {
    let decoded_size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    check_sizes(seq, meta.encoded_size, decoded_size, total_decoded)?;
    out.parts.push(ParsedPart {
        seq,
        mime_type: meta.mime_type,
        content_id: meta.content_id,
        disposition: meta.disposition,
        filename: meta.filename,
        encoding: meta.encoding,
        encoded_size: meta.encoded_size,
        decoded_size,
        content: PartContent::Binary(bytes.to_vec()),
    });
    Ok(())
}

fn attr<'a>(attrs: Option<&'a [mail_parser::Attribute<'_>]>, name: &str) -> Option<&'a str> {
    attrs?
        .iter()
        .find(|a| a.name.eq_ignore_ascii_case(name))
        .map(|a| a.value.as_ref())
}

fn convert_addr(value: Option<&MpAddress<'_>>) -> Vec<Address> {
    match value {
        Some(MpAddress::List(list)) => list
            .iter()
            .filter_map(|a| {
                a.address.as_ref().map(|addr| Address {
                    name: a.name.as_ref().map(ToString::to_string),
                    email: addr.to_string(),
                })
            })
            .collect(),
        Some(MpAddress::Group(groups)) => groups
            .iter()
            .flat_map(|g| g.addresses.iter())
            .filter_map(|a| {
                a.address.as_ref().map(|addr| Address {
                    name: a.name.as_ref().map(ToString::to_string),
                    email: addr.to_string(),
                })
            })
            .collect(),
        None => Vec::new(),
    }
}

fn first_message_id<'x>(value: &'x HeaderValue<'x>) -> Option<&'x str> {
    match value {
        HeaderValue::Text(t) => Some(t),
        HeaderValue::TextList(list) => list.first().map(AsRef::as_ref),
        _ => None,
    }
}

fn strip_angle(s: &str) -> String {
    let s = s.trim();
    let s = s.strip_prefix('<').unwrap_or(s);
    let s = s.strip_suffix('>').unwrap_or(s);
    s.to_owned()
}

/// Text used for full-text indexing: the plain body when present, otherwise
/// HTML with tags stripped (schema.md §5: `body_plain` is extracted text,
/// never HTML).
#[must_use]
pub fn text_for_index(parsed: &ParsedMessage) -> String {
    if let Some(text) = &parsed.text_body {
        return text.clone();
    }
    parsed
        .html_body
        .as_ref()
        .map_or_else(String::new, |html| html_to_plain_text(html))
}

/// Minimal HTML → text conversion for indexing and TUI fallbacks. Tolerant of
/// broken markup: it scans tags and decodes common entities; it is NOT a
/// renderer (requirements §5 uses a real HTML-to-text converter in the TUI).
/// `<script>`/`<style>` element contents are dropped entirely — they are not
/// text content.
#[must_use]
pub fn html_to_plain_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let mut in_tag = false;
    // Set while inside an element whose content must be suppressed.
    let mut suppress_for: Option<String> = None;
    let mut tag_name = String::new();
    let mut chars = html.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '<' => {
                in_tag = true;
                tag_name.clear();
            }
            '>' => {
                in_tag = false;
                let name = tag_name.trim().to_lowercase();
                if let Some(open) = &suppress_for {
                    if name == format!("/{open}") {
                        suppress_for = None;
                    }
                } else if name == "script" || name == "style" {
                    suppress_for = Some(name);
                }
                out.push(' ');
            }
            _ if in_tag => tag_name.push(c),
            _ if suppress_for.is_some() => {}
            _ => {
                if c == '&' {
                    decode_entity(&mut chars, &mut out);
                } else {
                    out.push(c);
                }
            }
        }
    }
    // Collapse whitespace runs.
    let mut collapsed = String::with_capacity(out.len());
    let mut last_space = false;
    for ch in out.chars() {
        let is_ws = ch.is_whitespace();
        if is_ws && last_space {
            continue;
        }
        collapsed.push(if is_ws { ' ' } else { ch });
        last_space = is_ws;
    }
    collapsed.trim().to_owned()
}

fn decode_entity(chars: &mut std::iter::Peekable<std::str::Chars<'_>>, out: &mut String) {
    let mut entity = String::new();
    while let Some(&c) = chars.peek() {
        entity.push(c);
        chars.next();
        match c {
            ';' => break,
            '#' | 'a'..='z' | 'A'..='Z' | '0'..='9' => {}
            _ => {
                out.push('&');
                out.push_str(&entity);
                return;
            }
        }
        if entity.len() > 10 {
            out.push('&');
            out.push_str(&entity);
            return;
        }
    }
    let decoded = match entity.as_str() {
        "amp;" => '&',
        "lt;" => '<',
        "gt;" => '>',
        "quot;" => '"',
        "apos;" => '\'',
        "nbsp;" => ' ',
        other => {
            if let Some(num) = other.strip_prefix('#') {
                let code =
                    if let Some(hex) = num.strip_prefix("x").or_else(|| num.strip_prefix("X")) {
                        u32::from_str_radix(hex.trim_end_matches(';'), 16).ok()
                    } else {
                        num.trim_end_matches(';').parse::<u32>().ok()
                    };
                code.and_then(char::from_u32).unwrap_or('\u{fffd}')
            } else {
                '\u{fffd}'
            }
        }
    };
    out.push(decoded);
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::fmt::Write as _;

    use proptest::prelude::*;

    use super::*;

    const SIMPLE: &str = "From: Alice <alice@example.org>\r\nTo: Bob <bob@example.org>\r\nSubject: Hello\r\nDate: Fri, 28 Aug 2026 10:00:00 +0000\r\nMessage-ID: <m1@example.org>\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nHi Bob!\r\n";

    const MULTIPART: &str = "From: a@example.org\r\nSubject: mp\r\nMessage-ID: <mp1@x>\r\nMIME-Version: 1.0\r\nContent-Type: multipart/alternative; boundary=\"BB\"\r\n\r\n--BB\r\nContent-Type: text/plain\r\n\r\nplain body\r\n--BB\r\nContent-Type: text/html\r\n\r\n<html><body>html body</body></html>\r\n--BB--\r\n";

    #[test]
    fn parses_simple_message() {
        let m = StalwartParser::parse(SIMPLE.as_bytes()).unwrap();
        assert_eq!(m.subject.as_deref(), Some("Hello"));
        assert_eq!(m.message_id.as_deref(), Some("m1@example.org"));
        assert_eq!(m.from.len(), 1);
        assert_eq!(m.from[0].email, "alice@example.org");
        assert_eq!(m.from[0].name.as_deref(), Some("Alice"));
        assert_eq!(m.text_body.as_deref(), Some("Hi Bob!\r\n"));
        assert!(m.date.is_some());
        assert!(m.warnings.is_empty());
    }

    #[test]
    fn parses_multipart_alternative() {
        let m = StalwartParser::parse(MULTIPART.as_bytes()).unwrap();
        assert_eq!(m.text_body.as_deref(), Some("plain body"));
        assert!(
            m.html_body
                .as_deref()
                .is_some_and(|h| h.contains("html body"))
        );
        assert_eq!(m.parts.len(), 2);
        assert_eq!(m.parts[0].mime_type, "text/plain");
        assert_eq!(m.parts[1].mime_type, "text/html");
    }

    #[test]
    fn broken_headers_fail_gracefully_not_panic() {
        // Missing semicolon in content-type params, unterminated boundary,
        // LF-only endings: parser must not panic and must produce something.
        let broken = b"From: x\r\nContent-Type: text/plain charset=utf-8\r\nSubject: \xff\xfe\r\n\r\nbody with \x00 ctl";
        let result = StalwartParser::parse(broken);
        assert!(result.is_ok() || matches!(result, Err(KestrelError::ParseMalformed { .. })));
    }

    #[test]
    fn header_count_limit_trips() {
        let mut msg = String::new();
        for i in 0..=MAX_HEADER_COUNT + 1 {
            let _ = write!(msg, "X-Pad-{i}: v\r\n");
        }
        msg.push_str("\r\nbody");
        let err = StalwartParser::parse(msg.as_bytes()).unwrap_err();
        assert!(matches!(
            err,
            KestrelError::ParseLimit {
                kind: LimitKind::HeaderCount,
                ..
            }
        ));
    }

    #[test]
    fn header_line_limit_trips() {
        let mut msg = String::from("X-Big: ");
        msg.push_str(&"a".repeat(MAX_HEADER_LINE_BYTES + 1));
        msg.push_str("\r\n\r\nbody");
        let err = StalwartParser::parse(msg.as_bytes()).unwrap_err();
        assert_eq!(err.kind(), "parse.limit");
    }

    #[test]
    fn nesting_depth_limit_trips() {
        // Build nested multipart/mixed deeper than the cap.
        let open = |msg: &mut String, depth: usize| {
            let _ = write!(
                msg,
                "--b{depth}\r\nContent-Type: multipart/mixed; boundary=\"b{}\"\r\n\r\n",
                depth + 1
            );
        };
        let mut msg =
            String::from("From: x@x\r\nContent-Type: multipart/mixed; boundary=\"b0\"\r\n\r\n");
        for d in 0..(MAX_NESTING_DEPTH + 4) {
            open(&mut msg, d);
        }
        msg.push_str("--innermost\r\nContent-Type: text/plain\r\n\r\ndeep\r\n");
        for d in (0..(MAX_NESTING_DEPTH + 4)).rev() {
            let _ = write!(msg, "--b{}--\r\n", d + 1);
        }
        let err = StalwartParser::parse(msg.as_bytes()).unwrap_err();
        assert!(matches!(
            err,
            KestrelError::ParseLimit {
                kind: LimitKind::NestingDepth,
                ..
            }
        ));
    }

    #[test]
    fn nested_rfc822_is_reparsed() {
        let inner =
            "From: inner@x\r\nSubject: inner subject\r\nMessage-ID: <inner@x>\r\n\r\ninner body";
        let outer = format!(
            "From: outer@x\r\nSubject: outer\r\nMessage-ID: <outer@x>\r\nContent-Type: message/rfc822\r\n\r\n{inner}"
        );
        let m = StalwartParser::parse(outer.as_bytes()).unwrap();
        assert_eq!(m.parts.len(), 1);
        match &m.parts[0].content {
            PartContent::Nested(n) => {
                assert_eq!(n.subject.as_deref(), Some("inner subject"));
                assert_eq!(n.text_body.as_deref(), Some("inner body"));
            }
            other => panic!("expected nested, got {other:?}"),
        }
    }

    #[test]
    fn attachment_metadata_extracted() {
        let msg = "From: x@x\r\nSubject: att\r\nContent-Type: multipart/mixed; boundary=\"b\"\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\nsee attached\r\n--b\r\nContent-Type: application/pdf; name=\"doc.pdf\"\r\nContent-Disposition: attachment; filename=\"doc.pdf\"\r\nContent-Transfer-Encoding: base64\r\n\r\nJVBERi0xLjQ=\r\n--b--\r\n";
        let m = StalwartParser::parse(msg.as_bytes()).unwrap();
        let att = m
            .parts
            .iter()
            .find(|p| p.disposition.as_deref() == Some("attachment"))
            .unwrap();
        assert_eq!(att.mime_type, "application/pdf");
        assert_eq!(att.filename.as_deref(), Some("doc.pdf"));
        assert!(matches!(att.content, PartContent::Binary(_)));
    }

    #[test]
    fn html_to_plain_text_strips_and_decodes() {
        let text = html_to_plain_text(
            "<p>Hello &amp; welcome</p><script>evil()</script><b>bold</b>&#65;&#x42;",
        );
        assert!(text.contains("Hello & welcome"));
        assert!(text.contains("bold"));
        assert!(
            !text.contains("evil()"),
            "script content must be dropped: {text}"
        );
        assert!(text.contains("AB"));
    }

    #[test]
    fn text_for_index_prefers_plain() {
        let m = ParsedMessage {
            text_body: Some("plain".into()),
            html_body: Some("<p>html</p>".into()),
            ..ParsedMessage::default()
        };
        assert_eq!(text_for_index(&m), "plain");
        let html_only = ParsedMessage {
            html_body: Some("<p>html</p>".into()),
            ..ParsedMessage::default()
        };
        assert_eq!(text_for_index(&html_only), "html");
    }

    fn build_nested_multipart(depth: usize) -> String {
        let mut parts = Vec::new();
        for i in 0..depth {
            parts.push(format!(
                "--b{i}\r\nContent-Type: multipart/mixed; boundary=\"b{}\"\r\n\r\n",
                i + 1
            ));
        }
        parts.push("--innermost\r\nContent-Type: text/plain\r\n\r\ndeep\r\n".to_string());
        for i in (0..depth).rev() {
            parts.push(format!("--b{i}--\r\n"));
        }
        let body: String = parts.concat();
        format!("From: x@x\r\nContent-Type: multipart/mixed; boundary=\"b0\"\r\n\r\n{body}")
    }

    fn build_excessive_headers(count: usize) -> String {
        let mut headers = String::new();
        for i in 0..count {
            use std::fmt::Write;
            let _ = write!(headers, "X-Pad-{i}: {i}\r\n");
        }
        format!("From: test@test\r\n{headers}\r\nbody")
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(crate::testkit::proptest_cases()))]

        #[test]
        fn parser_nesting_limit_always_trips(depth in (65usize..=80)) {
            let msg = build_nested_multipart(depth);
            let result = StalwartParser::parse(msg.as_bytes());
            prop_assert!(result.is_err(), "expected limit error at depth {depth}");
        }

        #[test]
        fn parser_header_count_limit_always_trips(count in (1025usize..=1075)) {
            let msg = build_excessive_headers(count);
            let result = StalwartParser::parse(msg.as_bytes());
            prop_assert!(result.is_err(), "expected limit error at count {count}");
        }

        #[test]
        fn parser_never_panics_on_arbitrary_bytes(bytes in proptest::collection::vec(0u8..=255u8, 0..2048)) {
            // Must not panic
            let _ = StalwartParser::parse(&bytes);
        }
    }
}
