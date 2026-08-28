//! RFC 5322 message building for composition (architecture §4.2): drafts
//! written in Markdown become `multipart/alternative` (plain + basic HTML)
//! payloads; attachments wrap in `multipart/mixed`.

use std::fmt::Write as _;

use crate::{
    clock::Clock,
    error::KestrelError,
    ids::IdGenerator,
    protocol::{Address, Draft},
};

/// Builds the raw RFC 5322 bytes for a draft.
///
/// `Bcc` recipients are carried in the envelope (outbox row) and the SMTP
/// `RCPT TO` set, but the header itself is omitted from the wire message —
/// standard Bcc hygiene.
///
/// # Errors
/// [`KestrelError::DraftInvalid`] for empty recipients/sender or invalid
/// addresses.
pub fn build_rfc5322(
    draft: &Draft,
    ids: &dyn IdGenerator,
    clock: &dyn Clock,
) -> Result<Vec<u8>, KestrelError> {
    if draft.from.email.trim().is_empty() {
        return Err(KestrelError::DraftInvalid {
            detail: "empty From".to_string(),
        });
    }
    if draft.to.is_empty() && draft.cc.is_empty() && draft.bcc.is_empty() {
        return Err(KestrelError::DraftInvalid {
            detail: "no recipients".to_string(),
        });
    }
    for addr in draft.to.iter().chain(&draft.cc).chain(&draft.bcc) {
        if !addr.email.contains('@') || addr.email.trim() != addr.email {
            return Err(KestrelError::DraftInvalid {
                detail: format!("invalid address: {}", addr.email),
            });
        }
    }

    let boundary_alt = boundary(ids);
    let boundary_mix = boundary(ids);
    let message_id = format!("<{}@kestrel>", ids.next_id());
    let date = rfc5322_date(clock.now_unix_ms());

    let mut out = String::with_capacity(draft.body_markdown.len() + 1024);
    let _ = write!(out, "From: {}\r\n", format_address(&draft.from));
    if !draft.to.is_empty() {
        let joined = draft
            .to
            .iter()
            .map(format_address)
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(out, "To: {joined}");
    }
    if !draft.cc.is_empty() {
        let joined = draft
            .cc
            .iter()
            .map(format_address)
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(out, "Cc: {joined}");
    }
    let _ = writeln!(out, "Subject: {}", encode_header(&draft.subject));
    let _ = writeln!(out, "Date: {date}");
    let _ = writeln!(out, "Message-ID: {message_id}");
    if let Some(irt) = &draft.in_reply_to {
        let _ = writeln!(out, "In-Reply-To: <{irt}>");
    }
    if !draft.references.is_empty() {
        let refs = draft
            .references
            .iter()
            .map(|r| format!("<{r}>"))
            .collect::<Vec<_>>()
            .join(" ");
        let _ = writeln!(out, "References: {refs}");
    }
    out.push_str("MIME-Version: 1.0\r\n");

    let html = markdown_to_html(&draft.body_markdown);
    if draft.attachments.is_empty() {
        let _ = write!(
            out,
            "Content-Type: multipart/alternative; boundary=\"{boundary_alt}\"\r\n\r\n\
             --{boundary_alt}\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{}\r\n\
             --{boundary_alt}\r\nContent-Type: text/html; charset=utf-8\r\n\r\n{}\r\n\
             --{boundary_alt}--\r\n",
            normalize_newlines(&draft.body_markdown),
            html
        );
    } else {
        let _ = write!(
            out,
            "Content-Type: multipart/mixed; boundary=\"{boundary_mix}\"\r\n\r\n\
             --{boundary_mix}\r\nContent-Type: multipart/alternative; boundary=\"{boundary_alt}\"\r\n\r\n\
             --{boundary_alt}\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{}\r\n\
             --{boundary_alt}\r\nContent-Type: text/html; charset=utf-8\r\n\r\n{}\r\n\
             --{boundary_alt}--\r\n\
             --{boundary_mix}\r\n",
            normalize_newlines(&draft.body_markdown),
            html
        );
        for att in &draft.attachments {
            let b64 = base64_wrap(&att.data);
            let name = att.name.replace(['"', '\r', '\n'], "");
            let _ = write!(
                out,
                "--{boundary_mix}\r\nContent-Type: {}\r\nContent-Disposition: attachment; filename=\"{name}\"\r\nContent-Transfer-Encoding: base64\r\n\r\n{b64}\r\n",
                att.mime_type
            );
        }
        let _ = write!(out, "--{boundary_mix}--\r\n");
    }

    Ok(out.into_bytes())
}

