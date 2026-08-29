//! `kestrel-sync` — network engines for Kestrel.
//!
//! IMAP state machine on `imap-next` (ADR 0005/0010), SMTP submission via
//! `lettre`, QRESYNC/CONDSTORE delta sync, `UIDVALIDITY` reconciliation,
//! and the outbox queue (`docs/sync-engine.md`).
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, missing_docs))]

pub mod error;
pub mod jmap;
pub mod outbox_service;
pub mod session;
pub mod smtp;
pub mod sync;

pub use error::{SyncError, SyncResult};
pub use outbox_service::OutboxService;
pub use session::{CommandOutcome, ConnectParams, ImapSession, SaslFactory, Security, Unsolicited};
pub use smtp::{SmtpParams, SmtpSecurity, submit_envelope};
pub use sync::SyncService;
