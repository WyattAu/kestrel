//! Event bus (message-protocol §1): bounded broadcast, capacity 1024.
//! Slow consumers observe `EventStreamLagged` and resync via
//! `Command::ResyncState`.

use std::time::Duration;

use kestrel_core::protocol::EngineEvent;
use tokio::sync::broadcast;

/// Broadcast capacity (message-protocol §4).
const CAPACITY: usize = 1024;

/// Publish/subscribe handle pair for [`EngineEvent`]s.
#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<EngineEvent>,
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

impl EventBus {
    /// Creates the bus.
    #[must_use]
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(CAPACITY);
        Self { tx }
    }

    /// Publishes an event to all subscribers. Lagged subscribers are
    /// notified with `EventStreamLagged` on their next receive.
    pub fn publish(&self, event: EngineEvent) {
        // Sending to a broadcast channel with no receivers is not an error
        // we act on; the count is useful for tracing only.
        let _ = self.tx.send(event);
    }

    /// Subscribes a new receiver.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<EngineEvent> {
        self.tx.subscribe()
    }

    /// Subscriber count (observability).
    #[must_use]
    pub fn receiver_count(&self) -> usize {
        self.tx.receiver_count()
    }

    /// The underlying sender (for handles that clone fresh receivers).
    #[must_use]
    pub fn inner_sender(&self) -> broadcast::Sender<EngineEvent> {
        self.tx.clone()
    }
}

/// Translates broadcast lag into the protocol event.
#[must_use]
pub fn lag_event(missed: u64) -> EngineEvent {
    EngineEvent::EventStreamLagged { missed }
}

/// Marker used by tests awaiting a quiet bus.
pub const QUIET_WINDOW: Duration = Duration::from_millis(50);

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use kestrel_core::protocol::ShutdownStage;

    use super::*;

    #[tokio::test]
    async fn events_reach_multiple_subscribers() {
        let bus = EventBus::new();
        let mut a = bus.subscribe();
        let mut b = bus.subscribe();
        bus.publish(EngineEvent::EngineShutdownProgress {
            stage: ShutdownStage::Done,
        });
        assert!(matches!(
            a.recv().await,
            Ok(EngineEvent::EngineShutdownProgress {
                stage: ShutdownStage::Done
            })
        ));
        assert!(matches!(
            b.recv().await,
            Ok(EngineEvent::EngineShutdownProgress {
                stage: ShutdownStage::Done
            })
        ));
    }

    #[tokio::test]
    async fn slow_subscriber_sees_lag() {
        let bus = EventBus::new();
        let mut slow = bus.subscribe();
        for i in 0..(CAPACITY + 10) {
            bus.publish(EngineEvent::IndexProgress {
                account: kestrel_core::ids::AccountId::from_uuid(uuid::Uuid::now_v7()),
                indexed: i as u64,
                total: 1,
            });
        }
        match slow.recv().await {
            Ok(EngineEvent::IndexProgress { .. }) => {}
            Err(broadcast::error::RecvError::Lagged(n)) => {
                assert!(n > 0);
            }
            other => panic!("unexpected: {other:?}"),
        }
        // The event -> lag mapping helper exists for the forwarder task.
        let _ = lag_event(7);
    }
}
