//! The loop that carries out timers: a stage's timeout, and the end of a retry's wait.
//!
//! A timer is a row in the store (see `buzz_hive_core::orchestrator::timers`), so a restart
//! loses nothing: the first pass takes up whatever is due. Timers that came due while the
//! service was down are spread over the first few seconds, so a restart does not fire them
//! all at once. After that a commit that set a timer wakes the loop, and a long timer is the
//! backstop for a wake that was lost. A pass that failed waits longer each time, up to a cap.

use std::sync::Arc;
use std::time::Duration;

use buzz_hive_core::bus::Bus;
use buzz_hive_core::orchestrator::timers::{due, fire, next_in, spread};
use buzz_hive_core::orchestrator::Env;
use buzz_hive_core::ports::store::{CommunityId, Directory, Store};
use buzz_hive_core::workflow::source::CatalogSource;
use tokio::sync::{watch, Notify};

use crate::drain::retry_after;

/// How long to wait with no timer set and nothing failed.
const BACKSTOP: Duration = Duration::from_secs(5 * 60);
/// The first pass starts within this long, so a restarted fleet does not read as one.
const START_JITTER_MS: u64 = 2_000;
/// Timers already overdue at start are fired over this long.
pub const CATCH_UP_WINDOW: Duration = Duration::from_secs(5);
/// Timers read per pass.
const BATCH: i64 = 50;

pub struct TimerLoop {
    pub store: Arc<dyn Store>,
    pub directory: Arc<dyn Directory>,
    /// Read again for every timer, so an edited package is the one a timer acts on.
    pub catalog: Arc<dyn CatalogSource>,
    pub bus: Bus,
    pub community: CommunityId,
    pub hive: String,
    /// Woken by a commit that set a timer.
    pub wake: Arc<Notify>,
    /// Woken when a timer queued a post, so it goes out at once.
    pub outbox: Arc<Notify>,
}

impl TimerLoop {
    /// Runs until `stop` changes.
    pub async fn run(self, mut stop: watch::Receiver<bool>) {
        let start = Duration::from_millis(rand::random_range(0..START_JITTER_MS));
        if sleep_or_stop(start, &mut stop).await {
            return;
        }
        let mut failures = 0u32;
        let mut first = true;
        loop {
            let outcome = self.pass(first, &mut stop).await;
            first = false;
            let wait = match outcome {
                Pass::Stopped => return,
                Pass::Failed => {
                    failures = failures.saturating_add(1);
                    Some(retry_after(failures))
                }
                Pass::Worked => {
                    failures = 0;
                    // More may be due: read again before sleeping.
                    continue;
                }
                Pass::Idle(wait) => {
                    failures = 0;
                    Some(wait)
                }
            };
            let wait = wait.unwrap_or(BACKSTOP);
            let stopped = if failures > 0 {
                // The store or a timer is failing, so a wake would only fail again: back off.
                sleep_or_stop(wait, &mut stop).await
            } else {
                tokio::select! {
                    _ = self.wake.notified() => false,
                    stopped = sleep_or_stop(wait, &mut stop) => stopped,
                }
            };
            if stopped || *stop.borrow() {
                return;
            }
        }
    }

    /// Fires what is due. `catch_up` spreads the timers that are already overdue.
    async fn pass(&self, catch_up: bool, stop: &mut watch::Receiver<bool>) -> Pass {
        let timers = match due(self.store.as_ref(), self.community, BATCH).await {
            Ok(timers) => timers,
            Err(error) => {
                tracing::warn!(%error, "cannot read the timers");
                return Pass::Failed;
            }
        };
        if timers.is_empty() {
            return match next_in(self.store.as_ref(), self.community).await {
                Ok(next) => Pass::Idle(next.map_or(BACKSTOP, |next| next.min(BACKSTOP))),
                Err(error) => {
                    tracing::warn!(%error, "cannot read the next timer");
                    Pass::Failed
                }
            };
        }
        let delays = if catch_up {
            spread(timers.len(), CATCH_UP_WINDOW, rand::random::<f64>)
        } else {
            vec![Duration::ZERO; timers.len()]
        };
        let mut failed = false;
        let mut waited = Duration::ZERO;
        for (timer, delay) in timers.iter().zip(delays) {
            if sleep_or_stop(delay.saturating_sub(waited), stop).await {
                return Pass::Stopped;
            }
            waited = waited.max(delay);
            let catalog = self.catalog.load();
            let env = Env {
                store: self.store.as_ref(),
                directory: self.directory.as_ref(),
                catalog: &catalog,
                hive: &self.hive,
            };
            match fire(&env, self.community, timer).await {
                Ok(events) => {
                    if !events.is_empty() {
                        tracing::info!(
                            run = %timer.run_id,
                            stage = %timer.stage,
                            kind = timer.kind.as_str(),
                            "timer fired"
                        );
                        self.bus.publish(&events);
                        self.outbox.notify_one();
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        %error,
                        run = %timer.run_id,
                        stage = %timer.stage,
                        "a timer could not be carried out; it stays set"
                    );
                    failed = true;
                }
            }
            if *stop.borrow() {
                return Pass::Stopped;
            }
        }
        if failed {
            Pass::Failed
        } else {
            Pass::Worked
        }
    }
}

/// How one pass ended.
enum Pass {
    Stopped,
    /// Timers were fired, so read again before sleeping.
    Worked,
    /// Nothing is due; the soonest timer is this far off.
    Idle(Duration),
    Failed,
}

/// Sleeps for `wait`. Returns true when `stop` changed first.
async fn sleep_or_stop(wait: Duration, stop: &mut watch::Receiver<bool>) -> bool {
    if wait.is_zero() {
        return *stop.borrow();
    }
    tokio::select! {
        _ = tokio::time::sleep(wait) => false,
        _ = stop.changed() => true,
    }
}
