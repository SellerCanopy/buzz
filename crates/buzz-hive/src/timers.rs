//! The loop that carries out timers: a stage's timeout, and the end of a retry's wait. It also
//! ends the assistant's jobs whose agent never reported: a job is no run, so it keeps its
//! deadline on its own row, and each pass sweeps the ones that are past it.
//!
//! A timer is a row in the store (see `buzz_hive_core::orchestrator::timers`), so a restart
//! loses nothing: the first pass takes up whatever is due. Timers that came due while the
//! service was down are spread over the first few seconds, so a restart does not fire them
//! all at once. After that a commit that set a timer wakes the loop, and a long timer is the
//! backstop for a wake that was lost.
//!
//! A timer that cannot be carried out (its workflow left the catalog, say) is pushed later
//! by a growing wait, the way the outbox drain holds back one card, and parked for an hour
//! once it has failed [`PARK_AFTER`] times in a row. It never holds up the others. Only a
//! failure of the store itself backs the whole loop off, and then a wake is not honored,
//! since it would only fail again.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use buzz_hive_core::bus::Bus;
use buzz_hive_core::domain::timer::Timer;
use buzz_hive_core::orchestrator::assistant::{expire_jobs, next_deadline_in, SWEEP_BATCH};
use buzz_hive_core::orchestrator::timers::{due, fire, next_in, spread};
use buzz_hive_core::orchestrator::{Env, OpError};
use buzz_hive_core::ports::store::{CommunityId, Directory, Store, StoreError};
use buzz_hive_core::workflow::source::CatalogSource;
use tokio::sync::{watch, Notify};
use tokio::time::Instant;
use uuid::Uuid;

use crate::drain::retry_after;

/// How long to wait with no timer set and nothing failed.
const BACKSTOP: Duration = Duration::from_secs(5 * 60);
/// The first pass starts within this long, so a restarted fleet does not read as one.
const START_JITTER_MS: u64 = 2_000;
/// Timers already overdue at start are fired over this long.
pub const CATCH_UP_WINDOW: Duration = Duration::from_secs(5);
/// Timers read per pass.
const BATCH: i64 = 50;
/// A timer that failed this many times in a row is parked, and logged as an error.
pub const PARK_AFTER: u32 = 10;
/// How far a parked timer is pushed back. It is tried again then, in case what was wrong
/// has been fixed, such as a package that loads again.
pub const PARKED: Duration = Duration::from_secs(60 * 60);

/// A timer that could not be carried out: how many times in a row, and when it last failed.
struct Stuck {
    failures: u32,
    last: Instant,
}

/// The timers the loop is holding back, by run, stage and kind.
#[derive(Default)]
struct Failing {
    timers: HashMap<(Uuid, String, &'static str), Stuck>,
}

impl Failing {
    fn key(timer: &Timer) -> (Uuid, String, &'static str) {
        (timer.run_id, timer.stage.clone(), timer.kind.as_str())
    }

    /// Counts one more failure of `timer`, and returns how many there are in a row.
    fn failed(&mut self, timer: &Timer, now: Instant) -> u32 {
        let stuck = self.timers.entry(Self::key(timer)).or_insert(Stuck {
            failures: 0,
            last: now,
        });
        stuck.failures = stuck.failures.saturating_add(1);
        stuck.last = now;
        stuck.failures
    }

    /// `timer` was carried out, so it starts again from no failures.
    fn cleared(&mut self, timer: &Timer) {
        self.timers.remove(&Self::key(timer));
    }

