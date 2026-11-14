//! Starting a run and the inputs that move it. Every input to a run goes through the same
//! row lock, and each is one transaction: the run, the events, the queued posts and the
//! card's state commit together or not at all.

use std::collections::BTreeMap;

use uuid::Uuid;

use super::routes::{apply, begin_run, facts, resolve_settings, Mode};
use super::settings;
use super::{Ctx, Env, OpError};
use crate::config::LayerKind;
use crate::domain::card::Card;
use crate::domain::events::Event;
use crate::domain::run::{Run, RunState};
use crate::ports::store::{Posting, Tx};
use crate::rules::run::{entry_stage, step, task_key, Input};
use crate::workflow::load::Package;
use crate::workflow::routes::reportable_results;

/// The longest summary a stage may send with its result.
pub const MAX_SUMMARY_CHARS: usize = 10_000;

/// The setting that lets a stage's agent move the card itself.
const KEY_AGENT_MOVES_CARDS: &str = "side_effects.agent_moves_cards";

/// Starts a run of the card's workflow at its first stage.
pub async fn start_run(
    env: &Env<'_>,
    ctx: &Ctx,
    card_id: Uuid,
) -> Result<(Run, Vec<Event>), OpError> {
    start_run_at(env, ctx, card_id, None, BTreeMap::new()).await
}

/// Starts a run of the card's workflow at `stage` (the first stage when `None`) with the
/// run's own parameter values.
///
/// One transaction holds the card, makes the run, moves the card to `running`, appends the
/// events and queues the posts, so a crash between them loses neither. The posts reach the
/// relay when the outbox is drained. Every refusal is a sentence the person can act on.
pub async fn start_run_at(
    env: &Env<'_>,
    ctx: &Ctx,
    card_id: Uuid,
    stage: Option<&str>,
    parameters: BTreeMap<String, String>,
) -> Result<(Run, Vec<Event>), OpError> {
    let mut tx = env.store.begin(ctx.community).await?;
    let mut card = tx
        .cards()
        .get_for_update(card_id)
        .await?
        .ok_or_else(|| OpError::NotFound(format!("card {card_id} does not exist")))?;
    if card.archived {
        return Err(OpError::FailedPrecondition(
            "restore the card before starting it".into(),
        ));
    }
    if card.workflow.is_empty() {
        return Err(OpError::FailedPrecondition(
            "the card has no workflow; choose one before starting it".into(),
        ));
    }
    let package = env.catalog.get(&card.workflow).ok_or_else(|| {
        OpError::FailedPrecondition(format!("workflow {} does not exist", card.workflow))
    })?;
    if let Some(id) = stage {
        if package.stage(id).is_none() {
            return Err(OpError::InvalidArgument(format!(
                "workflow {} has no stage {id}",
                package.meta.name
            )));
        }
    }
    entry_stage(package, stage).map_err(OpError::FailedPrecondition)?;
    // The card row is held, so two starts of one card queue here and the second sees the first.
    if let Some(active) = tx.runs().active_for_card(card_id).await? {
        return Err(OpError::FailedPrecondition(format!(
            "the card already has a run, {}",
            active.state.as_str()
        )));
    }
    let channel = settings::update_channel(&mut *tx).await?.ok_or_else(|| {
        OpError::FailedPrecondition(
            "no update channel is set; choose one in Settings (update_channel), then start the run again"
                .into(),
        )
    })?;
    settings::check_channel(
        env,
        ctx,
        channel,
        "add the Beehive account to the update channel, then start the run again",
    )
    .await?;
    check_parameters(env, &mut *tx, &card, &parameters).await?;

    let run = begin_run(
        env, ctx, &mut *tx, &mut card, package, stage, parameters, false,
    )
    .await?;
    let events = tx.commit().await?;
    Ok((run, events))
}

/// Refuses a parameter value the workflow does not declare or that breaks its rules.
async fn check_parameters(
    env: &Env<'_>,
    tx: &mut dyn Tx,
    card: &Card,
    parameters: &BTreeMap<String, String>,
) -> Result<(), OpError> {
    if parameters.is_empty() {
        return Ok(());
    }
    let resolution = resolve_settings(env, tx, card, parameters, None).await?;
    match resolution
        .invalid
        .iter()
        .find(|bad| bad.source.layer == LayerKind::Run)
    {
        Some(bad) => Err(OpError::InvalidArgument(bad.message.clone())),
        None => Ok(()),
    }
}

