//! Service supervisor (ADR 0004): panic containment + restart with
//! exponential backoff and jitter; `ServiceDegraded` events are never
//! silent. Backoff: 250 ms base, ×2, ±20% jitter, cap 5 min; reset after
//! 5 min healthy.

use std::{panic::AssertUnwindSafe, time::Duration};

use kestrel_core::{
    error::KestrelError,
    protocol::{EngineEvent, ServiceId},
};
use tokio_util::sync::CancellationToken;

use crate::bus::EventBus;

/// Supervision parameters (ADR 0004 / message-protocol §5).
#[derive(Clone, Copy, Debug)]
pub struct SupervisionConfig {
    /// Base restart delay.
    pub base_backoff: Duration,
    /// Backoff multiplier.
    pub multiplier: f64,
    /// Jitter fraction (±).
    pub jitter: f64,
    /// Maximum delay.
    pub cap: Duration,
    /// Healthy time that resets the backoff.
    pub reset_after: Duration,
}

/// Whole-minute `Duration`s (clippy's preferred unit boundary).
const fn mins(n: u64) -> Duration {
    Duration::from_secs(n * 60)
}

impl Default for SupervisionConfig {
    fn default() -> Self {
        Self {
            base_backoff: Duration::from_millis(250),
            multiplier: 2.0,
            jitter: 0.2,
            cap: mins(5),
            reset_after: mins(5),
        }
    }
}

/// A supervised service: spawns `run` in a task, restarting it (with
/// backoff) whenever it fails or panics, until `stop` fires. `run` receives
/// the per-attempt cancellation token and must return `Ok(())` only on
/// clean shutdown.
pub struct Supervisor;

/// Deterministic jitter source (xorshift over the attempt counter) —
/// reproducible in tests, no `rand` dependency.
fn jitter(seed: u32) -> f64 {
    let mut x = seed.wrapping_mul(2_655_443_576).wrapping_add(1);
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    (f64::from(x % 1_000) / 1_000.0).min(0.999)
}

impl Supervisor {
    /// Computes the restart delay for an attempt under the given config.
    // Duration arithmetic uses whole milliseconds (u64 domain) — no
    // float casts on the hot path.
    #[must_use]
    pub fn backoff(cfg: &SupervisionConfig, attempt: u32, seed: u32) -> Duration {
        // Durations are config-bounded; u128->u64 via try_from keeps the
        // audited-cast lint quiet.
        let base_ms = u64::try_from(cfg.base_backoff.as_millis()).unwrap_or(u64::MAX);
        let cap_ms = u64::try_from(cfg.cap.as_millis()).unwrap_or(u64::MAX);
        let mut ms = base_ms;
        for _ in 1..attempt {
            ms = ms.saturating_mul(2).min(cap_ms);
        }
        // Jitter scales within [1-j, 1+j]; ms is capped far below 2^53 so
        // the rounding-to-u64 below is exact for our ranges.
        let j = 1.0 + (jitter(seed) * 2.0 - 1.0) * cfg.jitter;
        let scaled = f64::from(u32::try_from(ms.min(u64::from(u32::MAX))).unwrap_or(u32::MAX)) * j;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let millis = scaled.max(0.0) as u64;
        Duration::from_millis(millis)
    }

    /// Runs the supervision loop for one service.
    ///
    /// The future `factory` is invoked once per (re)start with a fresh
    /// cancellation token; a returned `Err` or a panic triggers backoff +
    /// `ServiceDegraded`. Clean `Ok(())` exits end supervision (service
    /// completed; no restart).
    pub async fn supervise<F, Fut>(
        service: ServiceId,
        cfg: SupervisionConfig,
        bus: EventBus,
        mut stop: CancellationToken,
        factory: F,
    ) where
        F: Fn(CancellationToken) -> Fut,
        Fut: Future<Output = Result<(), KestrelError>>,
    {
        let mut attempt: u32 = 0;
        loop {
            let attempt_token = stop.child_token();
            let started = tokio::time::Instant::now();
            // Panic containment: catch_unwind around the future; tokio
            // panics surface as JoinError normally, but we run inline in
            // the supervisor task, so AssertUnwindSafe + catch_unwind is
            // the containment boundary (ADR 0004).
            let outcome =
                futures_future_catch(AssertUnwindSafe(factory(attempt_token.clone()))).await;
            match outcome {
                Ok(Ok(())) => {
                    tracing::info!(service = %service, "service stopped cleanly");
                    return;
                }
                Ok(Err(err)) => {
                    if stop.is_cancelled() {
                        return;
                    }
                    Self::degrade(
                        service,
                        cfg,
                        bus.clone(),
                        &mut attempt,
                        started,
                        err,
                        &mut stop,
                    )
                    .await;
                }
                Err(panic_payload) => {
                    if stop.is_cancelled() {
                        return;
                    }
                    let detail = panic_message(&panic_payload);
                    Self::degrade(
                        service,
                        cfg,
                        bus.clone(),
                        &mut attempt,
                        started,
                        KestrelError::Bug { detail },
                        &mut stop,
                    )
                    .await;
                }
            }
        }
    }

