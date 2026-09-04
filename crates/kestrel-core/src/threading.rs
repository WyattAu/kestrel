//! JWZ-lite threading (schema.md §3.4), delegated to `mailkit::threading`
//! (extracted crate). Groups by normalized `Message-ID` chain
//! (`in_reply_to` + `references`), falls back to normalized-subject
//! grouping within a ±7-day window.
//!
//! Threading runs inside the ingestion transaction; a message's thread
//! assignment is immutable once written (re-threading only on
//! `UIDVALIDITY` reconciliation). Algorithm coverage (unit + property
//! tests) lives upstream in `mailkit`.

pub use mailkit::threading::{
    SUBJECT_WINDOW_MS, ThreadAssignment, ThreadInput, normalize_subject, thread_messages,
};
