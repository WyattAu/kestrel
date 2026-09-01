//! RFC 5322 message building for composition (architecture §4.2): drafts
//! written in Markdown become `multipart/alternative` (plain + basic HTML)
//! payloads; attachments wrap in `multipart/mixed`.

use std::fmt::Write as _;

use crate::{
    clock::Clock,
    error::KestrelError,
    ids::IdGenerator,
    protocol::{Address, Draft, Priority},
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
    write_priority_header(&mut out, draft.priority);
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

/// Builds RFC 5322 message with optional `OpenPGP` sign/encrypt wrapping
/// (RFC 3156 / PGP/MIME).
///
/// The raw RFC 5322 bytes are produced by [`build_rfc5322`]. When `sign_fn` is
/// provided the message is signed; when `encrypt_fn` is provided the message
/// (optionally signed first) is encrypted. The result is wrapped in the
/// appropriate MIME content type:
///
/// * **Sign only** → `multipart/signed` (RFC 3156 §5)
/// * **Encrypt only** → `multipart/encrypted` (RFC 3156 §4)
/// * **Both** → sign first, then encrypt → `multipart/encrypted`
/// * **Neither** → raw RFC 5322 bytes returned unchanged
///
/// `sign_fn` receives the raw RFC 5322 bytes and must return the
/// signature data (e.g. an armored detached signature).
///
/// `encrypt_fn` receives the plaintext to encrypt (raw RFC 5322 bytes
/// when unsigned, or the signed output when both flags are set) and must
/// return the armored PGP ciphertext.
///
/// # Errors
/// [`KestrelError::DraftInvalid`] when the draft fails validation or a
/// PGP operation produces invalid UTF-8.
pub fn build_rfc5322_pgp<FSign, FEncrypt>(
    draft: &Draft,
    ids: &dyn IdGenerator,
    clock: &dyn Clock,
    sign_fn: Option<FSign>,
    encrypt_fn: Option<FEncrypt>,
) -> Result<Vec<u8>, KestrelError>
where
    FSign: FnOnce(&[u8]) -> Result<Vec<u8>, KestrelError>,
    FEncrypt: FnOnce(&[u8]) -> Result<Vec<u8>, KestrelError>,
{
    let raw = build_rfc5322(draft, ids, clock)?;

    let (data, was_signed) = if let Some(sign) = sign_fn {
        (sign(&raw)?, true)
    } else {
        (raw.clone(), false)
    };

    if let Some(encrypt) = encrypt_fn {
        let encrypted = encrypt(&data)?;
        return wrap_multipart_encrypted(encrypted, ids);
    }

    if was_signed {
        return wrap_multipart_signed(raw, data, ids);
    }

    Ok(raw)
}

fn wrap_multipart_encrypted(
    pgp_data: Vec<u8>,
    ids: &dyn IdGenerator,
) -> Result<Vec<u8>, KestrelError> {
    let boundary = boundary(ids);
    let pgp_str = String::from_utf8(pgp_data).map_err(|e| KestrelError::DraftInvalid {
        detail: format!("PGP output not valid UTF-8: {e}"),
    })?;

    let mut out = String::with_capacity(512 + pgp_str.len());
    let _ = write!(
        out,
        "Content-Type: multipart/encrypted; protocol=\"application/pgp-encrypted\"; \
         boundary=\"{boundary}\"\r\n\
         MIME-Version: 1.0\r\n\r\n\
         --{boundary}\r\n\
         Content-Type: application/pgp-encrypted\r\n\r\n\
         Version: 1\r\n\r\n\
         --{boundary}\r\n\
         Content-Type: application/octet-stream\r\n\r\n\
         {pgp_str}\
         --{boundary}--\r\n"
    );

    Ok(out.into_bytes())
}

fn wrap_multipart_signed(
    message: Vec<u8>,
    signature: Vec<u8>,
    ids: &dyn IdGenerator,
) -> Result<Vec<u8>, KestrelError> {
    let boundary = boundary(ids);
    let message_str = String::from_utf8(message).map_err(|e| KestrelError::DraftInvalid {
        detail: format!("message not valid UTF-8: {e}"),
    })?;
    let sig_str = String::from_utf8(signature).map_err(|e| KestrelError::DraftInvalid {
        detail: format!("signature not valid UTF-8: {e}"),
    })?;

    let mut out = String::with_capacity(512 + message_str.len() + sig_str.len());
    let _ = write!(
        out,
        "Content-Type: multipart/signed; micalg=pgp-sha256; \
         protocol=\"application/pgp-signature\"; boundary=\"{boundary}\"\r\n\
         MIME-Version: 1.0\r\n\r\n\
         --{boundary}\r\n\
         {message_str}\
         --{boundary}\r\n\
         Content-Type: application/pgp-signature\r\n\r\n\
         {sig_str}\
         --{boundary}--\r\n"
    );

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

fn write_priority_header(out: &mut String, priority: Priority) {
    match priority {
        Priority::High => {
            let _ = writeln!(out, "X-Priority: 1 (Highest)");
        }
        Priority::Normal => {
            let _ = writeln!(out, "X-Priority: 3 (Normal)");
        }
        Priority::Low => {
            let _ = writeln!(out, "X-Priority: 5 (Lowest)");
        }
    }
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

    type PgpFn = fn(&[u8]) -> Result<Vec<u8>, KestrelError>;

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
            pgp_sign: false,
            pgp_encrypt: false,
            smime_sign: false,
            smime_encrypt: false,
            send_after: None,
            priority: Priority::Normal,
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

    #[test]
    fn pgp_signed_only() {
        let ids = SequentialIds::new();
        let d = draft("signed body");
        let raw = build_rfc5322_pgp(
            &d,
            &ids,
            &FixedClock,
            Some(|_data: &[u8]| -> Result<Vec<u8>, KestrelError> {
                Ok(
                    b"-----BEGIN PGP SIGNATURE-----\nfakesig\n-----END PGP SIGNATURE-----\n"
                        .to_vec(),
                )
            }),
            None::<PgpFn>,
        )
        .unwrap();
        let text = String::from_utf8(raw).unwrap();
        assert!(
            text.contains("multipart/signed"),
            "should be multipart/signed"
        );
        assert!(
            text.contains("protocol=\"application/pgp-signature\""),
            "must declare PGP signature protocol"
        );
        assert!(text.contains("fakesig"));
        assert!(text.contains("signed body"));
    }

    #[test]
    fn pgp_encrypted_only() {
        let ids = SequentialIds::new();
        let d = draft("encrypted body");
        let raw = build_rfc5322_pgp(
            &d,
            &ids,
            &FixedClock,
            None::<PgpFn>,
            Some(|_data: &[u8]| -> Result<Vec<u8>, KestrelError> {
                Ok(
                    b"-----BEGIN PGP MESSAGE-----\nfakecipher\n-----END PGP MESSAGE-----\n"
                        .to_vec(),
                )
            }),
        )
        .unwrap();
        let text = String::from_utf8(raw).unwrap();
        assert!(
            text.contains("multipart/encrypted"),
            "should be multipart/encrypted"
        );
        assert!(
            text.contains("protocol=\"application/pgp-encrypted\""),
            "must declare PGP encrypted protocol"
        );
        assert!(text.contains("Version: 1"));
        assert!(text.contains("fakecipher"));
    }

    #[test]
    fn pgp_sign_and_encrypt() {
        let ids = SequentialIds::new();
        let d = draft("sign-then-encrypt body");
        let raw = build_rfc5322_pgp(
            &d,
            &ids,
            &FixedClock,
            Some(|_data: &[u8]| -> Result<Vec<u8>, KestrelError> {
                Ok(
                    b"-----BEGIN PGP SIGNATURE-----\nfakesig\n-----END PGP SIGNATURE-----\n"
                        .to_vec(),
                )
            }),
            Some(|data: &[u8]| -> Result<Vec<u8>, KestrelError> {
                // encrypt receives signed data
                assert!(
                    String::from_utf8_lossy(data).contains("fakesig"),
                    "encrypt should receive the signed output"
                );
                Ok(b"-----BEGIN PGP MESSAGE-----\nencrypted\n-----END PGP MESSAGE-----\n".to_vec())
            }),
        )
        .unwrap();
        let text = String::from_utf8(raw).unwrap();
        // Both sign + encrypt => outer layer is multipart/encrypted
        assert!(text.contains("multipart/encrypted"));
        assert!(text.contains("encrypted"));
        assert!(
            !text.contains("multipart/signed"),
            "no inner signed wrapper when encrypted"
        );
    }

    #[test]
    fn pgp_neither_returns_raw() {
        let ids = SequentialIds::new();
        let mut d = draft("plain body");
        d.priority = Priority::Normal;
        let raw = build_rfc5322_pgp(&d, &ids, &FixedClock, None::<PgpFn>, None::<PgpFn>).unwrap();
        let text = String::from_utf8(raw).unwrap();
        assert!(text.contains("multipart/alternative"));
        assert!(text.contains("plain body"));
    }

    #[test]
    fn priority_high_header() {
        let ids = SequentialIds::new();
        let mut d = draft("urgent body");
        d.priority = Priority::High;
        let raw = build_rfc5322(&d, &ids, &FixedClock).unwrap();
        let text = String::from_utf8(raw).unwrap();
        assert!(text.contains("X-Priority: 1 (Highest)"));
    }

    #[test]
    fn priority_normal_header() {
        let ids = SequentialIds::new();
        let mut d = draft("normal body");
        d.priority = Priority::Normal;
        let raw = build_rfc5322(&d, &ids, &FixedClock).unwrap();
        let text = String::from_utf8(raw).unwrap();
        assert!(text.contains("X-Priority: 3 (Normal)"));
    }

    #[test]
    fn priority_low_header() {
        let ids = SequentialIds::new();
        let mut d = draft("low priority body");
        d.priority = Priority::Low;
        let raw = build_rfc5322(&d, &ids, &FixedClock).unwrap();
        let text = String::from_utf8(raw).unwrap();
        assert!(text.contains("X-Priority: 5 (Lowest)"));
    }
}
