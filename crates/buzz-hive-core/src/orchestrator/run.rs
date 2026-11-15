//! Starting a run and the inputs that move it. Every input to a run goes through the same
//! row lock, and each is one transaction: the run, the events, the queued posts and the
//! card's state commit together or not at all.

use std::collections::BTreeMap;

use uuid::Uuid;

use super::artifacts::missing_outputs;
use super::routes::{apply, begin_run, facts, resolve_settings, Mode};
use super::settings;
use super::{Ctx, Env, OpError};
use crate::access::ActorKind;
use crate::config::LayerKind;
use crate::domain::card::{Card, CardState};
use crate::domain::events::Event;
use crate::domain::run::{Run, RunState};
use crate::domain::workflow::RESULT_PASSED;
use crate::ports::store::{Posting, Tx};
use crate::rules::run::{entry_stage, step, task_key, Input};
use crate::tool_names::STAGE_DONE;
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
    let run = start_in(env, ctx, &mut *tx, card_id, stage, parameters).await?;
    let events = tx.commit().await?;
    Ok((run, events))
}

/// [`start_run_at`] inside a transaction the caller holds and commits.
pub(super) async fn start_in(
    env: &Env<'_>,
    ctx: &Ctx,
    tx: &mut dyn Tx,
    card_id: Uuid,
    stage: Option<&str>,
    parameters: BTreeMap<String, String>,
) -> Result<Run, OpError> {
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
    let package = env
        .catalog
        .get(&card.workflow)
        .ok_or_else(|| OpError::FailedPrecondition(env.catalog.absent(&card.workflow)))?;
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

    begin_run(
        env, ctx, &mut *tx, &mut card, package, stage, parameters, false,
    )
    .await
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
///
/// `for_stage` and `visit` name the stage and visit the report is for, as the task said them. A
/// report for any other stage or visit is a `FailedPrecondition`, so a late answer to an
/// earlier visit cannot move the card on from a later one.
pub async fn stage_done(
    env: &Env<'_>,
    ctx: &Ctx,
    card_id: Uuid,
    for_stage: &str,
    visit: u32,
    result: &str,
    summary: &str,
) -> Result<(Run, Vec<Event>), OpError> {
    require_position(for_stage, visit)?;
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
    check_position(&run, &stage_id, for_stage, visit)?;
    let allowed = reportable_results(stage);
    if !allowed.iter().any(|r| r == result) {
        return Err(OpError::InvalidArgument(format!(
            "{result:?} is not a result of stage {stage_id}; use one of: {}",
            allowed.join(", ")
        )));
    }
    let facts = facts(&mut *tx, &card, package, Some(&stage_id)).await?;
    let missing = if result == RESULT_PASSED {
        missing_outputs(&mut *tx, card_id, stage, run.visit).await?
    } else {
        Vec::new()
    };
    let input = if missing.is_empty() {
        Input::StageDone {
            stage: stage_id,
            result: result.to_owned(),
            summary: summary.to_owned(),
        }
    } else {
        Input::MissingOutput {
            stage: stage_id,
            missing,
            summary: summary.to_owned(),
        }
    };
    let stepped = step(&run, package, &facts, input);
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

/// Refuses a report that does not say which stage and visit it is for.
pub(super) fn require_position(stage: &str, visit: u32) -> Result<(), OpError> {
    if stage.is_empty() {
        return Err(OpError::InvalidArgument(
            "stage is required: the stage the task named".into(),
        ));
    }
    if visit == 0 {
        return Err(OpError::InvalidArgument(
            "visit is required: the visit the task named, from 1".into(),
        ));
    }
    Ok(())
}

/// Refuses a report for a stage or visit the run is no longer at.
pub(super) fn check_position(
    run: &Run,
    running: &str,
    stage: &str,
    visit: u32,
) -> Result<(), OpError> {
    if stage != running || visit != run.visit {
        return Err(OpError::FailedPrecondition(format!(
            "this report is for stage {stage} visit {visit}, but the run is at stage {running} visit {}; \
             the task you answered is out of date",
            run.visit
        )));
    }
    Ok(())
}

/// Refuses an agent a call that is a person's to make. An agent acts through the stage task
/// it was given, so it may report a result or move its own card, and may not hold, release or
/// skip a card, which would let one agent override the others on a board.
pub(super) fn people_only(ctx: &Ctx, what: &str) -> Result<(), OpError> {
    if ctx.actor.kind == ActorKind::Agent {
        return Err(OpError::PermissionDenied(format!(
            "only a person may {what}; an agent reports with {STAGE_DONE}"
        )));
    }
    Ok(())
}

/// A person moves a card that waits, or is held, on as if its stage had passed. An agent may
/// not.
pub async fn skip_stage(
    env: &Env<'_>,
    ctx: &Ctx,
    card_id: Uuid,
) -> Result<(Run, Vec<Event>), OpError> {
    people_only(ctx, "skip a stage")?;
    let mut tx = env.store.begin(ctx.community).await?;
    let (mut card, run, package) = locked(env, &mut *tx, card_id).await?;
    if !matches!(run.state, RunState::Waiting | RunState::Paused) {
        return Err(OpError::FailedPrecondition(format!(
            "the run is {}; only a card that waits or is held can skip its stage",
            run.state.as_str()
        )));
    }
    let facts = facts(&mut *tx, &card, package, run.current_stage.as_deref()).await?;
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

/// A person holds a running card. Its agent may not report until the card is resumed. An
/// agent may not hold a card.
pub async fn pause_card(
    env: &Env<'_>,
    ctx: &Ctx,
    card_id: Uuid,
) -> Result<(Run, Vec<Event>), OpError> {
    people_only(ctx, "pause a card")?;
    let mut tx = env.store.begin(ctx.community).await?;
    let (mut card, run, package) = locked(env, &mut *tx, card_id).await?;
    if run.state != RunState::Running {
        return Err(OpError::FailedPrecondition(format!(
            "the run is {}; only a running card can be paused",
            run.state.as_str()
        )));
    }
    let facts = facts(&mut *tx, &card, package, run.current_stage.as_deref()).await?;
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

/// A person lets a held stage start, or tells a paused stage's agent to carry on. An agent
/// may not.
pub async fn resume_card(
    env: &Env<'_>,
    ctx: &Ctx,
    card_id: Uuid,
) -> Result<(Run, Vec<Event>), OpError> {
    people_only(ctx, "resume a card")?;
    let mut tx = env.store.begin(ctx.community).await?;
    let (mut card, run, package) = locked(env, &mut *tx, card_id).await?;
    if run.state != RunState::Paused {
        return Err(OpError::FailedPrecondition(format!(
            "the run is {}; only a paused card can be resumed",
            run.state.as_str()
        )));
    }
    let facts = facts(&mut *tx, &card, package, run.current_stage.as_deref()).await?;
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

/// Stops a run that has not ended: its stage's agent is told to stop in the stage's thread, the
/// run ends, and the card is `stopped`. A person or an agent may stop a run.
pub async fn stop_run(
    env: &Env<'_>,
    ctx: &Ctx,
    run_id: Uuid,
) -> Result<(Run, Vec<Event>), OpError> {
    let mut tx = env.store.begin(ctx.community).await?;
    let found = tx
        .runs()
        .get(run_id)
        .await?
        .ok_or_else(|| OpError::NotFound(format!("run {run_id} does not exist")))?;
    let run = stop_in(env, ctx, &mut *tx, found.card_id, Some(run_id)).await?;
    let events = tx.commit().await?;
    Ok((run, events))
}

/// Stops the card's run in a transaction the caller holds. With `only`, the run must be that one.
pub(super) async fn stop_in(
    env: &Env<'_>,
    ctx: &Ctx,
    tx: &mut dyn Tx,
    card_id: Uuid,
    only: Option<Uuid>,
) -> Result<Run, OpError> {
    let (mut card, run, package) = locked(env, &mut *tx, card_id).await.map_err(|e| match e {
        OpError::FailedPrecondition(_) if only.is_some() => {
            OpError::FailedPrecondition("the run has ended, so there is nothing to stop".into())
        }
        other => other,
    })?;
    if only.is_some_and(|id| id != run.id) {
        return Err(OpError::FailedPrecondition(
            "the run has ended, so there is nothing to stop".into(),
        ));
    }
    let who = match ctx.actor.kind {
        ActorKind::Agent => "an agent",
        _ => "a person",
    };
    let facts = facts(&mut *tx, &card, package, run.current_stage.as_deref()).await?;
    let stepped = step(
        &run,
        package,
        &facts,
        Input::Cancel {
            reason: format!("{who} stopped the run"),
        },
    );
    apply(
        env,
        ctx,
        &mut *tx,
        &mut card,
        package,
        stepped,
        Mode::Update,
    )
    .await
}

/// Starts the next attempt of a run that failed or was stopped: a new run of the card, at the
/// stage the old one ended at, with the old run's parameters. A new run opens a new thread.
///
/// The run must be the card's newest, and the card must be `failed` or `stopped`.
pub async fn retry_run(
    env: &Env<'_>,
    ctx: &Ctx,
    run_id: Uuid,
) -> Result<(Run, Vec<Event>), OpError> {
    let mut tx = env.store.begin(ctx.community).await?;
    let old = tx
        .runs()
        .get(run_id)
        .await?
        .ok_or_else(|| OpError::NotFound(format!("run {run_id} does not exist")))?;
    let card = tx
        .cards()
        .get_for_update(old.card_id)
        .await?
        .ok_or_else(|| OpError::NotFound(format!("card {} does not exist", old.card_id)))?;
    let newest = tx
        .runs()
        .list(Some(card.id))
        .await?
        .first()
        .map(|run| run.id);
    if old.state != RunState::Ended
        || newest != Some(run_id)
        || !matches!(card.state, CardState::Failed | CardState::Stopped)
    {
        return Err(OpError::FailedPrecondition(format!(
            "only the newest run of a card that failed or was stopped can be retried; this run is {} and the card is {}",
            old.state.as_str(),
            card.state.as_str()
        )));
    }
    let run = start_in(
        env,
        ctx,
        &mut *tx,
        card.id,
        old.current_stage.as_deref(),
        old.parameters.clone(),
    )
    .await?;
    let events = tx.commit().await?;
    Ok((run, events))
}

/// A person puts a card back to idle: its run, if it has one, is stopped first. Artifacts are
/// kept. An agent may not.
pub async fn reset_card(
    env: &Env<'_>,
    ctx: &Ctx,
    card_id: Uuid,
) -> Result<(Card, Vec<Event>), OpError> {
    people_only(ctx, "reset a card")?;
    let mut tx = env.store.begin(ctx.community).await?;
    let mut card = tx
        .cards()
        .get_for_update(card_id)
        .await?
        .ok_or_else(|| OpError::NotFound(format!("card {card_id} does not exist")))?;
    if tx.runs().active_for_card(card_id).await?.is_some() {
        stop_in(env, ctx, &mut *tx, card_id, None).await?;
        card = tx
            .cards()
            .get(card_id)
            .await?
            .ok_or_else(|| OpError::NotFound(format!("card {card_id} does not exist")))?;
    }
    if card.state != CardState::Idle {
        let from = card.state;
        card.state = CardState::Idle;
        tx.cards().update(&card).await?;
        let event = Event::new(
            "card.state",
            Some(card.id),
            None,
            ctx.actor.clone(),
            serde_json::json!({ "state": card.state.as_str(), "from": from.as_str(), "reason": "a person reset the card" }),
        );
        tx.events().append(event).await?;
    }
    let events = tx.commit().await?;
    Ok((card, events))
}

/// A person holds every running run, one transaction each, and the count of those held. A run
/// that changed state in the meantime is left alone. An agent may not.
pub async fn pause_all(env: &Env<'_>, ctx: &Ctx) -> Result<(u32, Vec<Event>), OpError> {
    people_only(ctx, "pause every run")?;
    let mut tx = env.store.begin(ctx.community).await?;
    let cards: Vec<Uuid> = tx
        .runs()
        .list(None)
        .await?
        .into_iter()
        .filter(|run| run.state == RunState::Running)
        .map(|run| run.card_id)
        .collect();
    drop(tx);
    let (mut paused, mut events) = (0, Vec::new());
    for card in cards {
        match pause_card(env, ctx, card).await {
            Ok((_, committed)) => {
                paused += 1;
                events.extend(committed);
            }
            Err(OpError::FailedPrecondition(_) | OpError::NotFound(_)) => {}
            Err(other) => return Err(other),
        }
    }
    Ok((paused, events))
}

/// The stage's agent moves the card to `target`, in place of the route its result would give.
/// A workflow allows it with `side_effects.agent_moves_cards`.
///
/// `for_stage` and `visit` name the stage and visit the move is made from, as the task said them,
/// and are checked as [`stage_done`] checks them. `target` is the stage to move to.
pub async fn move_card(
    env: &Env<'_>,
    ctx: &Ctx,
    card_id: Uuid,
    for_stage: &str,
    visit: u32,
    target: &str,
) -> Result<(Run, Vec<Event>), OpError> {
    require_position(for_stage, visit)?;
    let mut tx = env.store.begin(ctx.community).await?;
    let (mut card, run, package) = locked(env, &mut *tx, card_id).await?;
    let stage_id = running_stage(&run)?;
    check_agent(&mut *tx, ctx, &run, &stage_id).await?;
    check_position(&run, &stage_id, for_stage, visit)?;
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
    let facts = facts(&mut *tx, &card, package, Some(&stage_id)).await?;
    // A move with no result counts as passed for the stage's required outputs.
    let (stage, _) = package.stage(&stage_id).ok_or_else(|| {
        OpError::FailedPrecondition(format!("workflow {} has no stage {stage_id}", run.workflow))
    })?;
    let missing = missing_outputs(&mut *tx, card_id, stage, run.visit).await?;
    let input = if missing.is_empty() {
        Input::Move {
            stage: target.to_owned(),
        }
    } else {
        Input::MissingOutput {
            stage: stage_id,
            missing,
            summary: String::new(),
        }
    };
    let stepped = step(&run, package, &facts, input);
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
pub(super) async fn locked<'a>(
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
    let package = env
        .catalog
        .get(&run.workflow)
        .ok_or_else(|| OpError::FailedPrecondition(env.catalog.absent(&run.workflow)))?;
    Ok((card, run, package))
}

/// The stage a running run waits on a result from.
pub(super) fn running_stage(run: &Run) -> Result<String, OpError> {
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
pub(super) async fn check_agent(
    tx: &mut dyn Tx,
    ctx: &Ctx,
    run: &Run,
    stage_id: &str,
) -> Result<(), OpError> {
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
