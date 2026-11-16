//! What happens when a timer comes due: a stage's timeout, or the end of a retry's wait.
//!
//! A timer is a row, set in the transaction that changed the run (see `routes::apply`). The
//! loop in `buzz-hive` finds the due rows and calls [`fire`], so a restarted service takes up
//! exactly the timers a running one would have. A timer fires through the same reducer as any
//! other input, under the same card and run locks.

use std::time::Duration;

use super::routes::{apply, facts, Mode};
use super::{Ctx, Env, OpError};
use crate::access::ActorKind;
use crate::domain::events::{Actor, Event};
use crate::domain::run::RunState;
use crate::domain::timer::{Timer, TimerKind};
use crate::ports::store::{CommunityId, Store, StoreError};
use crate::rules::run::{step, Input};

/// The timers whose time has come, soonest first, at most `limit`.
pub async fn due(
    store: &dyn Store,
    community: CommunityId,
    limit: i64,
) -> Result<Vec<Timer>, StoreError> {
    store.begin(community).await?.timers().due(limit).await
}

/// How long until the soonest timer is due: zero when one is overdue, `None` when none are set.
pub async fn next_in(
    store: &dyn Store,
    community: CommunityId,
) -> Result<Option<Duration>, StoreError> {
    store.begin(community).await?.timers().next_in().await
}

/// Carries out one due timer, and returns the events it committed.
///
/// The timer is taken in the same transaction as the step, so one that was set again for later,
/// or cleared by a report that got there first, does nothing. A timer whose run has ended or is
/// gone is dropped. An error leaves the timer set, for the next pass.
pub async fn fire(
    env: &Env<'_>,
    community: CommunityId,
    timer: &Timer,
) -> Result<Vec<Event>, OpError> {
    let ctx = Ctx {
        community,
        actor: Actor {
            pubkey: env.hive.to_owned(),
            kind: ActorKind::Agent,
        },
        requester: None,
    };
    let stage = timer.stage.as_str();
    let mut tx = env.store.begin(community).await?;
    let Some(known) = tx.runs().get(timer.run_id).await? else {
        tx.timers().remove(timer.run_id, stage, timer.kind).await?;
        return Ok(tx.commit().await?);
    };
    // The card first, then the run, as every other input takes them.
    let Some(mut card) = tx.cards().get_for_update(known.card_id).await? else {
        tx.timers().remove(timer.run_id, stage, timer.kind).await?;
        return Ok(tx.commit().await?);
    };
    let run = tx
        .runs()
        .get_for_update(timer.run_id)
        .await?
        .ok_or_else(|| OpError::NotFound(format!("run {} does not exist", timer.run_id)))?;
    if run.state == RunState::Ended {
        tx.timers().remove(timer.run_id, stage, timer.kind).await?;
        return Ok(tx.commit().await?);
    }
    if !tx
        .timers()
        .take_due(timer.run_id, stage, timer.kind)
        .await?
    {
        return Ok(Vec::new());
    }
    let package = env
        .catalog
        .pinned(&run.workflow, &run.workflow_hash)
        .ok_or_else(|| OpError::FailedPrecondition(env.catalog.absent(&run.workflow)))?;
    let facts = facts(&mut *tx, &card, package, run.current_stage.as_deref()).await?;
    let input = match timer.kind {
        TimerKind::Timeout => Input::TimedOut {
            stage: stage.to_owned(),
        },
        TimerKind::Retry => Input::RetryNow {
            stage: stage.to_owned(),
        },
    };
    let stepped = step(&run, package, &facts, input);
    // A timer for a stage the run has left changes nothing, and is simply gone.
    if stepped.effects.is_empty() {
        return Ok(tx.commit().await?);
    }
    apply(
        env,
        &ctx,
        &mut *tx,
        &mut card,
        package,
        stepped,
        Mode::Update,
    )
    .await?;
    Ok(tx.commit().await?)
}

/// The delay before each of `count` overdue timers fires after a start, spread over `window` so
/// a restart does not fire them all at once. `draw` gives a number in `0.0..1.0`. The delays
/// come out in the order the timers should fire.
pub fn spread(count: usize, window: Duration, mut draw: impl FnMut() -> f64) -> Vec<Duration> {
    let mut delays: Vec<Duration> = (0..count)
        .map(|_| window.mul_f64(draw().clamp(0.0, 1.0)))
        .collect();
    delays.sort();
    delays
}
