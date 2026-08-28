//! `kestrel-core` — the shared vocabulary of the Kestrel email client.
//!
//! Domain types, typed IDs ([`ids`]), the error taxonomy ([`error`],
//! ADR 0007), the frozen core↔frontend message protocol ([`protocol`],
//! `docs/message-protocol.md`), configuration ([`config`], ADR 0006), XDG
//! path resolution ([`paths`]), the clock abstraction ([`clock`],
//! architecture §8), the `MimeParser` trait and `mail-parser` adapter
//! ([`mime`], ADR 0002), the terminal/HTML sanitizers ([`sanitizer`],
//! threat model §4.4–4.6), the link classifier ([`links`], threat model
//! §4.5), the pure JWZ-lite threading algorithm ([`threading`],
//! `docs/schema.md` §3.4), and shared test fixtures ([`testkit`]).
//!
//! This crate is dependency-light by design: no UI, no storage backends, no
//! network clients (architecture §2).
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, missing_docs))]

pub mod clock;
pub mod compose;
pub mod config;
pub mod error;
pub mod ids;
pub mod links;
pub mod mime;
pub mod paths;
pub mod protocol;
pub mod sanitizer;
pub mod testkit;
pub mod threading;
