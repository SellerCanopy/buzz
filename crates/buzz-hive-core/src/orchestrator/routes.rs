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

use super::lookup::Memo;
use super::task_message::{render_task, with_card_link, CardText, Handoff};
use super::{activity, artifacts};
use super::{Ctx, Env, OpError};
use crate::config::{self, Layer, Layers, Resolution};
use crate::domain::card::{Card, CardState};
use crate::domain::events::Event;
use crate::domain::run::{Run, RunState};
use crate::domain::timer::TimerKind;
use crate::domain::workflow::Duration as SettingDuration;
use crate::ports::store::{Posting, Tx};
use crate::rules::run::{
    entry_stage, step, task_key, Effect, Ended, Execution, Facts, Input, Note, Round, Step,
};
use crate::tool_names::STAGE_DONE;
use crate::workflow::load::Package;
use crate::workflow::options::resolve_options;

/// Whether the step's run is new, or one already stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Mode {
    Insert,
    Update,
}

/// What the reducer needs to know about the card, read inside the transaction. `stage` is
/// the run's current stage, whose repeat plan the facts count.
pub(super) async fn facts(
    tx: &mut dyn Tx,
    card: &Card,
    package: &Package,
    stage: Option<&str>,
) -> Result<Facts, OpError> {
    let options = tx.workflows().options().await?;
    let resolved = resolve_options(package, options.get(&package.meta.name));
    let plan = match stage.and_then(|id| package.stage(id)) {
        Some((stage, _)) => artifacts::plan_progress(tx, card.id, stage).await?,
        None => (0, 0),
    };
    let layers = layers(tx, card, Some(package), &BTreeMap::new()).await?;
    let mut execution = BTreeMap::new();
    for stage in &package.workflow.stages {
        let resolution = config::resolve(&card.workflow, Some(stage), &layers, false);
        execution.insert(stage.id.clone(), execution_from(&resolution));
    }
    Ok(Facts {
        // Issue labels arrive with intake (Phase 16).
        labels: Vec::new(),
        paused: resolved.paused,
        visits: tx.runs().visit_counts(card.id).await?,
        plan,
        execution,
    })
}

/// A stage's timeout and retries from its resolved settings. A value that does not parse was
/// refused when it was saved, so it falls back to the engine default here.
fn execution_from(resolution: &Resolution) -> Execution {
    let wait = |key: &str| {
        resolution
            .value(key)
            .and_then(|v| v.as_str())
            .and_then(|text| SettingDuration::parse(text).ok())
            .and_then(|d| u64::try_from(d.nanos()).ok())
            .map(std::time::Duration::from_nanos)
            .unwrap_or_default()
    };
    Execution {
        timeout: wait("execution.timeout"),
        retries: resolution
            .value("execution.retries.max")
            .and_then(|v| v.as_u64())
            .and_then(|n| u32::try_from(n).ok())
            .unwrap_or_default(),
        backoff: wait("execution.retries.backoff"),
    }
}

