//! The loop that sends queued posts to the relay.
//!
//! A start wakes it at once. A long timer is the backstop for a wake that was lost, and
//! after a failure it waits longer each time, up to a cap, so a relay that is down or has
//! just restarted is not hit by every instance at the same moment.

use std::sync::Arc;
use std::time::Duration;

use buzz_hive_core::bus::Bus;
use buzz_hive_core::orchestrator::outbox::drain_outbox;
use buzz_hive_core::orchestrator::Env;
use buzz_hive_core::ports::relay::Relay;
use buzz_hive_core::ports::store::{CommunityId, Directory, Store};
use buzz_hive_core::workflow::load::Catalog;
use tokio::sync::{watch, Notify};

/// How long to wait with nothing to send and nothing failed.
const BACKSTOP: Duration = Duration::from_secs(5 * 60);
const FIRST_RETRY: Duration = Duration::from_secs(2);
const MAX_RETRY: Duration = Duration::from_secs(5 * 60);
/// The first pass starts within this long, so a restarted fleet does not send as one.
const START_JITTER_MS: u64 = 2_000;

pub struct Drain {
    pub store: Arc<dyn Store>,
    pub directory: Arc<dyn Directory>,
    pub relay: Arc<dyn Relay>,
    pub bus: Bus,
    pub community: CommunityId,
    pub hive: String,
    /// Woken by a commit that queued a post.
    pub wake: Arc<Notify>,
}

impl Drain {
    /// Runs until `stop` changes.
    pub async fn run(self, mut stop: watch::Receiver<bool>) {
        let start = Duration::from_millis(rand::random_range(0..START_JITTER_MS));
        if sleep_or_stop(start, &mut stop).await {
            return;
        }
        let mut failures: u32 = 0;
        loop {
            failures = self.pass(failures).await;
            let stopped = if failures == 0 {
                tokio::select! {
                    _ = self.wake.notified() => false,
                    stopped = sleep_or_stop(BACKSTOP, &mut stop) => stopped,
                }
            } else {
                // A wake does not skip the wait after a failure: the cap is the point.
                sleep_or_stop(retry_after(failures), &mut stop).await
            };
            if stopped || *stop.borrow() {
                return;
            }
        }
    }

    /// One pass. Returns the number of passes in a row that failed, 0 after a clean one.
    async fn pass(&self, failures: u32) -> u32 {
        let catalog = Catalog::default();
        let env = Env {
            store: self.store.as_ref(),
            directory: self.directory.as_ref(),
            catalog: &catalog,
            hive: &self.hive,
        };
        match drain_outbox(&env, self.community, self.relay.as_ref()).await {
            Ok(done) => {
                self.bus.publish(&done.events);
                if done.posted > 0 {
                    tracing::info!(posted = done.posted, "posts sent to the relay");
                }
                if done.failed == 0 {
                    return 0;
                }
                tracing::warn!(
                    failed = done.failed,
                    error = done.first_error.as_deref().unwrap_or_default(),
                    "some posts were not sent; they stay queued"
                );
            }
            Err(error) => tracing::warn!(%error, "cannot read the outbox"),
        }
        failures.saturating_add(1)
    }
}

/// How long to wait after `failures` failed passes in a row: 2 s, doubling, capped at
/// five minutes, with up to a quarter added so instances drift apart.
pub fn retry_after(failures: u32) -> Duration {
    let base = FIRST_RETRY
        .saturating_mul(1u32 << failures.saturating_sub(1).min(16))
        .min(MAX_RETRY);
    let extra = rand::random_range(0..=base.as_millis() as u64 / 4);
    base + Duration::from_millis(extra)
}

/// Sleeps for `wait`. Returns true when `stop` changed first.
async fn sleep_or_stop(wait: Duration, stop: &mut watch::Receiver<bool>) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(wait) => false,
        _ = stop.changed() => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wait_doubles_and_stops_at_the_cap() {
        for _ in 0..50 {
            let first = retry_after(1);
            assert!(
                first >= FIRST_RETRY && first <= FIRST_RETRY * 5 / 4,
                "{first:?}"
            );
            let third = retry_after(3);
            assert!(third >= Duration::from_secs(8), "{third:?}");
            let late = retry_after(40);
            assert!(late >= MAX_RETRY && late <= MAX_RETRY * 5 / 4, "{late:?}");
        }
    }
}
