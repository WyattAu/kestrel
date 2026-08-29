//! Fuzz target: MIME adapter (threat model §4.2 — no panics on hostile
//! input, limits enforced, graceful degradation).

#![no_main]

use kestrel_core::mime::{MimeParser, StalwartParser};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // The adapter must never panic; any input yields Ok or a typed error.
    let _ = StalwartParser::parse(data);
});
