//! Carrying out a step of a run: saving the run, writing the posts and the events it asks
//! for, moving the card, and handing a finished card to the next workflow.
//!
//! Ported from the Go engine's `finish`, `land` and `finishRoute` (`orchestrator/routes.go`,
//! `orchestrator/landing.go`). The reducer in [`crate::rules::run`] decides; this module
//! only writes what it decided, inside the transaction the caller holds.

use std::collections::BTreeMap;

use chrono::{SubsecRound, Utc};
use serde_json::json;
use uuid::Uuid;

use super::task_message::{render_task, CardText};
use super::{Ctx, Env, OpError};
use crate::config::{self, Layer, Layers, Resolution};
use crate::domain::card::{Card, CardState};
use crate::domain::events::Event;
use crate::domain::run::{Run, RunState};
use crate::ports::store::{Posting, Tx};
use crate::rules::run::{
    entry_stage, step, task_key, Effect, Ended, Facts, Input, Note, Round, Step,
};
use crate::workflow::load::Package;
use crate::workflow::options::resolve_options;

/// Whether the step's run is new, or one already stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Mode {
    Insert,
    Update,
}

/// What the reducer needs to know about the card, read inside the transaction.
pub(super) async fn facts(
    tx: &mut dyn Tx,
    card: &Card,
    package: &Package,
) -> Result<Facts, OpError> {
    let options = tx.workflows().options().await?;
    let resolved = resolve_options(package, options.get(&package.meta.name));
    Ok(Facts {
        // Issue labels arrive with intake (Phase 16), and the plan with artifacts (Phase 9).
        labels: Vec::new(),
        paused: resolved.paused,
        visits: tx.runs().visit_counts(card.id).await?,
        plan: (0, 0),
    })
}

/// Every setting of one stage of the card's workflow, through the six layers. `parameters`
/// are the run's own values.
pub(super) async fn resolve_settings(
    env: &Env<'_>,
    tx: &mut dyn Tx,
    card: &Card,
    parameters: &BTreeMap<String, String>,
    stage: Option<&str>,
) -> Result<Resolution, OpError> {
    let package = env.catalog.get(&card.workflow);
    let layers = Layers {
        user: Layer::new(
            "community settings",
            tx.settings().list("engine", "").await?,
        ),
        workflow: Layer::new(
            format!("workflow {}", card.workflow),
            tx.settings().list("workflow", &card.workflow).await?,
        ),
        card: Layer::new(
            format!("card {}", card.title),
            tx.settings().list("card", &card.id.to_string()).await?,
        ),
        run: run_layer(parameters),
        params: package
            .map(|p| p.meta.parameters.clone())
            .unwrap_or_default(),
    };
    let stage = stage
        .and_then(|id| package.and_then(|p| p.stage(id)))
        .map(|(s, _)| s);
    Ok(config::resolve(&card.workflow, stage, &layers, false))
}

/// The run's parameter values as the run layer of the settings.
pub(super) fn run_layer(parameters: &BTreeMap<String, String>) -> Layer {
    Layer::new(
        "this run",
        parameters
            .iter()
            .map(|(name, value)| {
                (
                    format!("{}{name}", config::PARAM_PREFIX),
                    serde_json::Value::String(value.clone()),
                )
            })
            .collect(),
    )
}

/// Makes a run of `package` for `card`, starts it at `stage` (the first stage when `None`)
/// and carries out the first step. `held` makes the entry stage honor the workflow's
/// pauses. Nothing is written until the entry stage's agent is known, so a refusal leaves the
/// transaction as it was.
#[allow(clippy::too_many_arguments)]
pub(super) async fn begin_run(
    env: &Env<'_>,
    ctx: &Ctx,
    tx: &mut dyn Tx,
    card: &mut Card,
    package: &Package,
    stage: Option<&str>,
    parameters: BTreeMap<String, String>,
    held: bool,
) -> Result<Run, OpError> {
    entry_stage(package, stage).map_err(OpError::FailedPrecondition)?;
    let mut run = Run::new(card.id, &package.meta.name, &package.hash());
    run.parameters = parameters;
    let facts = facts(tx, card, package).await?;
    let stepped = step(
        &run,
        package,
        &facts,
        Input::Start {
            requester: Some(ctx.actor.pubkey.clone()),
            stage: stage.map(str::to_owned),
            held,
        },
    );
    apply(env, ctx, tx, card, package, stepped, Mode::Insert).await
}