/// Renders Markdown to a minimal styled HTML document (requirements §5:
/// "clean multipart/alternative ... Plaintext + basic HTML").
#[must_use]
pub fn markdown_to_html(markdown: &str) -> String {
    use pulldown_cmark::{Options, Parser, html};
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    let parser = Parser::new_ext(markdown, opts);
    let mut body = String::new();
    html::push_html(&mut body, parser);
    format!(
        "<!DOCTYPE html>\r\n<html><head><meta charset=\"utf-8\"><style>body{{font-family:sans-serif;margin:0.5em}}blockquote{{border-left:3px solid #999;margin:0.5em 0;padding-left:0.6em;color:#555}}pre{{background:#f4f4f4;padding:0.5em;overflow-x:auto}}code{{background:#f4f4f4;padding:0 0.2em}}</style></head><body>{body}</body></html>\r\n"
    )
}

fn boundary(ids: &dyn IdGenerator) -> String {
    format!("=_kestrel-{}", ids.next_id().simple())
}

fn normalize_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\n', "\r\n")
}

/// RFC 2047 encoded-word when the header value is not pure ASCII.
fn encode_header(value: &str) -> String {
    if value.is_ascii() && !value.contains(['\r', '\n']) {
        return value.replace(['\r', '\n'], " ");
    }
    // B-encoding over UTF-8, wrapped per RFC 2047 (75-char line limit is
    // handled by generators tolerant to long encoded words; we chunk at 60).
    let b64 = base64_impl::encode(value.as_bytes());
    let mut out = String::new();
    let mut rest = b64.as_str();
    // Single word when short enough.
    if rest.len() <= 60 {
        return format!("=?utf-8?B?{rest}?=");
    }
    while !rest.is_empty() {
        let take = rest.len().min(60);
        let (chunk, tail) = rest.split_at(take);
        if !out.is_empty() {
            out.push_str("\r\n ");
        }
        let _ = write!(out, "=?utf-8?B?{chunk}?=");
        rest = tail;
    }
    out
}

fn format_address(addr: &Address) -> String {
    match &addr.name {
        Some(name) if !name.trim().is_empty() => {
            let needs_quote = name
                .chars()
                .any(|c| !c.is_ascii_alphanumeric() && !" !#$%&'*+-/=?^_`{|}~.".contains(c));
            if needs_quote {
                format!("\"{}\" <{}>", name.replace('"', "\\\""), addr.email)
            } else {
                format!("{name} <{}>", addr.email)
            }
        }
        _ => addr.email.clone(),
    }
}

fn rfc5322_date(unix_ms: i64) -> String {
    // Civil-time conversion (Howard Hinnant's algorithm); no chrono needed.
    let secs = unix_ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400);
    let (hours, mins, secs) = (
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60,
    );
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    // Day of week: 1970-01-01 was a Thursday (4).
    let dow = (days.rem_euclid(7) + 4) % 7;
    let dow_names: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    let month_names: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    format!(
        "{}, {:02} {} {:04} {:02}:{:02}:{:02} +0000",
        dow_names[usize::try_from(dow).unwrap_or(0) % 7],
        d,
        month_names[usize::try_from(month - 1).unwrap_or(0) % 12],
        year,
        hours,
        mins,
        secs
    )
}

/// Minimal base64 (standard alphabet, padded) — avoids a dependency for the
/// one call site; verified against vectors below.
mod base64_impl {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    pub(super) fn encode(data: &[u8]) -> String {
        let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
        for chunk in data.chunks(3) {
            let b = [
                chunk[0],
                chunk.get(1).copied().unwrap_or(0),
                chunk.get(2).copied().unwrap_or(0),
            ];
            let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
            out.push(ALPHABET[(n >> 18) as usize & 63] as char);
            out.push(ALPHABET[(n >> 12) as usize & 63] as char);
            out.push(if chunk.len() > 1 {
                ALPHABET[(n >> 6) as usize & 63] as char
            } else {
                '='
            });
            out.push(if chunk.len() > 2 {
                ALPHABET[n as usize & 63] as char
            } else {
                '='
            });
        }
        out
    }
}

