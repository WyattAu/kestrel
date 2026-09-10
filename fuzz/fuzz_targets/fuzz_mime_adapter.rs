//! Fuzz target: MIME adapter (threat model §4.2 — no panics on hostile
//! input, limits enforced, graceful degradation).

#![no_main]

use kestrel_core::mime::{MimeParser, StalwartParser};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // libfuzzer-sys installs an abort-on-panic hook (libfuzzer-sys 0.4.13
    // src/lib.rs:92-94), which would abort the process inside
    // mail-parser's `debug_assert!(false, "Invalid part ID, could not find
    // multipart")` before the adapter's `catch_unwind` boundary can
    // contain it — exactly the issue #14 CI crash. Replace the hook with
    // one that only reports: unwinding then proceeds, upstream panics are
    // contained by the adapter as typed errors, and genuine panics in
    // kestrel code still crash the run through libfuzzer-sys's outer
    // catch_unwind → abort (src/lib.rs:60-70). Panic reports stay on
    // stderr, so upstream debug-assert bugs remain visible for reporting.
    std::panic::set_hook(Box::new(|info| {
        eprintln!("fuzz: contained panic: {info}");
    }));

    // The adapter must never panic; any input yields Ok or a typed error.
    let _ = StalwartParser::parse(data);
});
