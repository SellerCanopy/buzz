//! The loops that read the code host: one per enabled intake rule, and one for the comments
//! on the issues of cards that are working.
//!
//! What a pass does, and the schedule arithmetic, are in `buzz_hive_core::background`. This
//! file only runs them on time. The rules a restart finds in the store start again, each after
//! a delay of up to a fifth of its interval, so a fleet that restarts together does not read
//! the host together. A pass that fails is repeated after a doubling wait, up to half an hour.
//! One token serves every rule, so a rate limit stops them all until it lifts: the
//! [`Gate`] holds the loops, and each starts again a little apart.
//!
//! Host calls: one list request per rule per interval (a `304` costs no quota), one read per
//! issue that left a search, and one comment read per card that is running or waiting every
//! [`COMMENT_INTERVAL_S`] seconds. Nothing reads the relay.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use buzz_hive_core::background::comments::poll_comments;
use buzz_hive_core::background::intake::{
    backoff, next_wake, rate_wait, record_failure, refresh_rule, start_delay, Gate, JITTER,
};
use buzz_hive_core::bus::Bus;
use buzz_hive_core::domain::events::Event;
use buzz_hive_core::domain::intake::IntakeRule;
use buzz_hive_core::orchestrator::intake::{list_rules, IntakeError};
use buzz_hive_core::orchestrator::{Env, OpError};
use buzz_hive_core::ports::code_host::{CodeHost, HostError};
use buzz_hive_core::ports::store::{CommunityId, Directory, Store};
use buzz_hive_core::workflow::load::Catalog;
use buzz_hive_core::workflow::source::CatalogSource;
use chrono::Utc;
use tokio::sync::{watch, Notify};
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::drain::retry_after;
use crate::timers::sleep_or_stop;

/// How often the comments of working cards are read, in seconds.
pub const COMMENT_INTERVAL_S: u32 = 120;
/// How long the supervisor waits for a wake that was lost before it reads the rules again.
const BACKSTOP: Duration = Duration::from_secs(15 * 60);
/// How long shutdown waits for a pass in flight.
const STOP_GRACE: Duration = Duration::from_secs(10);

/// A random number in `[0, 1)`. A test passes one it chooses.
pub type Random = Arc<dyn Fn() -> f64 + Send + Sync>;

pub fn system_random() -> Random {
    Arc::new(rand::random::<f64>)
}

/// What every loop shares.
pub struct Shared {
    pub store: Arc<dyn Store>,
    pub directory: Arc<dyn Directory>,
    pub catalog: Arc<dyn CatalogSource>,
    pub host: Arc<dyn CodeHost>,
    pub bus: Bus,
    pub community: CommunityId,
    /// The Beehive account's pubkey, the actor of what intake does.
    pub hive: String,
    /// Woken when intake queued a post, so it goes out at once.
    pub outbox: Arc<Notify>,
    pub gate: Gate,
    pub random: Random,
}

impl Shared {
    fn unit(&self) -> f64 {
        (self.random)()
    }

    /// `wait` plus up to a fifth, so loops that wait for the same thing do not start together.
    fn jittered(&self, wait: Duration) -> Duration {
        wait + wait.mul_f64(JITTER * self.unit().clamp(0.0, 1.0))
    }

    fn publish(&self, events: &[Event]) {
        self.bus.publish(events);
    }

    /// The environment of an operation, over `catalog` as it is now.
    fn env<'a>(&'a self, catalog: &'a Catalog) -> Env<'a> {
        Env {
            store: self.store.as_ref(),
            directory: self.directory.as_ref(),
            catalog,
            hive: &self.hive,
        }
    }
}

/// Starts a loop for each enabled rule and keeps them matching the stored rules.
pub struct IntakeSupervisor {
    pub shared: Arc<Shared>,
    /// Woken when a rule was saved or deleted.
    pub wake: Arc<Notify>,
}

struct Running {
    rule: IntakeRule,
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
}

impl IntakeSupervisor {
    /// Runs until `stop` changes.
    pub async fn run(self, mut stop: watch::Receiver<bool>) {
        let comments = tokio::spawn(comment_loop(self.shared.clone(), stop.clone()));
        let mut running: HashMap<Uuid, Running> = HashMap::new();
        let mut failures = 0u32;
        loop {
            match list_rules(self.shared.store.as_ref(), self.shared.community).await {
                Ok((rules, _)) => {
                    failures = 0;
                    self.reconcile(&mut running, rules);
                }
                Err(error) => {
                    failures = failures.saturating_add(1);
                    tracing::warn!(%error, "cannot read the intake rules");
                }
            }
            let stopped = if failures > 0 {
                // The store is failing, so a wake would only fail again: back off.
                sleep_or_stop(retry_after(failures), &mut stop).await
            } else {
                tokio::select! {
                    _ = self.wake.notified() => false,
                    stopped = sleep_or_stop(BACKSTOP, &mut stop) => stopped,
                }
            };
            if stopped || *stop.borrow() {
                break;
            }
        }
        let tasks: Vec<_> = running
            .into_values()
            .map(|r| {
                let _ = r.stop.send(true);
                r.task
            })
            .chain(std::iter::once(comments))
            .collect();
        for mut task in tasks {
            if tokio::time::timeout(STOP_GRACE, &mut task).await.is_err() {
                tracing::warn!("an intake loop did not stop in time; it was cut off");
                task.abort();
            }
        }
    }