/// Wraps base64 into 76-char lines with CRLF (RFC 5322 line discipline).
fn base64_wrap(data: &[u8]) -> String {
    let encoded = base64_impl::encode(data);
    let mut out = String::with_capacity(encoded.len() + encoded.len() / 76 * 2);
    for (i, c) in encoded.chars().enumerate() {
        if i > 0 && i % 76 == 0 {
            out.push_str("\r\n");
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::sync::Arc;

    use super::*;
    use crate::{mime::MimeParser as _, protocol::DraftAttachment, testkit::SequentialIds};

    fn draft(body: &str) -> Draft {
        Draft {
            account: crate::ids::AccountId::from_uuid(uuid::Uuid::now_v7()),
            from: Address {
                name: Some("Alice".into()),
                email: "alice@example.org".into(),
            },
            to: vec![Address::bare("bob@example.net")],
            cc: vec![],
            bcc: vec![],
            subject: "Hello".into(),
            in_reply_to: None,
            references: vec![],
            body_markdown: body.into(),
            attachments: vec![],
        }
    }

    struct FixedClock;
    impl Clock for FixedClock {
        fn now_unix_ms(&self) -> i64 {
            1_750_000_000_000
        }
    }

    #[test]
    fn builds_multipart_alternative() {
        let ids = SequentialIds::new();
        let raw = build_rfc5322(&draft("**hi** _there_"), &ids, &FixedClock).unwrap();
        let text = String::from_utf8(raw).unwrap();
        assert!(text.contains("multipart/alternative"));
        assert!(text.contains("From: Alice <alice@example.org>"));
        assert!(text.contains("To: bob@example.net"));
        assert!(text.contains("Subject: Hello"));
        assert!(text.contains("Date: "));
        assert!(text.contains("text/plain"));
        assert!(text.contains("text/html"));
        assert!(text.contains("<strong>hi</strong>"));
        assert!(text.contains("<em>there</em>"));
        // Round-trips through the parser.
        let parsed = crate::mime::StalwartParser::parse(text.as_bytes()).unwrap();
        assert_eq!(parsed.subject.as_deref(), Some("Hello"));
        assert!(parsed.text_body.unwrap_or_default().contains("hi"));
        assert!(parsed.html_body.unwrap_or_default().contains("<strong>"));
    }

    #[test]
    fn attachments_wrap_in_mixed() {
        let ids = SequentialIds::new();
        let mut d = draft("see file");
        d.attachments.push(DraftAttachment {
            name: "notes.txt".into(),
            mime_type: "text/plain".into(),
            data: b"attachment payload".to_vec(),
        });
        let raw = build_rfc5322(&d, &ids, &FixedClock).unwrap();
        let text = String::from_utf8(raw).unwrap();
        assert!(text.contains("multipart/mixed"));
        assert!(text.contains("filename=\"notes.txt\""));
        assert!(text.contains("YXR0YWNobWVudCBwYXlsb2Fk")); // base64 of payload
        let parsed = crate::mime::StalwartParser::parse(text.as_bytes()).unwrap();
        assert!(
            parsed
                .parts
                .iter()
                .any(|p| p.disposition.as_deref() == Some("attachment"))
        );
    }

    #[test]
    fn non_ascii_subject_is_encoded_word() {
        let ids = SequentialIds::new();
        let mut d = draft("body");
        d.subject = "Grüße aus München".into();
        let raw = build_rfc5322(&d, &ids, &FixedClock).unwrap();
        let text = String::from_utf8(raw).unwrap();
        assert!(text.contains("=?utf-8?B?"), "{text}");
        let parsed = crate::mime::StalwartParser::parse(text.as_bytes()).unwrap();
        assert_eq!(parsed.subject.as_deref(), Some("Grüße aus München"));
    }

    #[test]
    fn reply_headers_present() {
        let ids = SequentialIds::new();
        let mut d = draft("reply body");
        d.in_reply_to = Some("orig@x".into());
        d.references = vec!["first@x".into(), "orig@x".into()];
        let raw = build_rfc5322(&d, &ids, &FixedClock).unwrap();
        let text = String::from_utf8(raw).unwrap();
        assert!(text.contains("In-Reply-To: <orig@x>"));
        assert!(text.contains("References: <first@x> <orig@x>"));
    }

    #[test]
    fn invalid_drafts_rejected() {
        let ids = SequentialIds::new();
        let mut d = draft("x");
        d.to = vec![];
        d.cc = vec![];
        d.bcc = vec![];
        assert!(build_rfc5322(&d, &ids, &FixedClock).is_err());
        let mut bad_addr = draft("x");
        bad_addr.to = vec![Address::bare("not-an-address")];
        assert!(build_rfc5322(&bad_addr, &ids, &FixedClock).is_err());
    }

    #[test]
    fn date_format_is_rfc5322() {
        // 1970-01-01T00:00:00Z is a Thursday.
        assert_eq!(rfc5322_date(0), "Thu, 01 Jan 1970 00:00:00 +0000");
        // 2026-08-28T12:34:56Z → Friday.
        assert_eq!(
            rfc5322_date(1_787_920_496_000),
            "Fri, 28 Aug 2026 12:34:56 +0000"
        );
    }

    #[test]
    fn base64_vectors() {
        assert_eq!(base64_impl::encode(b""), "");
        assert_eq!(base64_impl::encode(b"f"), "Zg==");
        assert_eq!(base64_impl::encode(b"fo"), "Zm8=");
        assert_eq!(base64_impl::encode(b"foo"), "Zm9v");
        assert_eq!(base64_impl::encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_impl::encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_impl::encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn boundary_uniqueness() {
        let ids = Arc::new(SequentialIds::new());
        let a = boundary(ids.as_ref());
        let b = boundary(ids.as_ref());
        assert_ne!(a, b);
        assert!(a.starts_with("=_kestrel-"));
    }
}
