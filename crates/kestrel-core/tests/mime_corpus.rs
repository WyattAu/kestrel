//! Corpus test: every file under `tests/mime-corpus/` must parse
//! deterministically without panicking (docs/testing-strategy.md §2).
//! Charset cases additionally assert transcoded output.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use kestrel_core::{
    error::{KestrelError, LimitKind},
    mime::{MimeParser, PartContent, StalwartParser},
    testkit::load_mime_corpus,
};

#[test]
fn corpus_parses_without_panic_and_deterministically() {
    let corpus = load_mime_corpus();
    assert!(
        corpus.len() >= 15,
        "corpus must cover all groups (testing-strategy §2); found {}",
        corpus.len()
    );
    for (name, bytes) in &corpus {
        let first = StalwartParser::parse(bytes);
        let second = StalwartParser::parse(bytes);
        match (&first, &second) {
            (Ok(a), Ok(b)) => {
                assert_eq!(a.subject, b.subject, "{name}: nondeterministic subject");
                assert_eq!(
                    a.parts.len(),
                    b.parts.len(),
                    "{name}: nondeterministic parts"
                );
            }
            (Err(a), Err(b)) => {
                assert_eq!(a, b, "{name}: nondeterministic error");
            }
            _ => panic!("{name}: nondeterministic outcome (Ok vs Err)"),
        }
    }
}

#[test]
fn corpus_charset_transcoding_is_correct() {
    let corpus = load_mime_corpus();
    let get = |needle: &str| {
        corpus
            .iter()
            .find(|(name, _)| name.contains(needle))
            .unwrap_or_else(|| panic!("missing {needle} in corpus"))
            .1
            .clone()
    };
    let latin1 = StalwartParser::parse(&get("latin1.eml")).unwrap();
    let body = latin1.text_body.unwrap_or_default();
    assert!(body.contains("café"), "latin1 transcoded: {body:?}");
    assert!(body.contains("naïve résumé"));

    let sjis = StalwartParser::parse(&get("shiftjis.eml")).unwrap();
    let body = sjis.text_body.unwrap_or_default();
    assert_eq!(body.trim_end(), "こんにちは");

    let gb = StalwartParser::parse(&get("gb2312.eml")).unwrap();
    assert_eq!(gb.text_body.unwrap_or_default().trim_end(), "你好");

    let utf16 = StalwartParser::parse(&get("utf16le.eml")).unwrap();
    assert_eq!(
        utf16.text_body.unwrap_or_default().trim_end(),
        "héllo wörld ✓"
    );

    let iso15 = StalwartParser::parse(&get("iso885915.eml")).unwrap();
    assert_eq!(
        iso15.text_body.unwrap_or_default().trim_end(),
        "le prix est de 5 €"
    );

    // Quoted charset parameter with a trailing attribute must still decode.
    let quoted = StalwartParser::parse(&get("quoted-charset-trailing-attr.eml")).unwrap();
    assert!(quoted.text_body.unwrap_or_default().contains("café"));
}

#[test]
fn corpus_rfc2047_mixed_words_decode_gracefully() {
    let corpus = load_mime_corpus();
    let (_, bytes) = corpus
        .iter()
        .find(|(n, _)| n.contains("adjacent-encoded-words-mixed-charsets.eml"))
        .unwrap_or_else(|| panic!("missing adjacent-encoded-words in corpus"))
        .to_owned();
    let parsed = StalwartParser::parse(&bytes).unwrap();
    // Adjacent encoded words in different charsets must both decode:
    // latin-1 caf=E9 -> "café", utf-8 IMmg -> " é" (space + U+00E9).
    let subject = parsed.subject.unwrap_or_default();
    assert!(
        subject.starts_with("café"),
        "latin-1 word decoded: {subject:?}"
    );
    assert!(subject.contains('é'), "utf-8 word decoded: {subject:?}");
    assert!(subject.contains('!'), "trailing literal kept: {subject:?}");
}

#[test]
fn corpus_ambiguous_messages_are_listable() {
    let corpus = load_mime_corpus();
    for (name, bytes) in corpus.iter().filter(|(n, _)| n.starts_with("ambiguous/")) {
        let parsed = StalwartParser::parse(bytes);
        assert!(
            parsed.is_ok(),
            "{name} must remain listable (degraded view), got {parsed:?}"
        );
    }
}

#[test]
fn corpus_nesting_valid_depths_stay_under_limit() {
    let corpus = load_mime_corpus();
    for (name, bytes) in corpus.iter().filter(|(n, _)| n.starts_with("nesting/")) {
        // The over-cap chain fixture asserts the *limit*, not success.
        if name.contains("over-cap") || name.contains("chain-80") {
            continue;
        }
        let parsed = StalwartParser::parse(bytes);
        assert!(
            parsed.is_ok(),
            "{name} (valid depth) must parse: {parsed:?}"
        );
    }
}

// --------------------------------------------------------------- rfc822

