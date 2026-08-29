//! Fuzz target: link classifier (threat model §4.5 — never panics, always
//! returns a verdict).

#![no_main]

use kestrel_core::links::classify_link;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Split at the first NUL: href | display_text (both potentially hostile).
    if let Some(pos) = data.iter().position(|&b| b == 0) {
        let href = String::from_utf8_lossy(&data[..pos]).into_owned();
        let display = String::from_utf8_lossy(&data[pos + 1..]).into_owned();
        let _ = classify_link(&href, &display);
    } else {
        let href = String::from_utf8_lossy(data).into_owned();
        let _ = classify_link(&href, "");
    }
});