/// Writes `stepped`: the run, the events and posts it asks for, and the card's new state. A
/// run that ended with the workflow done hands its card to the workflow's next one.
pub(super) async fn apply(
    env: &Env<'_>,
    ctx: &Ctx,
    tx: &mut dyn Tx,
    card: &mut Card,
    package: &Package,
    stepped: Step,
    mode: Mode,
) -> Result<Run, OpError> {
    let Step {
        mut run,
        effects,
        ended,
        reason,
    } = stepped;
    if ended.is_some() {
        run.ended_at = Some(Utc::now().trunc_subsecs(6));
    }
    match mode {
        Mode::Insert => tx.runs().insert(&run).await?,
        Mode::Update => tx.runs().update(&run).await?,
    }
    let state = card_state(&run, ended);
    let before = card.state;
    let changed = state != before;
    if changed {
        card.state = state;
        tx.cards().update(card).await?;
    }
    let mut state_event = None;
    if changed {
        let mut payload = json!({ "state": state.as_str(), "from": before.as_str() });
        if let Some(reason) = &reason {
            payload["reason"] = json!(reason);
        }
        state_event = Some(payload);
    }
    // A run's first events say the card moved before they say what it did there.
    if mode == Mode::Insert {
        if let Some(payload) = state_event.take() {
            append(tx, ctx, "card.state", &run, payload).await?;
        }
    }
    let channel = super::settings::update_channel(tx).await?;
    for effect in effects {
        match effect {
            Effect::Emit { kind, payload } => append(tx, ctx, kind, &run, payload).await?,
            Effect::PostStatus {
                card: id,
                key,
                text,
            } => {
                let posting = Posting::Status {
                    channel: need_channel(channel)?,
                    text: format!("{}: {text}", card.title),
                };
                tx.outbox().enqueue(&key, id, &posting).await?;
            }
            Effect::PostStageTask {
                card: id,
                stage,
                visit,
                agent: agent_name,
                key,
                round,
            } => {
                let channel = need_channel(channel)?;
                let agent = find_agent(env, ctx, &agent_name, &package.meta.name, &stage).await?;
                if !env
                    .directory
                    .member_of_channel(ctx.community, channel, &agent)
                    .await?
                {
                    return Err(OpError::FailedPrecondition(format!(
                        "add {agent_name} to the update channel, then try again"
                    )));
                }
                let text = task_text(env, tx, card, &run, package, &stage, round.as_ref()).await?;
                let posting = Posting::StageTask {
                    channel,
                    stage,
                    visit,
                    agent_name,
                    agent,
                    text,
                };
                tx.outbox().enqueue(&key, id, &posting).await?;
            }
            Effect::PostReply {
                card: id,
                stage,
                visit,
                agent: agent_name,
                key,
                note,
            } => {
                // The reply mentions the agent its thread's task mentioned.
                let agent = match tx.outbox().by_key(&task_key(id, &stage, visit)).await? {
                    Some(row) => match row.posting {
                        Posting::StageTask { agent, .. } => agent,
                        _ => return Err(no_thread(&stage)),
                    },
                    None => return Err(no_thread(&stage)),
                };
                let text = match &note {
                    Note::Round(round) => {
                        task_text(env, tx, card, &run, package, &stage, Some(round)).await?
                    }
                    Note::Resumed => format!(
                        "The card was resumed. Stage {stage} is running again: report its result with the Beehive tool `stage_done` with card_id `{id}`."
                    ),
                };
                let posting = Posting::StageReply {
                    channel: need_channel(channel)?,
                    stage,
                    visit,
                    agent_name,
                    agent,
                    text,
                };
                tx.outbox().enqueue(&key, id, &posting).await?;
            }
        }
    }
    if let Some(payload) = state_event {
        append(tx, ctx, "card.state", &run, payload).await?;
    }
    // A run that ends in the step that starts it, because its conditions skip every stage,
    // does not hand off, so two workflows that hand off to each other cannot loop.
    if ended == Some(Ended::Done) && mode == Mode::Update {
        hand_off(env, ctx, tx, card, package).await?;
    }
    Ok(run)
}