    /// Forgets a timer that has not failed for two parked spells: it was cleared by a step
    /// that got there first, and will not be seen again.
    fn prune(&mut self, now: Instant) {
        self.timers
            .retain(|_, stuck| now.saturating_duration_since(stuck.last) < PARKED * 2);
    }
}

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
        let mut failing = Failing::default();
        // Every batch of overdue timers is spread, until a pass finds nothing due.
        let mut catch_up = true;
        loop {
            let outcome = self.pass(catch_up, &mut failing, &mut stop).await;
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
                    catch_up = false;
                    Some(wait)
                }
            };
            let wait = wait.unwrap_or(BACKSTOP);
            let stopped = if failures > 0 {
                // The store is failing, so a wake would only fail again: back off.
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
    async fn pass(
        &self,
        catch_up: bool,
        failing: &mut Failing,
        stop: &mut watch::Receiver<bool>,
    ) -> Pass {
        failing.prune(Instant::now());
        if let Err(pass) = self.expire_jobs().await {
            return pass;
        }
        let timers = match due(self.store.as_ref(), self.community, BATCH).await {
            Ok(timers) => timers,
            Err(error) => {
                tracing::warn!(%error, "cannot read the timers");
                return Pass::Failed;
            }
        };
        if timers.is_empty() {
            let next = async {
                let timer = next_in(self.store.as_ref(), self.community).await?;
                let job = next_deadline_in(self.store.as_ref(), self.community).await?;
                Ok::<_, StoreError>(timer.into_iter().chain(job).min())
            };
            return match next.await {
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
                    failing.cleared(timer);
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
                // The store is down or broken, which says nothing about this timer. The rest
                // of the batch would fail the same way.
                Err(OpError::Store(
                    error @ (StoreError::Unavailable(_) | StoreError::Internal(_)),
                )) => {
                    tracing::warn!(%error, "the store failed while carrying out a timer");
                    return Pass::Failed;
                }
                Err(error) => {
                    let failures = failing.failed(timer, Instant::now());
                    let wait = if failures >= PARK_AFTER {
                        tracing::error!(
                            %error,
                            run = %timer.run_id,
                            stage = %timer.stage,
                            failures,
                            "a timer keeps failing; it is parked and tried again later"
                        );
                        PARKED
                    } else {
                        tracing::warn!(
                            %error,
                            run = %timer.run_id,
                            stage = %timer.stage,
                            failures,
                            "a timer could not be carried out; it is tried again later"
                        );
                        retry_after(failures)
                    };
                    // The timer is still set, because its step rolled back. Pushing it later
                    // keeps it from being read first on every pass.
                    if let Err(error) = self.postpone(timer, wait).await {
                        tracing::warn!(%error, "cannot set a failed timer back");
                        return Pass::Failed;
                    }
                }
            }
            if *stop.borrow() {
                return Pass::Stopped;
            }
        }
        Pass::Worked
    }

    /// Ends the assistant's jobs that are past their deadline, as failed. `Err` carries the
    /// pass to return when the store failed.
    async fn expire_jobs(&self) -> Result<(), Pass> {
        let catalog = self.catalog.load();
        let env = Env {
            store: self.store.as_ref(),
            directory: self.directory.as_ref(),
            catalog: &catalog,
            hive: &self.hive,
        };
        match expire_jobs(&env, self.community, SWEEP_BATCH).await {
            Ok(events) => {
                if !events.is_empty() {
                    tracing::info!(jobs = events.len(), "assistant jobs ended without a result");
                    self.bus.publish(&events);
                }
                Ok(())
            }
            Err(error) => {
                tracing::warn!(%error, "cannot end the assistant jobs that ran out of time");
                Err(Pass::Failed)
            }
        }
    }

    /// Sets `timer` to fire `wait` from now, in place of the time it had.
    async fn postpone(&self, timer: &Timer, wait: Duration) -> Result<(), StoreError> {
        let mut tx = self.store.begin(self.community).await?;
        tx.timers()
            .arm(timer.run_id, &timer.stage, timer.kind, wait)
            .await?;
        tx.commit().await?;
        Ok(())
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
pub(crate) async fn sleep_or_stop(wait: Duration, stop: &mut watch::Receiver<bool>) -> bool {
    if wait.is_zero() {
        return *stop.borrow();
    }
    tokio::select! {
        _ = tokio::time::sleep(wait) => false,
        _ = stop.changed() => true,
    }
}