/// The settings layers of the card's workflow, as stored. `parameters` are the run's own values.
async fn layers(
    tx: &mut dyn Tx,
    card: &Card,
    package: Option<&Package>,
    parameters: &BTreeMap<String, String>,
) -> Result<Layers, OpError> {
    Ok(Layers {
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
    let layers = layers(tx, card, package, parameters).await?;
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
    let facts = facts(tx, card, package, None).await?;
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
///
/// The first event appended takes the community's log lock, which is held to the commit. So
/// every answer the relay's directory has to give, which agent a stage names, whether it is in
/// the channel, and whether the next workflow can start, is asked for before that append.
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
    let directory = Memo::new(env.directory);
    let env = &Env {
        directory: &directory,
        ..*env
    };
    let channel = super::settings::update_channel(tx).await?;
    for effect in &effects {
        if let Effect::PostStageTask {
            stage,
            agent: agent_name,
            ..
        } = effect
        {
            let channel = need_channel(channel)?;
            let agent = find_agent(env, ctx, agent_name, &package.meta.name, stage).await?;
            env.directory
                .member_of_channel(ctx.community, channel, &agent)
                .await?;
        }
    }
    let next = if ended == Some(Ended::Done) && mode == Mode::Update {
        Some(next_workflow(env, ctx, tx, package).await?)
    } else {
        None
    };
    if ended.is_some() {
        run.ended_at = Some(Utc::now().trunc_subsecs(6));
    }
    match mode {
        Mode::Insert => tx.runs().insert(&run).await?,
        Mode::Update => tx.runs().update(&run).await?,
    }
    // Every change to a run retires its timers, and the effects that follow set the ones the
    // new state needs: a stage that waits, ends or is held has none, and one that runs has the
    // timeout it just started.
    if !effects.is_empty() {
        tx.timers().clear_run(run.id).await?;
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
                    text: with_card_link(&format!("{}: {text}", card.title), id),
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
                let text = task_text(
                    env,
                    tx,
                    card,
                    &run,
                    package,
                    &stage,
                    visit,
                    channel,
                    round.as_ref(),
                )
                .await?;
                let posting = Posting::StageTask {
                    channel,
                    stage,
                    visit,
                    agent_name,
                    agent,
                    // The first post of a thread, so a person who finds it in the channel can
                    // open the card.
                    text: with_card_link(&text, id),
                };
                tx.outbox().enqueue(&key, id, &posting).await?;
            }
            Effect::ArmTimeout { stage, after } => {
                tx.timers()
                    .arm(run.id, &stage, TimerKind::Timeout, after)
                    .await?;
            }
            Effect::ArmRetry { stage, after } => {
                tx.timers()
                    .arm(run.id, &stage, TimerKind::Retry, after)
                    .await?;
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
                    // A stage that was held or never launched has no thread, and nobody to tell
                    // to stop.
                    None if matches!(note, Note::Stopped { .. }) => continue,
                    None => return Err(no_thread(&stage)),
                };
                let text = match &note {
                    Note::Round(round) => {
                        task_text(
                            env,
                            tx,
                            card,
                            &run,
                            package,
                            &stage,
                            visit,
                            need_channel(channel)?,
                            Some(round),
                        )
                        .await?
                    }
                    Note::Retry { reason, attempt } => format!(
                        "Attempt {attempt} of stage {stage} failed: {reason}. Please try again in this thread, and report the result with the Beehive tool `{STAGE_DONE}` with card_id `{id}`, stage `{stage}` and visit {visit}."
                    ),
                    Note::Stopped { reason } => format!(
                        "The run was stopped: {reason}. Stop work on stage {stage} and do not report a result."
                    ),
                    Note::Resumed => format!(
                        "The card was resumed. Stage {stage} is running again: report its result with the Beehive tool `{STAGE_DONE}` with card_id `{id}`, stage `{stage}` and visit {visit}."
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
    if let Some(next) = next {
        hand_off(env, ctx, tx, card, next).await?;
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
        (_, Some(Ended::Stopped | Ended::Cancelled)) => CardState::Stopped,
        (_, Some(Ended::Failed)) => CardState::Failed,
        (RunState::Waiting | RunState::Paused, _) => CardState::Waiting,
        _ => CardState::Running,
    }
}

/// The text of a stage's task, with the round's line when it is a later round. `visit` is the
/// visit the task opens, and `channel` the channel it is posted in.
#[allow(clippy::too_many_arguments)]
async fn task_text(
    env: &Env<'_>,
    tx: &mut dyn Tx,
    card: &Card,
    run: &Run,
    package: &Package,
    stage: &str,
    visit: u32,
    channel: Uuid,
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
    let handoff = Handoff {
        inputs: artifacts::task_inputs(tx, card.id, def).await?,
        brief: if visit > 1 {
            activity::brief_for(tx, card.id, stage).await?
        } else {
            String::new()
        },
        channel: Some(channel),
        visit,
    };
    let mut text = render_task(
        package,
        def,
        &CardText {
            id: card.id,
            title: &card.title,
            description: &card.description,
        },
        &params,
        &handoff,
    )
    .map_err(OpError::FailedPrecondition)?;
    if let Some(round) = round {
        text.push_str("\n\n");
        text.push_str(&round_line(round, stage, visit));
    }
    Ok(text)
}

/// What the agent is told about the round it is starting.
fn round_line(round: &Round, stage: &str, visit: u32) -> String {
    if round.plan.is_empty() {
        format!(
            "This is round {}. Carry on, and call `{STAGE_DONE}` with stage `{stage}`, visit {visit} and result passed when the work is done.",
            round.number
        )
    } else {
        format!(
            "This is round {}. {} of {} items in the plan artifact {} are checked; carry on with the unchecked ones.",
            round.number, round.checked, round.total, round.plan
        )
    }
}

/// What becomes of a card whose workflow is done.
enum Next<'a> {
    /// The workflow hands nothing on.
    Nothing,
    /// The card goes to the workflow of this name, whose first stage can start.
    Start(String, &'a Package),
    /// The card stays done, and this is what its activity says.
    Stay(String),
}

/// Decides, before anything is written, where a card whose workflow is done goes next: the
/// workflow's next one, unless that is not loaded or its first stage cannot start.
async fn next_workflow<'a>(
    env: &Env<'a>,
    ctx: &Ctx,
    tx: &mut dyn Tx,
    package: &Package,
) -> Result<Next<'a>, OpError> {
    let options = tx.workflows().options().await?;
    let next = resolve_options(package, options.get(&package.meta.name)).next;
    if next.is_empty() {
        return Ok(Next::Nothing);
    }
    let Some(target) = env.catalog.get(&next) else {
        return Ok(Next::Stay(format!(
            "{} hands finished cards to {next}, which is not loaded, so the card stayed done",
            package.meta.name
        )));
    };
    let refusal = match entry_stage(target, None) {
        Err(refusal) => Some(refusal),
        Ok((stage, agent)) => {
            entry_agent(env, ctx, tx, stage.id.as_str(), agent, &target.meta.name)
                .await?
                .err()
        }
    };
    Ok(match refusal {
        Some(refusal) => Next::Stay(format!(
            "{} hands finished cards to {next}, which cannot start: {refusal}. The card stayed done",
            package.meta.name
        )),
        None => Next::Start(next, target),
    })
}

/// With the workflow done, the card takes the workflow's next one and starts at its first stage,
/// unless that stage is held. A next workflow that is not loaded, or whose first stage cannot
/// start, leaves the card done and says why in its activity.
async fn hand_off(
    env: &Env<'_>,
    ctx: &Ctx,
    tx: &mut dyn Tx,
    card: &mut Card,
    next: Next<'_>,
) -> Result<(), OpError> {
    let (next, target) = match next {
        Next::Nothing => return Ok(()),
        Next::Stay(summary) => return note(tx, ctx, card, summary).await,
        Next::Start(next, target) => (next, target),
    };
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