fn need_channel(channel: Option<Uuid>) -> Result<Uuid, OpError> {
    channel.ok_or_else(|| {
        OpError::FailedPrecondition(
            "no update channel is set; choose one in Settings (update_channel), then try again"
                .into(),
        )
    })
}

fn no_thread(stage: &str) -> OpError {
    OpError::FailedPrecondition(format!("stage {stage} has no task thread to reply in"))
}

/// The card's state while its run is in `run`'s state. A run that is held waits for a person
/// like one that asks a question, so the card shows `waiting` for both.
fn card_state(run: &Run, ended: Option<Ended>) -> CardState {
    match (run.state, ended) {
        (_, Some(Ended::Done)) => CardState::Done,
        (_, Some(Ended::Stopped)) => CardState::Stopped,
        (RunState::Waiting | RunState::Paused, _) => CardState::Waiting,
        _ => CardState::Running,
    }
}

/// The text of a stage's task, with the round's line when it is a later round.
async fn task_text(
    env: &Env<'_>,
    tx: &mut dyn Tx,
    card: &Card,
    run: &Run,
    package: &Package,
    stage: &str,
    round: Option<&Round>,
) -> Result<String, OpError> {
    let (def, _) = package.stage(stage).ok_or_else(|| {
        OpError::FailedPrecondition(format!(
            "workflow {} has no stage {stage}",
            package.meta.name
        ))
    })?;
    let params = resolve_settings(env, tx, card, &run.parameters, Some(stage))
        .await?
        .param_values();
    let mut text = render_task(
        package,
        def,
        &CardText {
            id: card.id,
            title: &card.title,
            description: &card.description,
        },
        &params,
    )
    .map_err(OpError::FailedPrecondition)?;
    if let Some(round) = round {
        text.push_str("\n\n");
        text.push_str(&round_line(round));
    }
    Ok(text)
}

/// What the agent is told about the round it is starting.
fn round_line(round: &Round) -> String {
    if round.plan.is_empty() {
        format!(
            "This is round {}. Carry on, and call `stage_done` with passed when the work is done.",
            round.number
        )
    } else {
        format!(
            "This is round {}. {} of {} items in the plan artifact {} are checked; carry on with the unchecked ones.",
            round.number, round.checked, round.total, round.plan
        )
    }
}

