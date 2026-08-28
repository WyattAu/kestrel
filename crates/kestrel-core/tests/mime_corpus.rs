//! Corpus test: every file under `tests/mime-corpus/` must parse
//! deterministically without panicking (docs/testing-strategy.md §2).
//! Charset cases additionally assert transcoded output.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use kestrel_core::{
    mime::{MimeParser, StalwartParser},
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
        let parsed = StalwartParser::parse(bytes);
        assert!(
            parsed.is_ok(),
            "{name} (valid depth) must parse: {parsed:?}"
        );
    }
}
