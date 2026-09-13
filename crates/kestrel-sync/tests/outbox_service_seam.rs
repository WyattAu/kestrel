//! Outbox service seam tests (sync-engine.md §6) over the real
//! storage-backed `MailStore`: transient SMTP failures schedule retries with
//! the documented backoff, retry exhaustion emits a permanent `MailFailed`,
//! a missing CAS blob is rejected without a retry, and the offline gate
//! defers flushing entirely.

#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

use std::{sync::Arc, time::Duration};

use kestrel_core::{
    clock::{Clock as _, FakeClock},
    paths::Paths,
    protocol::{Address, EngineEvent, MailProtocol, Provider},
    sasl::SaslMechanism,
    secrets::SecretString,
    store_model::OutboxEnvelope,
    testkit::{SequentialIds, temp_paths},
};
use kestrel_storage::{BlobStore, NewAccount, StorageHandle, StorageService};
use kestrel_sync::{ConnectParams, OutboxService, Security, SmtpParams, SmtpSecurity};
use tokio_rustls::TlsConnector;
use tokio_util::sync::CancellationToken;

/// SMTP params pointing at a closed local port: connection refused, mapped
/// by `smtp::submit_envelope` to `ConnectionLost` (a transient outcome).
fn unreachable_smtp() -> SmtpParams {
    SmtpParams {
        host: "127.0.0.1".into(),
        port: 1,
        username: "outbox-test".into(),
        secret: SecretString::new("not-a-secret".into()),
        secret_override: None,
        oauth2: false,
        security: SmtpSecurity::Insecure,
    }
}

/// IMAP params for the Sent-folder APPEND (never reached here: every test
/// path exits before a successful SMTP submission).
fn fixture_imap() -> ConnectParams {
    ConnectParams {
        host: "127.0.0.1".into(),
        port: 1,
        security: Security::Insecure,
        username: "outbox-test".into(),
        secret: SecretString::new("not-a-secret".into()),
        secret_override: None,
        mechanisms: vec![SaslMechanism::Plain],
        tls: TlsConnector::from(Arc::new(
            rustls::ClientConfig::builder()
                .with_root_certificates(rustls::RootCertStore::empty())
                .with_no_client_auth(),
        )),
        sasl_factory: Arc::new(|mech, user, secret| {
            kestrel_crypto::sasl::start(mech, user, secret)
        }),
    }
}

async fn setup() -> (StorageHandle, Arc<FakeClock>, tempfile::TempDir, Paths) {
    let (dir, paths) = temp_paths();
    paths.ensure().unwrap();
    let clock = Arc::new(FakeClock::new(1_700_000_000_000));
    let ids = Arc::new(SequentialIds::new());
    let (storage, _cancel) = StorageService::spawn(paths.clone(), ids.clone(), clock.clone());
    storage.list_accounts().await.expect("service opens");
    (storage, clock, dir, paths)
}

async fn enqueued_row(storage: &StorageHandle) -> kestrel_core::ids::OutboxId {
    let account = storage
        .upsert_account(NewAccount {
            name: "Outbox".into(),
            email: "outbox@x.example".into(),
            provider: Provider::Generic,
            protocol: MailProtocol::Imap,
            auth_kind: "password".into(),
            host: String::new(),
        })
        .await
        .unwrap();
    let envelope = OutboxEnvelope {
        from: Address::bare("me@x.example"),
        to: vec![Address::bare("you@y.example")],
        cc: vec![],
        bcc: vec![],
        subject: "seam".into(),
    };
    storage
        .outbox_enqueue(account, envelope, b"raw draft bytes".to_vec(), None)
        .await
        .unwrap()
}

fn service(
    storage: StorageHandle,
    clock: Arc<FakeClock>,
    online: bool,
) -> (
    OutboxService,
    tokio::sync::mpsc::Receiver<EngineEvent>,
    CancellationToken,
) {
    let (tx, rx) = tokio::sync::mpsc::channel(64);
    let online_flag = Arc::new(std::sync::atomic::AtomicBool::new(online));
    let cancel = CancellationToken::new();
    let svc = OutboxService::new(
        Arc::new(storage),
        unreachable_smtp(),
        fixture_imap(),
        clock,
        tx,
        online_flag,
    );
    (svc, rx, cancel)
}

async fn next_event(rx: &mut tokio::sync::mpsc::Receiver<EngineEvent>) -> EngineEvent {
    tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .unwrap_or_else(|_| panic!("event within 10s"))
        .expect("bus open")
}

/// Pins the documented backoff ladder (sync-engine.md §6): 30 s → 2 m →
/// 8 m → 30 m → 2 h → 6 h, capping at the 6 h rung, cycling mod 12, with
/// deterministic ±20% jitter derived from the retry counter.
#[test]
fn backoff_schedule_matches_spec() {
    // The spec ladder, restated: six distinct rungs up to 6 h, then cycling
    // mod 12 back through the schedule.
    fn spec_backoff(attempt: u32) -> Duration {
        const LADDER_MS: [u64; 12] = [
            30_000, 120_000, 480_000, 1_800_000, 7_200_000, 21_600_000, 21_600_000, 21_600_000,
            21_600_000, 21_600_000, 21_600_000, 21_600_000,
        ];
        #[allow(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss
        )]
        {
            let idx = usize::try_from(attempt.saturating_sub(1)).unwrap_or(0) % LADDER_MS.len();
            let jitter = 1.0 + f64::from(attempt % 10) / 25.0 - 0.2;
            Duration::from_millis((LADDER_MS[idx] as f64 * jitter) as u64)
        }
    }
    for attempt in 1_u32..=24 {
        assert_eq!(
            OutboxService::backoff_for(attempt),
            spec_backoff(attempt),
            "attempt {attempt}"
        );
        // Deterministic for a fixed retry counter.
        assert_eq!(
            OutboxService::backoff_for(attempt),
            OutboxService::backoff_for(attempt)
        );
    }
    // First rung: 30 s base with attempt-1 jitter.
    assert_eq!(OutboxService::backoff_for(1), Duration::from_millis(25_200));
}