    /// Stops the loop of every rule that is gone, changed or switched off, and starts one for
    /// every enabled rule that has none. A pass already running when its rule changes finishes;
    /// the row lock on each issue keeps it and the new loop from making two cards.
    fn reconcile(&self, running: &mut HashMap<Uuid, Running>, rules: Vec<IntakeRule>) {
        let wanted: HashMap<Uuid, IntakeRule> = rules
            .into_iter()
            .filter(|rule| rule.enabled)
            .map(|rule| (rule.id, rule))
            .collect();
        running.retain(|id, have| {
            let keep = wanted.get(id) == Some(&have.rule);
            if !keep {
                let _ = have.stop.send(true);
            }
            keep
        });
        for (id, rule) in wanted {
            if running.contains_key(&id) {
                continue;
            }
            let (own_stop, own_stopped) = watch::channel(false);
            let task = tokio::spawn(rule_loop(self.shared.clone(), rule.clone(), own_stopped));
            running.insert(
                id,
                Running {
                    rule,
                    stop: own_stop,
                    task,
                },
            );
        }
    }
}

/// Holds until the gate is open. Returns true when `stop` changed first.
async fn gate_open(gate: &Gate, stop: &mut watch::Receiver<bool>) -> bool {
    tokio::select! {
        _ = gate.wait() => *stop.borrow(),
        _ = stop.changed() => true,
    }
}

/// One rule's loop: a jittered start, then a pass every interval until `stop` changes or the
/// rule is deleted.
async fn rule_loop(shared: Arc<Shared>, rule: IntakeRule, mut stop: watch::Receiver<bool>) {
    run_rule(&shared, &rule, &mut stop).await;
}

async fn run_rule(shared: &Shared, rule: &IntakeRule, stop: &mut watch::Receiver<bool>) {
    let interval = rule.interval_s;
    if sleep_or_stop(start_delay(interval, shared.unit()), stop).await {
        return;
    }
    let mut failures = 0u32;
    let mut limits = 0u32;
    loop {
        if gate_open(&shared.gate, stop).await {
            return;
        }
        let catalog = shared.catalog.load();
        let result = refresh_rule(
            &shared.env(&catalog),
            shared.host.as_ref(),
            shared.community,
            rule.id,
            Utc::now(),
        )
        .await;
        let wait = match result {
            Ok((outcome, events)) => {
                failures = 0;
                limits = 0;
                shared.publish(&events);
                tracing::info!(
                    rule = %rule.id,
                    repo = %rule.repo,
                    matched = outcome.matched,
                    added = outcome.added.len(),
                    left = outcome.left,
                    "intake pass"
                );
                next_wake(interval, shared.unit())
            }
            // The rule was deleted while its loop slept; the supervisor stops this loop too.
            Err(IntakeError::Op(OpError::NotFound(_))) => return,
            Err(IntakeError::Host(HostError::RateLimited { retry_after })) => {
                failures = failures.saturating_add(1);
                limits = limits.saturating_add(1);
                let wait = rate_wait(retry_after, interval, limits);
                shared.gate.hold(wait).await;
                tracing::warn!(rule = %rule.id, wait_s = wait.as_secs(), "GitHub rate limit; intake waits");
                fail(shared, rule, "rate limited", failures).await;
                shared.jittered(wait)
            }
            Err(error) => {
                failures = failures.saturating_add(1);
                limits = 0;
                tracing::warn!(rule = %rule.id, repo = %rule.repo, %error, failures, "intake pass failed");
                fail(shared, rule, &error.to_string(), failures).await;
                shared.jittered(backoff(interval, failures))
            }
        };
        if sleep_or_stop(wait, stop).await {
            return;
        }
    }
}

/// Records a failed pass in the log of events.
async fn fail(shared: &Shared, rule: &IntakeRule, error: &str, failures: u32) {
    let catalog = shared.catalog.load();
    let recorded = record_failure(
        &shared.env(&catalog),
        shared.community,
        rule.id,
        &rule.repo.to_string(),
        error,
        failures,
    )
    .await;
    match recorded {
        Ok(events) => shared.publish(&events),
        Err(error) => tracing::warn!(%error, "cannot record a failed intake pass"),
    }
}

/// Reads the comments of working cards on a timer, and hands a trusted one to its agent.
async fn comment_loop(shared: Arc<Shared>, mut stop: watch::Receiver<bool>) {
    let start = Duration::from_secs(u64::from(COMMENT_INTERVAL_S)).mul_f64(shared.unit());
    if sleep_or_stop(start, &mut stop).await {
        return;
    }
    let mut failures = 0u32;
    loop {
        if gate_open(&shared.gate, &mut stop).await {
            return;
        }
        let catalog = shared.catalog.load();
        let result = poll_comments(
            &shared.env(&catalog),
            shared.host.as_ref(),
            shared.community,
            Utc::now(),
        )
        .await;
        let wait = match result {
            Ok((polled, events)) => {
                failures = 0;
                shared.publish(&events);
                if polled.delivered > 0 {
                    shared.outbox.notify_one();
                }
                next_wake(COMMENT_INTERVAL_S, shared.unit())
            }
            Err(IntakeError::Host(HostError::RateLimited { retry_after })) => {
                failures = failures.saturating_add(1);
                let wait = rate_wait(retry_after, COMMENT_INTERVAL_S, failures);
                shared.gate.hold(wait).await;
                tracing::warn!(
                    wait_s = wait.as_secs(),
                    "GitHub rate limit; comment reads wait"
                );
                shared.jittered(wait)
            }
            Err(error) => {
                failures = failures.saturating_add(1);
                tracing::warn!(%error, failures, "comment poll failed");
                shared.jittered(backoff(COMMENT_INTERVAL_S, failures))
            }
        };
        if sleep_or_stop(wait, &mut stop).await {
            return;
        }
    }
}
