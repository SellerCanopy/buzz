//! What a read of the code host does to a card: the pull request it holds is brought up to date,
//! and a stage that waits on outside state passes or fails on it.
//!
//! The host is read with no transaction open (see `background::pull_requests`), then
//! [`record`] writes what was read in one transaction, so a slow host holds no lock.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::json;
use uuid::Uuid;

use super::routes::{apply, facts, Mode};
use super::{Ctx, Env, OpError};
use crate::access::ActorKind;
use crate::domain::card::{Card, PullRequest};
use crate::domain::events::{Actor, Event};
use crate::domain::run::RunState;
use crate::domain::workflow::{RESULT_FAILED, RESULT_PASSED};
use crate::ports::code_host::{Check, PullStatus};
use crate::ports::store::{CommunityId, Tx};
use crate::rules::run::{step, Input};
use crate::rules::wait::{poll_decision, summarize, waiting_of, WaitOutcome};

/// What one read of the host found for a card.
#[derive(Debug, Clone)]
pub struct Seen {
    /// The pull request as the host has it. `None` when the card has no pull request, or the
    /// read failed.
    pub status: Option<PullStatus>,
    /// The pull request's checks, or `None` when they were not read: a pull request that is no
    /// longer open has none worth reading.
    pub checks: Option<Vec<Check>>,
    /// The host no longer has the pull request.
    pub gone: bool,
    /// When the host was read.
    pub now: DateTime<Utc>,
}

/// Writes what the host said about the card's pull request, and moves its stage on when it
/// waits and the answer is in. Returns the events, and whether the run moved.
pub async fn record(
    env: &Env<'_>,
    community: CommunityId,
    card_id: Uuid,
    seen: &Seen,
) -> Result<(Vec<Event>, bool), OpError> {
    let ctx = Ctx {
        community,
        actor: Actor {
            pubkey: env.hive.to_owned(),
            kind: ActorKind::Agent,
        },
        requester: None,
    };
    let mut tx = env.store.begin(community).await?;
    let Some(mut card) = tx.cards().get_for_update(card_id).await? else {
        return Ok((Vec::new(), false));
    };
    store_pull(&ctx, &mut *tx, &card, seen).await?;
    let moved = evaluate(env, &ctx, &mut *tx, &mut card, seen).await?;
    Ok((tx.commit().await?, moved))
}

/// Stores the pull request as the host has it, and logs `pull_request.changed` when anything
/// the card page shows is different.
pub(super) async fn store_pull(
    ctx: &Ctx,
    tx: &mut dyn Tx,
    card: &Card,
    seen: &Seen,
) -> Result<(), OpError> {
    let (Some(status), Some(stored)) = (&seen.status, tx.pull_requests().by_card(card.id).await?)
    else {
        return Ok(());
    };
    let mut fresh: PullRequest = status.pull.clone();
    fresh.checks = match &seen.checks {
        Some(checks) => summarize(checks).to_owned(),
        None => stored.checks.clone(),
    };
    if fresh == stored {
        return Ok(());
    }
    tx.pull_requests().put(card.id, &fresh).await?;
    let event = Event::new(
        "pull_request.changed",
        Some(card.id),
        None,
        ctx.actor.clone(),
        json!({
            "repo": fresh.repo,
            "number": fresh.number,
            "state": fresh.state,
            "draft": fresh.draft,
            "approved": fresh.approved,
            "checks": fresh.checks,
        }),
    );
    tx.events().append(event).await?;
    Ok(())
}

/// Passes or fails the stage the card's run is at, when it waits on outside state and `seen`
/// settles it. Returns whether the run moved. The card is held by the caller.
pub(super) async fn evaluate(
    env: &Env<'_>,
    ctx: &Ctx,
    tx: &mut dyn Tx,
    card: &mut Card,
    seen: &Seen,
) -> Result<bool, OpError> {
    let Some(active) = tx.runs().active_for_card(card.id).await? else {
        return Ok(false);
    };
    let Some(run) = tx.runs().get_for_update(active.id).await? else {
        return Ok(false);
    };
    let Some(stage_id) = run
        .current_stage
        .clone()
        .filter(|_| run.state == RunState::Running)
    else {
        return Ok(false);
    };
    let package = env
        .catalog
        .get(&run.workflow)
        .ok_or_else(|| OpError::FailedPrecondition(env.catalog.absent(&run.workflow)))?;
    let Some(waiting) = package.stage(&stage_id).and_then(|(s, _)| waiting_of(s)) else {
        return Ok(false);
    };
    let pull = tx.pull_requests().by_card(card.id).await?;
    let mut labels = tx
        .issues()
        .by_card(card.id)
        .await?
        .map(|row| row.issue.labels)
        .unwrap_or_default();
    labels.extend(
        seen.status
            .iter()
            .flat_map(|status| status.labels.iter().cloned()),
    );
    let waited = run
        .steps
        .last()
        .and_then(|s| s.entered_at)
        .map_or(Duration::MAX, |at| {
            (seen.now - at).to_std().unwrap_or_default()
        });
    let outcome = if seen.gone && waiting.needs_pull_request() {
        WaitOutcome::Failed("the pull request is gone from the host".into())
    } else {
        poll_decision(
            &waiting,
            pull.as_ref(),
            &labels,
            seen.checks.as_deref().unwrap_or_default(),
            waited,
        )
    };
    let (result, summary) = match outcome {
        WaitOutcome::Pending => return Ok(false),
        WaitOutcome::Passed => (RESULT_PASSED, format!("finished {}", waiting.describe())),
        WaitOutcome::Failed(reason) => (RESULT_FAILED, reason),
    };
    let facts = facts(&mut *tx, card, package, Some(&stage_id)).await?;
    let stepped = step(
        &run,
        package,
        &facts,
        Input::StageDone {
            stage: stage_id,
            result: result.to_owned(),
            summary,
        },
    );
    apply(env, ctx, &mut *tx, card, package, stepped, Mode::Update).await?;
    Ok(true)
}
