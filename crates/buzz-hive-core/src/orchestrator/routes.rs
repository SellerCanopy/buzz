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
use super::task_message::{render_task, with_card_link, CardText, Handoff, IssueText};
use super::{activity, artifacts};
use super::{Ctx, Env, OpError};
use crate::access::ActorKind;
use crate::config::{self, Layer, Layers, Resolution};
use crate::domain::card::{Card, CardState};
use crate::domain::events::Event;
use crate::domain::inbox::{InboxItem, ANSWER_SUPERSEDED};
use crate::domain::run::{Run, RunState};
use crate::domain::timer::TimerKind;
use crate::domain::workflow::Duration as SettingDuration;
use crate::ports::store::{Posting, Pubkey, PullCall, Tx};
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
    // A card made from an issue has the issue's labels as last read, and the issue's repo; any
    // other has neither.
    let issue = tx.issues().by_card(card.id).await?;
    let repo = issue.as_ref().map(|row| row.issue.repo.to_string());
    let labels = issue.map(|row| row.issue.labels).unwrap_or_default();
    let pull_request = tx
        .pull_requests()
        .by_card(card.id)
        .await?
        .map(|pull| (pull.repo, pull.number));
    Ok(Facts {
        repo,
        pull_request,
        labels,
        paused: resolved.paused,
        visits: tx.runs().visit_counts(card.id).await?,
        plan,
        execution,
        auto_mode: auto_mode_of(tx, card.id).await?,
    })
}

