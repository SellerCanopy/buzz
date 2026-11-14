//! Where a result sends a card. Ported from the Go engine's `rules/decide.go`.
//!
//! These functions are pure: the reducer in [`super::run`] calls them and writes the outcome
//! into the run.

use std::collections::BTreeMap;
use std::time::Duration;

use crate::domain::workflow::{Route as RouteDef, Stage, RESULT_PASSED};
use crate::workflow::routes::{ROUTE_ASK, ROUTE_DONE, ROUTE_STOP};

/// Where a result sends the card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// Another stage, or the same one again.
    Stage(String),
    /// The workflow is finished.
    Done,
    /// A person chooses where the card goes.
    Ask,
    /// The card stops.
    Stop,
    /// A stage that is given documents, and where the card goes once it passes.
    Call {
        to: String,
        give: BTreeMap<String, String>,
        then: Option<String>,
    },
}

impl Route {
    /// The stage this route enters, if it enters one.
    pub fn stage(&self) -> Option<&str> {
        match self {
            Self::Stage(to) | Self::Call { to, .. } => Some(to),
            Self::Done | Self::Ask | Self::Stop => None,
        }
    }

    /// The route a workflow file writes: a stage id, `stop`, `ask` or `done`, or a call.
    fn from_def(def: &RouteDef) -> Self {
        if def.is_call() {
            return Self::Call {
                to: def.to.clone(),
                give: def.give.clone(),
                then: (!def.then.is_empty()).then(|| def.then.clone()),
            };
        }
        Self::from_target(&def.to)
    }

    /// A plain target: a stage id or one of the words `stop`, `ask` and `done`.
    pub fn from_target(target: &str) -> Self {
        match target {
            ROUTE_STOP => Self::Stop,
            ROUTE_ASK => Self::Ask,
            ROUTE_DONE => Self::Done,
            other => Self::Stage(other.to_owned()),
        }
    }
}

/// Where `result` sends a card from `stage`. An `on` entry wins. Without one, `passed` goes to
/// `next`, the stage after this one in file order, or to done after the last stage, and every
/// other result goes to ask. The route keeps a call's `give` and `then`.
pub fn next_route(stage: &Stage, next: Option<&str>, result: &str) -> Route {
    if let Some(def) = stage.on.get(result).filter(|def| !def.to.is_empty()) {
        return Route::from_def(def);
    }
    if result == RESULT_PASSED {
        return next.map_or(Route::Done, |n| Route::Stage(n.to_owned()));
    }
    Route::Ask
}

/// The route to follow for a stage that was entered by a call with `then`. A `then` wins over
/// the stage's own route, but only when the stage passed: any other result routes as
/// [`next_route`] does. The route a `then` gives is a plain stage, so it leaves no call behind.
pub fn resolve_route(stage: &Stage, next: Option<&str>, result: &str, then: &str) -> Route {
    if !then.is_empty() && result == RESULT_PASSED {
        return Route::from_target(then);
    }
    next_route(stage, next, result)
}

/// Whether `target` is the stage at `from` or an earlier one.
pub fn routes_backward(stages: &[Stage], from: usize, target: &str) -> bool {
    stages.iter().take(from + 1).any(|s| s.id == target)
}

/// The backward routes a stage has taken in a row. `earlier` holds, oldest first, whether each
/// earlier entry of the stage routed backward. The count walks back from the latest while each
/// did, and a forward route, or none, ends it.
pub fn loop_count(earlier: &[bool]) -> u32 {
    earlier.iter().rev().take_while(|back| **back).count() as u32
}

/// Whether one more backward route, after `earlier` in a row, passes `limit`. A limit of zero
/// or less never does.
pub fn over_loop_limit(limit: i64, earlier: u32) -> bool {
    limit > 0 && i64::from(earlier) + 1 > limit
}

/// The longest wait before a retry.
pub const MAX_RETRY_BACKOFF: Duration = Duration::from_secs(15 * 60);

/// Whether a failed attempt runs again, and after how long. `attempt` is the number of the
/// attempt that failed, from 1, and `max` the stage's `execution.retries.max`. The wait starts
/// at `backoff`, doubles with each attempt and is capped at 15 minutes.
pub fn retry_decision(attempt: u32, max: u32, backoff: Duration) -> Option<Duration> {
    if attempt > max {
        return None;
    }
    let doubled = 2u32
        .checked_pow(attempt.saturating_sub(1))
        .and_then(|factor| backoff.checked_mul(factor))
        .unwrap_or(MAX_RETRY_BACKOFF);
    Some(doubled.min(MAX_RETRY_BACKOFF))
}