/// A transient SMTP failure marks the row for retry with the backoff offset
/// and emits `OutboxRetry`; the row is not due again until the backoff
/// elapses on the injected clock.
#[tokio::test]
async fn transient_failure_schedules_retry_with_backoff() {
    let (storage, clock, _dir, _paths) = setup().await;
    let id = enqueued_row(&storage).await;
    let (svc, mut rx, cancel) = service(storage.clone(), clock.clone(), true);

    let run_cancel = cancel.clone();
    let handle = tokio::spawn(async move { svc.run(run_cancel).await });
    // The interval's first tick fires immediately, so no time travel needed.
    match next_event(&mut rx).await {
        EngineEvent::OutboxRetry {
            id: event_id,
            attempt,
            next_in,
            ..
        } => {
            assert_eq!(event_id, id);
            assert_eq!(attempt, 1);
            assert_eq!(next_in, OutboxService::backoff_for(1));
        }
        other => panic!("expected OutboxRetry, got {other:?}"),
    }

    // Backoff defers the row until the clock passes the offset.
    assert!(
        storage.outbox_due().await.unwrap().is_empty(),
        "backoff defers the row"
    );
    clock.advance(i64::try_from(OutboxService::backoff_for(1).as_millis()).unwrap() + 1);
    assert_eq!(
        storage.outbox_due().await.unwrap().len(),
        1,
        "due again after backoff"
    );

    cancel.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(10), handle).await;
}

/// The 12th transient failure exhausts the ladder: `MailFailed` with
/// `RetryExhausted` and `permanent: true`.
#[tokio::test]
async fn retry_exhaustion_emits_permanent_mail_failed() {
    let (storage, clock, _dir, _paths) = setup().await;
    let id = enqueued_row(&storage).await;
    // Burn 11 attempts; each remains due immediately (next_at = now).
    for attempt in 1_u32..=11 {
        storage
            .outbox_mark_retry(id, attempt, clock.now_unix_ms(), "burned".into())
            .await
            .unwrap();
    }

    let (svc, mut rx, cancel) = service(storage.clone(), clock.clone(), true);
    let run_cancel = cancel.clone();
    let handle = tokio::spawn(async move { svc.run(run_cancel).await });

    let mut saw_retry_at_12 = false;
    let mut saw_exhausted = false;
    for _ in 0..2 {
        match next_event(&mut rx).await {
            EngineEvent::OutboxRetry { attempt: 12, .. } => {
                saw_retry_at_12 = true;
            }
            EngineEvent::MailFailed {
                id: failed_id,
                error,
                permanent,
            } => {
                assert_eq!(failed_id, id);
                assert!(permanent, "exhaustion is permanent");
                assert!(matches!(
                    error,
                    kestrel_core::error::KestrelError::RetryExhausted { .. }
                ));
                saw_exhausted = true;
            }
            other => panic!("unexpected event {other:?}"),
        }
    }
    assert!(saw_retry_at_12, "retry 12 recorded before exhaustion");
    assert!(saw_exhausted, "exhaustion emits MailFailed");

    cancel.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(10), handle).await;
}

/// A raw blob missing from the CAS is a permanent rejection: `MailFailed`
/// fires with `MessageRejected`, and the row is never marked for retry.
#[tokio::test]
async fn missing_cas_blob_is_permanent_rejection() {
    let (storage, clock, _dir, paths) = setup().await;
    let id = enqueued_row(&storage).await;
    let row = &storage.outbox_due().await.unwrap()[0];
    let blobs = BlobStore::new(paths.blob_root(), paths.blob_tmp());
    blobs.remove(&row.raw_blob).await.unwrap();

    let (svc, mut rx, cancel) = service(storage, clock.clone(), true);
    let run_cancel = cancel.clone();
    let handle = tokio::spawn(async move { svc.run(run_cancel).await });

    match next_event(&mut rx).await {
        EngineEvent::MailFailed {
            id: failed_id,
            error,
            permanent,
        } => {
            assert_eq!(failed_id, id);
            assert!(permanent, "missing raw is permanent");
            assert!(matches!(
                error,
                kestrel_core::error::KestrelError::MessageRejected { .. }
            ));
        }
        other => panic!("expected MailFailed, got {other:?}"),
    }
    assert!(
        rx.try_recv().is_err(),
        "no retry event for a permanent rejection"
    );

    cancel.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(10), handle).await;
}

/// Offline mode defers flushing entirely: no SMTP contact, no events.
#[tokio::test]
async fn offline_gate_defers_flushing() {
    let (storage, clock, _dir, _paths) = setup().await;
    enqueued_row(&storage).await;
    let (svc, mut rx, cancel) = service(storage, clock, false);

    let run_cancel = cancel.clone();
    let handle = tokio::spawn(async move { svc.run(run_cancel).await });
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        rx.try_recv().is_err(),
        "offline outbox must not emit flush events"
    );

    cancel.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;
}