/// Whether the card is in auto mode.
pub(super) async fn auto_mode_of(tx: &mut dyn Tx, card: Uuid) -> Result<bool, OpError> {
    let value = tx
        .settings()
        .get("card", &card.to_string(), config::KEY_AUTO_MODE)
        .await?;
    Ok(value.and_then(|v| v.as_bool()).unwrap_or(false))
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

/// The person a run started by `ctx` is for: the person an agent acts for, or the person
/// who asked. An agent with no claim, and the engine's own account, name no one, so a run
/// never records an agent as the person it acts for.
fn requester_of(ctx: &Ctx) -> Option<Pubkey> {
    ctx.requester
        .clone()
        .or_else(|| (ctx.actor.kind == ActorKind::Person).then(|| ctx.actor.pubkey.clone()))
}

/// `ctx` for a run that someone other than a person starts on a run's behalf, such as auto
/// mode retrying it or a finished workflow handing its card on: the new run keeps `earlier`'s
/// requester. A person who asks keeps their own place as the requester.
pub(super) fn carrying_requester(ctx: &Ctx, earlier: &Run) -> Ctx {
    let mut carried = ctx.clone();
    if ctx.actor.kind != ActorKind::Person && carried.requester.is_none() {
        carried.requester = earlier.requester.clone();
    }
    carried
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
            requester: requester_of(ctx),
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
    let opens_item = effects
        .iter()
        .any(|effect| matches!(effect, Effect::OpenInbox { .. }));
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
    // A card in auto mode may answer its own items below, and each answer posts to the next
    // stage's agent. Ask for the workflow's agents now, before the first append, so those
    // answers find them here.
    if auto_mode_of(tx, card.id).await? {
        if let Some(channel) = channel {
            ask_directory(env, ctx, package, channel).await?;
            // An answer auto mode gives may finish the workflow and hand the card on.
            ask_next(env, ctx, tx, package, channel).await?;
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
    if let Some(last) = run.steps.last_mut().filter(|s| s.entered_at.is_none()) {
        last.entered_at = Some(Utc::now().trunc_subsecs(6));
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
    // A step that changes a run closes the items that waited on it, and the effects below open
    // the ones the new state needs. An item a person answered is closed already.
    if mode == Mode::Insert || !effects.is_empty() {
        let closed = tx
            .inbox()
            .resolve_card(card.id, ANSWER_SUPERSEDED, &ctx.actor.pubkey)
            .await?;
        for item in closed {
            append(
                tx,
                ctx,
                "inbox.resolved",
                &run,
                resolved_payload(&item, ANSWER_SUPERSEDED, &ctx.actor.pubkey),
            )
            .await?;
        }
    }
    for effect in effects {
        match effect {
            Effect::Emit { kind, payload } => append(tx, ctx, kind, &run, payload).await?,
            Effect::OpenInbox {
                stage,
                visit,
                kind,
                reason,
                artifacts,
            } => {
                let mut listed = Vec::new();
                for name in artifacts {
                    let version = tx.artifacts().version(card.id, &name, None).await?;
                    listed.push(json!({ "name": name, "version": version.map(|v| v.version) }));
                }
                let item = InboxItem::open(
                    card.id,
                    run.id,
                    &stage,
                    visit,
                    kind,
                    &reason,
                    json!({ "artifacts": listed }),
                );
                tx.inbox().insert(&item).await?;
                append(
                    tx,
                    ctx,
                    "inbox.opened",
                    &run,
                    json!({
                        "id": item.id,
                        "kind": kind.as_str(),
                        "stage": stage,
                        "visit": visit,
                        "title": reason,
                    }),
                )
                .await?;
            }
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
            Effect::OpenPullRequest {
                card: id,
                stage,
                visit,
                repo,
                key,
            } => {
                let posting = super::pull_request::posting(
                    env,
                    tx,
                    card,
                    &run,
                    package,
                    (&stage, visit),
                    &repo,
                )
                .await?;
                tx.outbox().enqueue(&key, id, &posting).await?;
            }
            Effect::PullCall {
                card: id,
                stage,
                visit,
                repo,
                number,
                call,
                key,
            } => {
                // A merge goes to the commit whose checks the sync last read. A card with none
                // read merges unpinned, as the host's own rules decide.
                let call = match call {
                    PullCall::Merge {
                        method,
                        head_sha: None,
                    } => PullCall::Merge {
                        method,
                        head_sha: tx
                            .pull_requests()
                            .by_card(id)
                            .await?
                            .map(|pull| pull.head_sha)
                            .filter(|sha| !sha.is_empty()),
                    },
                    other => other,
                };
                let posting = Posting::PullCall {
                    stage,
                    visit,
                    repo,
                    number,
                    call,
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
                        // An action stage has no agent to tell.
                        _ if matches!(note, Note::Stopped { .. }) => continue,
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
                    Note::Answer { text } if text.is_empty() => format!(
                    "A person asks you to carry on with stage {stage}. Report the result with the Beehive tool `{STAGE_DONE}` with card_id `{id}`, stage `{stage}` and visit {visit}."
                    ),
                    Note::Answer { text } => format!(
                    "A person answered: {text}\n\nCarry on with stage {stage}, and report the result with the Beehive tool `{STAGE_DONE}` with card_id `{id}`, stage `{stage}` and visit {visit}."
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
        hand_off(env, ctx, tx, card, &run, next).await?;
    }
    // A card in auto mode answers the item this step opened, and whatever the answer opens in
    // turn. The run and the card are then as the answers left them.
    if opens_item && Box::pin(super::auto::settle(env, ctx, tx, card.id)).await? {
        if let Some(current) = tx.cards().get(card.id).await? {
            *card = current;
        }
        let latest = match tx.runs().active_for_card(card.id).await? {
            Some(active) => Some(active),
            None => tx.runs().get(run.id).await?,
        };
        if let Some(latest) = latest {
            run = latest;
        }
    }
    Ok(run)
}

/// Asks the directory for what a step on the workflow's card may need: that the update channel
/// exists, that the Beehive account and each agent the stages name are in it. A step that goes
/// on to answer an item, or to start a run, finds the answers here instead of asking after its
/// first append, which takes the log lock. An agent that cannot be found is left for the step
/// that needs it to refuse.
pub(super) async fn ask_directory(
    env: &Env<'_>,
    ctx: &Ctx,
    package: &Package,
    channel: Uuid,
) -> Result<(), OpError> {
    env.directory.channel_exists(ctx.community, channel).await?;
    env.directory
        .member_of_channel(ctx.community, channel, env.hive)
        .await?;
    for stage in &package.workflow.stages {
        let Some(name) = stage.agent.as_deref() else {
            continue;
        };
        match find_agent(env, ctx, name, &package.meta.name, &stage.id).await {
            Ok(agent) => {
                env.directory
                    .member_of_channel(ctx.community, channel, &agent)
                    .await?;
            }
            Err(OpError::FailedPrecondition(_)) => {}
            Err(other) => return Err(other),
        }
    }
    Ok(())
}

/// Asks the directory what the workflow `package` hands finished cards to will need, for a
/// step that appends before it knows whether the workflow ends. Nothing is asked when the
/// workflow has no next one, or it is not loaded.
pub(super) async fn ask_next(
    env: &Env<'_>,
    ctx: &Ctx,
    tx: &mut dyn Tx,
    package: &Package,
    channel: Uuid,
) -> Result<(), OpError> {
    let options = tx.workflows().options().await?;
    let next = resolve_options(package, options.get(&package.meta.name)).next;
    match env.catalog.get(&next) {
        Some(target) if !next.is_empty() => ask_directory(env, ctx, target, channel).await,
        _ => Ok(()),
    }
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
        issue: tx.issues().by_card(card.id).await?.map(|row| IssueText {
            repo: row.issue.repo.to_string(),
            number: row.issue.number,
            url: row.issue.url,
        }),
        // Only the stage's first task carries them: a later round is a reply, not a new brief.
        sendback: match round {
            None => run
                .steps
                .iter()
                .rev()
                .find(|s| s.stage == stage && s.visit == visit)
                .map(|s| s.sendback.clone())
                .unwrap_or_default(),
            Some(_) => String::new(),
        },
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
    finished: &Run,
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
        &carrying_requester(ctx, finished),
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

/// The payload of an `inbox.resolved` event.
pub(super) fn resolved_payload(item: &InboxItem, answer: &str, by: &str) -> serde_json::Value {
    json!({
        "id": item.id,
        "kind": item.kind.as_str(),
        "stage": item.stage,
        "answer": answer,
        "by": by,
    })
}

pub(super) async fn append(
    tx: &mut dyn Tx,
    ctx: &Ctx,
    kind: &str,
    run: &Run,
    payload: serde_json::Value,
) -> Result<(), OpError> {
    // One line per transition, so a stuck or misrouted run can be followed from the log. It is
    // written before the commit: a step that rolls back, such as an auto answer the savepoint
    // undoes, still shows what it tried.
    tracing::info!(
        kind,
        card = %run.card_id,
        run = %run.id,
        stage = payload["stage"].as_str().unwrap_or(""),
        visit = payload["visit"].as_u64().unwrap_or(0),
        actor = %ctx.actor.pubkey,
        "run event"
    );
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