/// With the workflow done, the card takes the workflow's next one and starts at its first stage,
/// unless that stage is held. A next workflow that is not loaded, or whose first stage cannot
/// start, leaves the card done and says why in its activity.
async fn hand_off(
    env: &Env<'_>,
    ctx: &Ctx,
    tx: &mut dyn Tx,
    card: &mut Card,
    package: &Package,
) -> Result<(), OpError> {
    let options = tx.workflows().options().await?;
    let next = resolve_options(package, options.get(&package.meta.name)).next;
    if next.is_empty() {
        return Ok(());
    }
    let Some(target) = env.catalog.get(&next) else {
        return note(
            tx,
            ctx,
            card,
            format!(
                "{} hands finished cards to {next}, which is not loaded, so the card stayed done",
                package.meta.name
            ),
        )
        .await;
    };
    let refusal = match entry_stage(target, None) {
        Err(refusal) => Some(refusal),
        Ok((stage, agent)) => {
            entry_agent(env, ctx, tx, stage.id.as_str(), agent, &target.meta.name)
                .await?
                .err()
        }
    };
    if let Some(refusal) = refusal {
        return note(
            tx,
            ctx,
            card,
            format!(
                "{} hands finished cards to {next}, which cannot start: {refusal}. The card stayed done",
                package.meta.name
            ),
        )
        .await;
    }
    let from = std::mem::replace(&mut card.workflow, next.clone());
    tx.cards().update(card).await?;
    let at = Event::new(
        "card.moved",
        Some(card.id),
        None,
        ctx.actor.clone(),
        json!({ "workflow": next, "from": from, "reason": "handed off" }),
    );
    tx.events().append(at).await?;
    Box::pin(begin_run(
        env,
        ctx,
        tx,
        card,
        target,
        None,
        BTreeMap::new(),
        true,
    ))
    .await?;
    Ok(())
}

/// Whether `agent_name` can be mentioned for the first stage of a workflow: one Buzz agent
/// has the name and it is in the update channel. The inner error is a sentence for a person.
async fn entry_agent(
    env: &Env<'_>,
    ctx: &Ctx,
    tx: &mut dyn Tx,
    stage: &str,
    agent_name: &str,
    workflow: &str,
) -> Result<Result<(), String>, OpError> {
    let Some(channel) = super::settings::update_channel(tx).await? else {
        return Ok(Err("no update channel is set".into()));
    };
    let agent = match find_agent(env, ctx, agent_name, workflow, stage).await {
        Ok(agent) => agent,
        Err(OpError::FailedPrecondition(refusal)) => return Ok(Err(refusal)),
        Err(other) => return Err(other),
    };
    let member = env
        .directory
        .member_of_channel(ctx.community, channel, &agent)
        .await?;
    Ok(if member {
        Ok(())
    } else {
        Err(format!("{agent_name} is not in the update channel"))
    })
}

/// A line in the card's activity, as a note by the actor.
async fn note(tx: &mut dyn Tx, ctx: &Ctx, card: &Card, summary: String) -> Result<(), OpError> {
    let event = Event::new(
        "activity.added",
        Some(card.id),
        None,
        ctx.actor.clone(),
        json!({ "type": "note", "summary": summary, "detail": "" }),
    );
    tx.events().append(event).await?;
    Ok(())
}

pub(super) async fn append(
    tx: &mut dyn Tx,
    ctx: &Ctx,
    kind: &str,
    run: &Run,
    payload: serde_json::Value,
) -> Result<(), OpError> {
    let event = Event::new(
        kind,
        Some(run.card_id),
        Some(run.id),
        ctx.actor.clone(),
        payload,
    );
    tx.events().append(event).await?;
    Ok(())
}

/// The pubkey of the one Buzz agent named `name`. Two agents with one name are refused,
/// because the mention would reach whichever the relay happened to list first.
pub(super) async fn find_agent(
    env: &Env<'_>,
    ctx: &Ctx,
    name: &str,
    workflow: &str,
    stage: &str,
) -> Result<String, OpError> {
    let mut found = env.directory.agents_named(ctx.community, name).await?;
    match found.len() {
        0 => Err(OpError::FailedPrecondition(format!(
            "no Buzz agent is named {name}; create it in Buzz, or change the agent of stage {stage} in workflow {workflow}"
        ))),
        1 => Ok(found.remove(0).pubkey),
        n => {
            let keys: Vec<String> = found.into_iter().map(|a| a.pubkey).collect();
            Err(OpError::FailedPrecondition(format!(
                "{n} Buzz agents are named {name} ({}); rename all but one so the name is unique",
                keys.join(", ")
            )))
        }
    }
}