/// The stage's agent reports its result. Only the agent the stage task mentioned may
/// report, whatever level others hold.
///
/// The card and the run are held, so a report and a start or another report queue. The
/// result routes the card: to another stage, to done, to stop, or to a wait for a person.
/// A stale call, for a card whose run is not running, is refused with a sentence rather
/// than applied twice.
pub async fn stage_done(
    env: &Env<'_>,
    ctx: &Ctx,
    card_id: Uuid,
    result: &str,
    summary: &str,
) -> Result<(Run, Vec<Event>), OpError> {
    if summary.chars().count() > MAX_SUMMARY_CHARS {
        return Err(OpError::InvalidArgument(format!(
            "the summary is over {MAX_SUMMARY_CHARS} characters"
        )));
    }
    let mut tx = env.store.begin(ctx.community).await?;
    let (mut card, run, package) = locked(env, &mut *tx, card_id).await?;
    let stage_id = running_stage(&run)?;
    let (stage, _) = package.stage(&stage_id).ok_or_else(|| {
        OpError::FailedPrecondition(format!("workflow {} has no stage {stage_id}", run.workflow))
    })?;
    check_agent(&mut *tx, ctx, &run, &stage_id).await?;
    let allowed = reportable_results(stage);
    if !allowed.iter().any(|r| r == result) {
        return Err(OpError::InvalidArgument(format!(
            "{result:?} is not a result of stage {stage_id}; use one of: {}",
            allowed.join(", ")
        )));
    }
    let facts = facts(&mut *tx, &card, package).await?;
    let stepped = step(
        &run,
        package,
        &facts,
        Input::StageDone {
            stage: stage_id,
            result: result.to_owned(),
            summary: summary.to_owned(),
        },
    );
    let run = apply(
        env,
        ctx,
        &mut *tx,
        &mut card,
        package,
        stepped,
        Mode::Update,
    )
    .await?;
    let events = tx.commit().await?;
    Ok((run, events))
}

/// A person moves a card that waits, or is held, on as if its stage had passed.
pub async fn skip_stage(
    env: &Env<'_>,
    ctx: &Ctx,
    card_id: Uuid,
) -> Result<(Run, Vec<Event>), OpError> {
    let mut tx = env.store.begin(ctx.community).await?;
    let (mut card, run, package) = locked(env, &mut *tx, card_id).await?;
    if !matches!(run.state, RunState::Waiting | RunState::Paused) {
        return Err(OpError::FailedPrecondition(format!(
            "the run is {}; only a card that waits or is held can skip its stage",
            run.state.as_str()
        )));
    }
    let facts = facts(&mut *tx, &card, package).await?;
    let stepped = step(&run, package, &facts, Input::Skip);
    let run = apply(
        env,
        ctx,
        &mut *tx,
        &mut card,
        package,
        stepped,
        Mode::Update,
    )
    .await?;
    let events = tx.commit().await?;
    Ok((run, events))
}

/// A person holds a running card. Its agent may not report until the card is resumed.
pub async fn pause_card(
    env: &Env<'_>,
    ctx: &Ctx,
    card_id: Uuid,
) -> Result<(Run, Vec<Event>), OpError> {
    let mut tx = env.store.begin(ctx.community).await?;
    let (mut card, run, package) = locked(env, &mut *tx, card_id).await?;
    if run.state != RunState::Running {
        return Err(OpError::FailedPrecondition(format!(
            "the run is {}; only a running card can be paused",
            run.state.as_str()
        )));
    }
    let facts = facts(&mut *tx, &card, package).await?;
    let stepped = step(&run, package, &facts, Input::Pause);
    let run = apply(
        env,
        ctx,
        &mut *tx,
        &mut card,
        package,
        stepped,
        Mode::Update,
    )
    .await?;
    let events = tx.commit().await?;
    Ok((run, events))
}

