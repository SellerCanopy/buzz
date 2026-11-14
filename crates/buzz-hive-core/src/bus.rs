//! An in-process wake-up for parked `PollEvents` calls.
//!
//! `Tx::commit` returns the appended events and the caller publishes them after the
//! commit succeeds, so a rolled-back transaction wakes nobody.

use crate::domain::events::Event;

#[derive(Clone)]
pub struct Bus {
    tx: tokio::sync::watch::Sender<i64>,
    closed: tokio::sync::watch::Sender<bool>,
}

impl Bus {
    pub fn new(start: i64) -> Self {
        Self {
            tx: tokio::sync::watch::channel(start).0,
            closed: tokio::sync::watch::channel(false).0,
        }
    }

    /// Raises the newest seq to the highest in `events` and wakes every subscriber.
    pub fn publish(&self, events: &[Event]) {
        if let Some(max) = events.iter().map(|e| e.seq).max() {
            self.tx.send_if_modified(|current| {
                if max > *current {
                    *current = max;
                    true
                } else {
                    false
                }
            });
        }
    }

    /// A receiver whose `changed()` fires for any publish after this call.
    pub fn subscribe(&self) -> tokio::sync::watch::Receiver<i64> {
        self.tx.subscribe()
    }

    pub fn latest(&self) -> i64 {
        *self.tx.borrow()
    }

    /// Tells every parked poll to return now, so a shutdown does not wait out a long poll.
    pub fn close(&self) {
        self.closed.send_replace(true);
    }

    /// Resolves once [`Bus::close`] has been called, including before this call.
    pub async fn closed(&self) {
        let mut rx = self.closed.subscribe();
        // The sender lives in `self`, so `wait_for` ends only when the value is true.
        let _ = rx.wait_for(|closed| *closed).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::ActorKind;
    use crate::domain::events::Actor;

    fn event(seq: i64) -> Event {
        let mut e = Event::new(
            "card.created",
            None,
            None,
            Actor {
                pubkey: "aa".into(),
                kind: ActorKind::Person,
            },
            serde_json::json!({}),
        );
        e.seq = seq;
        e
    }

    #[tokio::test]
    async fn publish_wakes_a_subscriber_and_raises_latest() {
        let bus = Bus::new(0);
        let mut rx = bus.subscribe();
        bus.publish(&[event(3), event(5)]);
        rx.changed().await.unwrap();
        assert_eq!(*rx.borrow(), 5);
        assert_eq!(bus.latest(), 5);
    }

    #[tokio::test]
    async fn closed_resolves_after_close_even_if_called_late() {
        let bus = Bus::new(0);
        bus.close();
        tokio::time::timeout(std::time::Duration::from_secs(1), bus.closed())
            .await
            .expect("closed resolves");
    }

    #[tokio::test]
    async fn closed_waits_until_close() {
        let bus = Bus::new(0);
        let waiting =
            tokio::time::timeout(std::time::Duration::from_millis(30), bus.closed()).await;
        assert!(waiting.is_err());
    }

    #[test]
    fn a_stale_or_empty_publish_wakes_nobody() {
        let bus = Bus::new(7);
        let rx = bus.subscribe();
        bus.publish(&[event(4)]);
        bus.publish(&[]);
        assert!(!rx.has_changed().unwrap());
        assert_eq!(bus.latest(), 7);
    }
}