    async fn degrade(
        service: ServiceId,
        cfg: SupervisionConfig,
        bus: EventBus,
        attempt: &mut u32,
        started: tokio::time::Instant,
        err: KestrelError,
        stop: &mut CancellationToken,
    ) {
        // Reset backoff after a healthy stretch (ADR 0004).
        if started.elapsed() >= cfg.reset_after {
            *attempt = 0;
        }
        *attempt += 1;
        let restart_in = Self::backoff(&cfg, *attempt, 1_000_u32.wrapping_add(*attempt));
        tracing::warn!(
            service = %service,
            error = %err,
            attempt = *attempt,
            restart_in_ms = u64::try_from(restart_in.as_millis()).unwrap_or(u64::MAX),
            "service degraded; restarting"
        );
        bus.publish(EngineEvent::ServiceDegraded {
            service,
            error: err,
            restart_in,
        });
        tokio::select! {
            () = stop.cancelled() => {}
            () = tokio::time::sleep(restart_in) => {}
        }
    }
}

async fn futures_future_catch<F: Future>(
    fut: std::panic::AssertUnwindSafe<F>,
) -> Result<F::Output, Box<dyn std::any::Any + Send>> {
    futures::FutureExt::catch_unwind(fut).await
}

fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "opaque panic payload".to_string()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    };

    use super::*;

    #[test]
    fn backoff_grows_exponentially_and_caps() {
        let cfg = SupervisionConfig::default();
        let first = Supervisor::backoff(&cfg, 1, 1);
        assert!(first.as_millis() >= 200 && first.as_millis() <= 300);
        let a5 = Supervisor::backoff(&cfg, 5, 1);
        let a12 = Supervisor::backoff(&cfg, 12, 1);
        assert!(a5 > first);
        assert!(a12 >= a5);
        assert!(a12 <= cfg.cap);
    }

    #[tokio::test]
    async fn failing_service_is_restarted_with_events() {
        let bus = EventBus::new();
        let mut events = bus.subscribe();
        let counter = Arc::new(AtomicU32::new(0));
        let c = counter.clone();
        let stop = CancellationToken::new();
        let cfg = SupervisionConfig {
            base_backoff: Duration::from_millis(5),
            multiplier: 1.0,
            jitter: 0.0,
            cap: Duration::from_millis(5),
            reset_after: Duration::from_mins(5),
        };
        let task = tokio::spawn(Supervisor::supervise(
            ServiceId::Index,
            cfg,
            bus.clone(),
            stop.clone(),
            move |token| {
                let c = c.clone();
                async move {
                    if c.fetch_add(1, Ordering::SeqCst) >= 2 {
                        // Third start: wait for cancellation then exit clean.
                        token.cancelled().await;
                        Ok(())
                    } else {
                        Err(KestrelError::IndexCommitFailed)
                    }
                }
            },
        ));
        // Two quick failures before the third run settles.
        let mut degraded = 0;
        while degraded < 2 {
            if let Ok(EngineEvent::ServiceDegraded { service, .. }) = events.recv().await
                && service == ServiceId::Index
            {
                degraded += 1;
            }
        }
        stop.cancel();
        task.await.unwrap();
        assert!(
            counter.load(Ordering::SeqCst) >= 3,
            "restarted to third run"
        );
    }

    #[tokio::test]
    async fn panicking_service_is_contained() {
        let bus = EventBus::new();
        let mut events = bus.subscribe();
        let counter = Arc::new(AtomicU32::new(0));
        let c = counter.clone();
        let stop = CancellationToken::new();
        let cfg = SupervisionConfig {
            base_backoff: Duration::from_millis(5),
            multiplier: 1.0,
            jitter: 0.0,
            cap: Duration::from_millis(5),
            reset_after: Duration::from_mins(5),
        };
        let task = tokio::spawn(Supervisor::supervise(
            ServiceId::Search,
            cfg,
            bus.clone(),
            stop.clone(),
            move |token| {
                let c = c.clone();
                async move {
                    assert!(c.fetch_add(1, Ordering::SeqCst) != 0, "service exploded");
                    token.cancelled().await;
                    Ok(())
                }
            },
        ));
        match events.recv().await {
            Ok(EngineEvent::ServiceDegraded { error, .. }) => {
                assert_eq!(error.kind(), "engine.bug");
            }
            other => panic!("expected ServiceDegraded, got {other:?}"),
        }
        stop.cancel();
        task.await.unwrap();
    }
}