/// A person lets a held stage start, or tells a paused stage's agent to carry on.
pub async fn resume_card(
    env: &Env<'_>,
    ctx: &Ctx,
    card_id: Uuid,
) -> Result<(Run, Vec<Event>), OpError> {
    let mut tx = env.store.begin(ctx.community).await?;
    let (mut card, run, package) = locked(env, &mut *tx, card_id).await?;
    if run.state != RunState::Paused {
        return Err(OpError::FailedPrecondition(format!(
            "the run is {}; only a paused card can be resumed",
            run.state.as_str()
        )));
    }
    let facts = facts(&mut *tx, &card, package).await?;
    let stepped = step(&run, package, &facts, Input::Resume);
    let run = apply(
        env,
        ctx,
        &mut *tx,
        &mut card,
        package,
        stepped,
        Mode::Update,
    )
    .await?;
    let events = tx.commit().await?;
    Ok((run, events))
}

/// The stage's agent moves the card to `target`, in place of the route its result would give.
/// A workflow allows it with `side_effects.agent_moves_cards`.
pub async fn move_card(
    env: &Env<'_>,
    ctx: &Ctx,
    card_id: Uuid,
    target: &str,
) -> Result<(Run, Vec<Event>), OpError> {
    let mut tx = env.store.begin(ctx.community).await?;
    let (mut card, run, package) = locked(env, &mut *tx, card_id).await?;
    let stage_id = running_stage(&run)?;
    check_agent(&mut *tx, ctx, &run, &stage_id).await?;
    let allowed = resolve_settings(env, &mut *tx, &card, &run.parameters, Some(&stage_id))
        .await?
        .value(KEY_AGENT_MOVES_CARDS)
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    if !allowed {
        return Err(OpError::FailedPrecondition(format!(
            "workflow {} does not let an agent move cards; set {KEY_AGENT_MOVES_CARDS} to allow it",
            run.workflow
        )));
    }
    if package.stage(target).is_none() {
        return Err(OpError::InvalidArgument(format!(
            "workflow {} has no stage {target}",
            run.workflow
        )));
    }
    let facts = facts(&mut *tx, &card, package).await?;
    let stepped = step(
        &run,
        package,
        &facts,
        Input::Move {
            stage: target.to_owned(),
        },
    );
    let run = apply(
        env,
        ctx,
        &mut *tx,
        &mut card,
        package,
        stepped,
        Mode::Update,
    )
    .await?;
    let events = tx.commit().await?;
    Ok((run, events))
}

/// The card and its run, both held until the transaction ends, and the run's package.
async fn locked<'a>(
    env: &Env<'a>,
    tx: &mut dyn Tx,
    card_id: Uuid,
) -> Result<(Card, Run, &'a Package), OpError> {
    let card = tx
        .cards()
        .get_for_update(card_id)
        .await?
        .ok_or_else(|| OpError::NotFound(format!("card {card_id} does not exist")))?;
    let active = tx
        .runs()
        .active_for_card(card_id)
        .await?
        .ok_or_else(|| OpError::FailedPrecondition("the card has no run in progress".into()))?;
    let run = tx
        .runs()
        .get_for_update(active.id)
        .await?
        .ok_or_else(|| OpError::NotFound(format!("run {} does not exist", active.id)))?;
    let package = env.catalog.get(&run.workflow).ok_or_else(|| {
        OpError::FailedPrecondition(format!("workflow {} does not exist", run.workflow))
    })?;
    Ok((card, run, package))
}

/// The stage a running run waits on a result from.
fn running_stage(run: &Run) -> Result<String, OpError> {
    match (run.state, run.current_stage.clone()) {
        (RunState::Running, Some(stage)) => Ok(stage),
        (state, _) => Err(OpError::FailedPrecondition(format!(
            "the run is {}, so no stage is waiting for a result",
            state.as_str()
        ))),
    }
}

/// Checks the caller is the agent the stage's task mentioned. The agent is read from the
/// queued post, so it is known before the post reaches the relay and cannot change with the
/// directory.
async fn check_agent(tx: &mut dyn Tx, ctx: &Ctx, run: &Run, stage_id: &str) -> Result<(), OpError> {
    let queued = tx
        .outbox()
        .by_key(&task_key(run.card_id, stage_id, run.visit))
        .await?;
    let Some(Posting::StageTask {
        agent, agent_name, ..
    }) = queued.map(|row| row.posting)
    else {
        return Err(OpError::FailedPrecondition(format!(
            "stage {stage_id} has no task waiting for a result"
        )));
    };
    if ctx.actor.pubkey != agent {
        return Err(OpError::PermissionDenied(format!(
            "only the agent {agent_name} may report a result for stage {stage_id}"
        )));
    }
    Ok(())
}