/// Regression (issue #14): `message/rfc822` shells used to restart the walk
/// at depth 0 per attachment, so a chain of nested forwarded messages never
/// hit the 64-level cap and the parser recursed until the stack overflowed
/// (SIGABRT on a ~440 KB email). The cap must accumulate across shells.
#[test]
fn regression_14_deep_rfc822_chain_hits_depth_cap_not_stack_overflow() {
    // Build a 5,000-level rfc822 chain in-memory (~440 KB, like the
    // reported crash input) instead of committing a large fixture.
    let mut msg = String::from(
        "From: leaf@example.org\r\nSubject: leaf body\r\n\r\nThe original message.\r\n",
    );
    for i in 0..5_000 {
        msg = format!(
            "From: fwd@example.org\r\nSubject: Fwd: level {i}\r\n\
             Content-Type: message/rfc822\r\n\r\n{msg}"
        );
    }
    let result = StalwartParser::parse(msg.as_bytes());
    match result {
        Err(KestrelError::ParseLimit { kind, .. }) => {
            assert_eq!(kind, LimitKind::NestingDepth, "must trip the depth cap");
        }
        other => panic!("deep rfc822 chain must fail with NestingDepth, got {other:?}"),
    }
}

#[test]
fn corpus_rfc822_chains_parse_with_nested_subject() {
    let corpus = load_mime_corpus();
    let get = |needle: &str| {
        corpus
            .iter()
            .find(|(name, _)| name.contains(needle))
            .unwrap_or_else(|| panic!("missing {needle} in corpus"))
            .1
            .clone()
    }; // A 3-level forward chain: the innermost message stays reachable.
    let fwd = StalwartParser::parse(&get("rfc822-forward.eml")).unwrap();
    let mut innermost = &fwd;
    while let Some(nested) = innermost.parts.iter().find_map(|p| match &p.content {
        PartContent::Nested(n) => Some(&**n),
        _ => None,
    }) {
        innermost = nested;
    }
    assert_eq!(innermost.subject.as_deref(), Some("leaf body"));

    // A 63-level chain sits one shell under the cap and must still parse.
    assert!(StalwartParser::parse(&get("rfc822-chain-63.eml")).is_ok());
}

#[test]
fn corpus_rfc822_chain_over_cap_fails_with_depth_limit() {
    let corpus = load_mime_corpus();
    let (_, bytes) = corpus
        .iter()
        .find(|(n, _)| n.contains("rfc822-chain-80.eml"))
        .unwrap_or_else(|| panic!("missing rfc822-chain-80 in corpus"))
        .to_owned();
    match StalwartParser::parse(&bytes) {
        Err(KestrelError::ParseLimit { kind, .. }) => {
            assert_eq!(kind, LimitKind::NestingDepth);
        }
        other => panic!("80-level rfc822 chain must trip NestingDepth, got {other:?}"),
    }
}

// ------------------------------------------------------- multipart/related

#[test]
fn corpus_related_keeps_cid_parts_resolvable() {
    let corpus = load_mime_corpus();
    let (_, bytes) = corpus
        .iter()
        .find(|(n, _)| n.contains("related-inline-image.eml"))
        .unwrap_or_else(|| panic!("missing related-inline-image in corpus"))
        .to_owned();
    let parsed = StalwartParser::parse(&bytes).unwrap();
    let png = parsed
        .parts
        .iter()
        .find(|p| p.mime_type == "image/png")
        .unwrap_or_else(|| panic!("png part must be listed"));
    assert_eq!(png.content_id.as_deref(), Some("logo@example.org"));
    assert_eq!(png.disposition.as_deref(), Some("inline"));
    assert_eq!(png.filename.as_deref(), Some("logo.png"));
    // The referencing html body survives so cid: links can resolve.
    let html = parsed.html_body.unwrap_or_default();
    assert!(html.contains("cid:logo@example.org"), "html: {html:?}");

    // The structurally-broken variant (bogus type param, no html root)
    // must remain listable, not fatal.
    let (_, broken) = corpus
        .iter()
        .find(|(n, _)| n.contains("related-no-html-root.eml"))
        .unwrap()
        .to_owned();
    assert!(StalwartParser::parse(&broken).is_ok());
}

// ------------------------------------------------------------------- bombs

#[test]
fn corpus_bombs_stay_inert_at_the_parser_boundary() {
    let corpus = load_mime_corpus();
    for (name, bytes) in corpus.iter().filter(|(n, _)| n.starts_with("bombs/")) {
        // The parser never inflates compressed payloads, so a zlib bomb
        // must decode to (at most) its compressed size and stay far below
        // the part cap — the hostile expansion would only happen in a
        // consumer that unpacks it, which the parser must not be.
        let parsed = StalwartParser::parse(bytes)
            .unwrap_or_else(|e| panic!("{name} must not panic or hard-fail: {e:?}"));
        let max_decoded = parsed
            .parts
            .iter()
            .map(|p| p.decoded_size)
            .max()
            .unwrap_or(0);
        assert!(
            max_decoded <= 1024 * 1024,
            "{name}: largest decoded part {max_decoded} bytes — bomb was inflated"
        );
    }
}
