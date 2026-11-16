//! A person merges the card's pull request.
//!
//! The pull request is read from the host first, so the button works from what GitHub says now,
//! not from the last sync. Nothing is merged unless the pull request is open, out of draft and
//! its checks have finished and passed, and the merge names the commit those checks are of, so
//! a push after they were read is refused instead of merged unchecked. After the host has merged it, a stage that waits for the
//! merge passes, and a stage the workflow holds after it (the `cleanup` stage of the built-in
//! workflows) starts.

use chrono::Utc;
use uuid::Uuid;

use super::intake::IntakeError;
use super::routes::{apply, facts, Mode};
use super::run::people_only;
use super::wait::{evaluate, store_pull, Seen};
use super::{Ctx, Env, OpError};
use crate::domain::card::Card;
use crate::domain::events::Event;
use crate::domain::intake::RepoRef;
use crate::domain::run::RunState;
use crate::ports::code_host::{CodeHost, MergeMethod, PullStatus};
use crate::rules::run::{step, Input};
use crate::rules::wait::{summarize, waiting_of};

const KEY_DONT_MERGE: &str = "card.dont_merge";

/// Merges the card's pull request with `method` (squash when `None`). A pull request that is
/// merged already is not merged again, so trying again after a failure later in the call, or
/// after a person merged by hand, only brings the card up to date.
pub async fn merge_card(
    env: &Env<'_>,
    ctx: &Ctx,
    host: &dyn CodeHost,
    card_id: Uuid,
    method: Option<MergeMethod>,
) -> Result<Vec<Event>, IntakeError> {
    people_only(ctx, "merge a card")?;
    let (pull, working) = {
        let mut tx = env.store.begin(ctx.community).await?;
        let card = tx
            .cards()
            .get(card_id)
            .await?
            .ok_or_else(|| OpError::NotFound(format!("card {card_id} does not exist")))?;
        if card.archived {
            return Err(precondition("restore the card before merging it"));
        }
        let dont_merge = tx
            .settings()
            .get("card", &card_id.to_string(), KEY_DONT_MERGE)
            .await?;
        if dont_merge.and_then(|v| v.as_bool()).unwrap_or(false) {
            return Err(precondition(
                "the card is set to not merge (card.dont_merge)",
            ));
        }
        // A stage that works on the branch is not to be merged under. One that waits is not.
        let working = match tx.runs().active_for_card(card_id).await? {
            Some(run) => {
                run.state == RunState::Running
                    && !run
                        .current_stage
                        .as_deref()
                        .and_then(|id| env.catalog.get(&run.workflow)?.stage(id))
                        .is_some_and(|(stage, _)| waiting_of(stage).is_some())
            }
            None => false,
        };
        let pull =
            tx.pull_requests().by_card(card_id).await?.ok_or_else(|| {
                OpError::FailedPrecondition("the card has no pull request".into())
            })?;
        (pull, working)
    };
    let repo = RepoRef::parse(&pull.repo).map_err(OpError::FailedPrecondition)?;
    let status = host.get_pull_request(&repo, pull.number).await?;
    let read = host.list_checks(&repo, pull.number).await?;
    let checks = read.checks;
    match status.pull.state.as_str() {
        "merged" => {}
        "closed" => return Err(precondition("the pull request is closed")),
        _ if working => {
            return Err(precondition(
                "a stage is running; stop it, or wait for it to finish, before merging",
            ))
        }
        _ if status.pull.draft => {
            return Err(precondition(
                "the pull request is a draft; mark it ready for review first",
            ))
        }
        _ => match summarize(&checks) {
            "failing" => return Err(precondition("the pull request has failing checks")),
            "pending" => return Err(precondition("the pull request's checks have not finished")),
            // Pinned to the commit the checks are of: a push since then is refused by the host.
            _ => {
                host.merge(
                    &repo,
                    pull.number,
                    method.unwrap_or_default(),
                    Some(&read.head_sha),
                )
                .await?
            }
        },
    }
    // Written from what the host said, with the state it now has.
    let mut merged = status.pull;
    merged.state = "merged".into();
    let seen = Seen {
        status: Some(PullStatus {
            pull: merged,
            labels: status.labels,
        }),
        checks: Some(checks),
        head_sha: Some(read.head_sha),
        gone: false,
        now: Utc::now(),
    };
    let mut tx = env.store.begin(ctx.community).await?;
    let Some(mut card) = tx.cards().get_for_update(card_id).await? else {
        return Err(OpError::NotFound(format!("card {card_id} does not exist")).into());
    };
    store_pull(ctx, &mut *tx, &card, &seen).await?;
    // A stage that waited for the merge passes, and then the stage held after it starts.
    evaluate(env, ctx, &mut *tx, &mut card, &seen).await?;
    start_held(env, ctx, &mut *tx, &mut card).await?;
    Ok(tx.commit().await?)
}

/// Starts the stage a workflow holds the card before, when the run waits there: the stage after
/// the merge.
async fn start_held(
    env: &Env<'_>,
    ctx: &Ctx,
    tx: &mut dyn crate::ports::store::Tx,
    card: &mut Card,
) -> Result<(), OpError> {
    let Some(active) = tx.runs().active_for_card(card.id).await? else {
        return Ok(());
    };
    let Some(run) = tx.runs().get_for_update(active.id).await? else {
        return Ok(());
    };
    if run.state != RunState::Paused || !run.steps.last().is_some_and(|s| s.held) {
        return Ok(());
    }
    let package = env
        .catalog
        .get(&run.workflow)
        .ok_or_else(|| OpError::FailedPrecondition(env.catalog.absent(&run.workflow)))?;
    let facts = facts(tx, card, package, run.current_stage.as_deref()).await?;
    let stepped = step(&run, package, &facts, Input::Resume);
    apply(env, ctx, tx, card, package, stepped, Mode::Update).await?;
    Ok(())
}

fn precondition(message: &str) -> IntakeError {
    OpError::FailedPrecondition(message.to_owned()).into()
}

/// Checks `repos` is the card's one repo, which is the only order a card with one repo has.
/// Returns the card.
pub async fn set_merge_order(
    env: &Env<'_>,
    ctx: &Ctx,
    card_id: Uuid,
    repos: &[String],
) -> Result<Card, OpError> {
    people_only(ctx, "set the merge order")?;
    let mut tx = env.store.begin(ctx.community).await?;
    let card = tx
        .cards()
        .get(card_id)
        .await?
        .ok_or_else(|| OpError::NotFound(format!("card {card_id} does not exist")))?;
    let issue = tx.issues().by_card(card_id).await?;
    let own: Vec<String> = issue.iter().map(|row| row.issue.repo.to_string()).collect();
    let mut want = repos.to_vec();
    want.sort();
    if want != own {
        return Err(OpError::InvalidArgument(if own.is_empty() {
            "the card has no repo, so it has no merge order".into()
        } else {
            format!(
                "repos must name every repo on the card exactly once: {}",
                own.join(", ")
            )
        }));
    }
    Ok(card)
}
