//! Fuzz target: terminal escape sanitizer (threat model M21/M22 — OSC
//! and control sequences neutralized).

#![no_main]

use kestrel_core::sanitizer::sanitize_terminal_text;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let input = String::from_utf8_lossy(data).into_owned();
    let sanitized = sanitize_terminal_text(&input);

    // Invariants:
    assert!(
        !sanitized.contains('\x1b'),
        "ESC leaked through sanitizer"
    );
    assert!(
        !sanitized.chars().any(|c| c.is_control() && c != '\t' && c != '\n' && c != '\r'),
        "C0 control leaked"
    );
});
