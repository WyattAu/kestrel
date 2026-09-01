//! Fuzz target: IMAP response parsing via `imap-next` Client.
//!
//! Feeds arbitrary bytes into the sans-I/O IMAP client state machine to find
//! panics or crashes in the response parser (threat model §4 — no panics on
//! hostile input).

#![no_main]

use imap_next::{
    Interrupt, Io, State,
    client::{Client, Event, Options},
};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut client = Client::new(Options::default());
    client.enqueue_input(data);

    // Drive the state machine until it needs more input, errors, or we've
    // processed a reasonable number of events to avoid infinite loops.
    for _ in 0..100 {
        match client.next() {
            Ok(Event::GreetingReceived { .. }) => {}
            Ok(Event::DataReceived { .. }) => {}
            Ok(Event::StatusReceived { .. }) => {}
            Ok(Event::ContinuationRequestReceived { .. }) => {}
            Ok(_) => {}
            Err(Interrupt::Io(Io::NeedMoreInput)) => break,
            Err(Interrupt::Io(Io::Output(_))) => {}
            Err(Interrupt::Error(_)) => break,
        }
    }
});
