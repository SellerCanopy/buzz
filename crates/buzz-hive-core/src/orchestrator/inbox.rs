//! Needs you: answering the places a run waits for a person, and the agent's way to ask.
//!
//! An item is opened by the step that makes the run wait (see `routes::apply`) and closed by
//! the answer, or by whatever else moves the run on. An answer is one transaction: the item,
//! the run, the events and the posts to the agent commit together.

use std::collections::BTreeMap;

use serde_json::json;
use uuid::Uuid;

use super::auto::{artifacts_of, ACTIVITY_AUTO};
use super::lookup::Memo;
use super::routes::{
    append, apply, ask_directory, ask_next, carrying_requester, facts, resolved_payload, Mode,
};
use super::run::{
    check_agent, check_position, locked, people_only, require_position, running_stage, start_in,
    stop_in,
};
use super::settings::update_channel;
use super::{Ctx, Env, OpError};
use crate::access::ActorKind;
use crate::domain::card::Card;
use crate::domain::events::Event;
use crate::domain::inbox::{
    InboxItem, InboxKind, ANSWER_APPROVE, ANSWER_DISMISS, ANSWER_REJECT, ANSWER_REPLY,
    ANSWER_RETRY, ANSWER_SKIP, ANSWER_STOP,
};
use crate::domain::run::{Run, RunState};
use crate::ports::store::{Posting, Store, Tx};
use crate::rules::auto::{answer_gate, AutoAnswer};
use crate::rules::run::{step, task_key, Approved, Input};

/// The longest reason, reply or question, in characters.
pub const MAX_TEXT_CHARS: usize = 10_000;
/// The longest message a person sends a running agent, in bytes.
pub const MAX_MESSAGE_BYTES: usize = 32 * 1024;
/// The longest summary an answer leaves in the card's activity, in characters.
const MAX_SUMMARY_CHARS: usize = 200;

/// The card's items, or every card's when `card` is `None`. Open items come first, and each
/// group is newest first. Resolved items are left out unless `include_resolved`.
pub async fn list(
    store: &dyn Store,
    ctx: &Ctx,
    card: Option<Uuid>,
    include_resolved: bool,
) -> Result<Vec<InboxItem>, OpError> {
    let mut tx = store.begin(ctx.community).await?;
    Ok(tx.inbox().list(card, include_resolved).await?)
}

/// A person answers the open item `id` with one of its choices. `text` is the reason for
/// `reject` and the reply for `reply`; the other choices ignore it. The item comes back as
/// resolved.
pub async fn answer(
    env: &Env<'_>,
    ctx: &Ctx,
    id: Uuid,
    choice: &str,
    text: &str,
) -> Result<(InboxItem, Vec<Event>), OpError> {
    people_only(ctx, "answer an item in Needs you")?;
    let mut tx = env.store.begin(ctx.community).await?;
    let found = tx
        .inbox()
        .get(id)
        .await?
        .ok_or_else(|| OpError::NotFound(format!("item {id} does not exist")))?;
    let (_, item) = decide(
        env,
        ctx,
        &mut *tx,
        found.card_id,
        Some(id),
        choice,
        text,
        &BTreeMap::new(),
        None,
    )
    .await?;
    let events = tx.commit().await?;
    Ok((item, events))
}

/// Approves the approval stage the card waits at. `versions` names a version to approve for an
/// artifact the stage lists; the others are approved at their latest.
pub async fn approve(
    env: &Env<'_>,
    ctx: &Ctx,
    card_id: Uuid,
    versions: &BTreeMap<String, i32>,
) -> Result<(Card, Vec<Event>), OpError> {
    let mut tx = env.store.begin(ctx.community).await?;
    let (card, _) = decide(
        env,
        ctx,
        &mut *tx,
        card_id,
        None,
        ANSWER_APPROVE,
        "",
        versions,
        None,
    )
    .await?;
    let events = tx.commit().await?;
    Ok((card, events))
}

/// Sends the approval stage the card waits at back, with comments. The stage the card goes to
/// gets them with its task. A person only.
pub async fn send_back(
    env: &Env<'_>,
    ctx: &Ctx,
    card_id: Uuid,
    comments: &str,
) -> Result<(Card, Vec<Event>), OpError> {
    people_only(ctx, "send a card back")?;
    let mut tx = env.store.begin(ctx.community).await?;
    let (card, _) = decide(
        env,
        ctx,
        &mut *tx,
        card_id,
        None,
        ANSWER_REJECT,
        comments,
        &BTreeMap::new(),
        None,
    )
    .await?;
    let events = tx.commit().await?;
    Ok((card, events))
}

/// Answers an item, or the card's open approval when `item` is `None`, inside the caller's
/// transaction. Returns the card as it is after the answer, and the item as resolved. `auto` is
/// the reason auto mode gives when the engine answers: it is recorded as an `auto` entry in the
/// card's activity, and the rules of auto mode have already decided the answer.
#[allow(clippy::too_many_arguments)]
pub(super) async fn decide(
    env: &Env<'_>,
    ctx: &Ctx,
    tx: &mut dyn Tx,
    card_id: Uuid,
    item: Option<Uuid>,
    choice: &str,
    text: &str,
    versions: &BTreeMap<String, i32>,
    auto: Option<&str>,
) -> Result<(Card, InboxItem), OpError> {
    // The card first, as every other input takes it, then the item it holds.
    let held = tx
        .cards()
        .get_for_update(card_id)
        .await?
        .ok_or_else(|| OpError::NotFound(format!("card {card_id} does not exist")))?;
    let mut found = match item {
        Some(id) => tx
            .inbox()
            .get(id)
            .await?
            .filter(|found| found.card_id == card_id)
            .ok_or_else(|| OpError::NotFound(format!("item {id} does not exist")))?,
        None => tx
            .inbox()
            .list(Some(card_id), false)
            .await?
            .into_iter()
            .find(|open| open.kind == InboxKind::Approval)
            .ok_or_else(|| {
                OpError::FailedPrecondition(format!(
                    "card {card_id} is not waiting for an approval"
                ))
            })?,
    };
    if !found.is_open() {
        return Err(OpError::FailedPrecondition(format!(
            "item {} is already answered",
            found.id
        )));
    }
    check_choice(&found, choice, text)?;
    let run = tx
        .runs()
        .get(found.run_id)
        .await?
        .ok_or_else(|| OpError::NotFound(format!("run {} does not exist", found.run_id)))?;
    // The directory is asked before the first append of the transaction, which `resolve` makes.
    let memo = Memo::new(env.directory);
    let env = &Env {
        directory: &memo,
        ..*env
    };
    if !matches!(choice, ANSWER_STOP | ANSWER_DISMISS) {
        let package = env.catalog.get(&held.workflow);
        if let (Some(package), Some(channel)) = (package, update_channel(tx).await?) {
            ask_directory(env, ctx, package, channel).await?;
            // An answer that finishes the workflow hands the card on, after `resolve` has
            // appended, so the next workflow's agents are asked for now too.
            ask_next(env, ctx, tx, package, channel).await?;
        }
    }
    let summary = summary_of(&found, choice);
    let by = Answered { auto, text };
    let card = match (found.kind, choice) {
        // An ended run has nothing to step: a retry starts a new one, and a dismiss only closes.
        (InboxKind::Failure, ANSWER_RETRY) => {
            resolve(tx, ctx, &mut found, choice, &by, &summary, &run).await?;
            let parameters = run.parameters.clone();
            // Auto mode retries for the person the run was for, not for the engine's account.
            let restarted = carrying_requester(ctx, &run);
            start_in(
                env,
                &restarted,
                tx,
                card_id,
                run.current_stage.as_deref(),
                parameters,
            )
            .await?;
            reload(tx, card_id).await?
        }
        (InboxKind::Failure, _) => {
            resolve(tx, ctx, &mut found, choice, &by, &summary, &run).await?;
            held
        }
        (_, ANSWER_STOP) => {
            resolve(tx, ctx, &mut found, choice, &by, &summary, &run).await?;
            stop_in(env, ctx, tx, card_id, Some(run.id)).await?;
            reload(tx, card_id).await?
        }
        _ => {
            let (mut locked_card, run, package) = locked(env, tx, card_id).await?;
            if run.id != found.run_id {
                return Err(OpError::FailedPrecondition(
                    "the run moved on; the item is out of date".into(),
                ));
            }
            // An agent is held to the rule auto mode uses: a gate that lists an artifact with no
            // saved version waits for a person, whether or not the card is in auto mode.
            if choice == ANSWER_APPROVE && ctx.actor.kind == ActorKind::Agent && auto.is_none() {
                let (listed, saved, _) = artifacts_of(env, tx, &found).await?;
                if let AutoAnswer::Person { reason } = answer_gate(&listed, &saved) {
                    return Err(OpError::FailedPrecondition(format!(
                        "{reason}; the gate waits for a person"
                    )));
                }
            }
            let input = match choice {
                ANSWER_APPROVE => Input::Approve {
                    artifacts: approved_versions(
                        tx,
                        &locked_card,
                        package_stage(package, &run)?,
                        versions,
                    )
                    .await?,
                },
                ANSWER_REJECT => Input::Reject {
                    reason: text.trim().to_owned(),
                },
                ANSWER_REPLY => Input::Answer {
                    text: text.trim().to_owned(),
                },
                ANSWER_RETRY => Input::Answer {
                    text: String::new(),
                },
                _ => Input::Skip,
            };
            let facts = facts(tx, &locked_card, package, run.current_stage.as_deref()).await?;
            let stepped = step(&run, package, &facts, input);
            if stepped.effects.is_empty() {
                return Err(OpError::FailedPrecondition(format!(
                    "the run is {}; it no longer waits for this answer",
                    run.state.as_str()
                )));
            }
            resolve(tx, ctx, &mut found, choice, &by, &summary, &run).await?;
            apply(
                env,
                ctx,
                tx,
                &mut locked_card,
                package,
                stepped,
                Mode::Update,
            )
            .await?;
            locked_card
        }
    };
    Ok((card, found))
}

fn package_stage<'a>(
    package: &'a crate::workflow::load::Package,
    run: &Run,
) -> Result<&'a crate::domain::workflow::Stage, OpError> {
    run.current_stage
        .as_deref()
        .and_then(|id| package.stage(id))
        .map(|(stage, _)| stage)
        .ok_or_else(|| OpError::FailedPrecondition("the run is at no stage".into()))
}

async fn reload(tx: &mut dyn Tx, card_id: Uuid) -> Result<Card, OpError> {
    tx.cards()
        .get(card_id)
        .await?
        .ok_or_else(|| OpError::NotFound(format!("card {card_id} does not exist")))
}

/// Refuses a choice the item does not take, and the text a choice needs but lacks.
fn check_choice(item: &InboxItem, choice: &str, text: &str) -> Result<(), OpError> {
    if !item.kind.choices().contains(&choice) {
        return Err(OpError::InvalidArgument(format!(
            "{choice:?} is not an answer to this {} item; choose one of: {}",
            item.kind.as_str(),
            item.kind.choices().join(", ")
        )));
    }
    let needs_text = match choice {
        ANSWER_REJECT => Some("a reason is required to send a card back"),
        ANSWER_REPLY => Some("a reply needs text"),
        _ => None,
    };
    if let Some(message) = needs_text {
        if text.trim().is_empty() {
            return Err(OpError::InvalidArgument(message.into()));
        }
    }
    if text.chars().count() > MAX_TEXT_CHARS {
        return Err(OpError::InvalidArgument(format!(
            "the text is over {MAX_TEXT_CHARS} characters"
        )));
    }
    Ok(())
}

/// The artifact versions an approval stage records: each listed artifact at the version the
/// person named, or its latest. A listed artifact with no saved version cannot be approved.
async fn approved_versions(
    tx: &mut dyn Tx,
    card: &Card,
    stage: &crate::domain::workflow::Stage,
    named: &BTreeMap<String, i32>,
) -> Result<Vec<Approved>, OpError> {
    let listed = stage
        .approval
        .as_ref()
        .map(|a| a.artifacts.clone())
        .unwrap_or_default();
    if let Some(extra) = named.keys().find(|name| !listed.contains(name)) {
        return Err(OpError::InvalidArgument(format!(
            "{extra} is not an artifact this stage asks to approve; it asks for {}",
            listed.join(", ")
        )));
    }
    let mut approved = Vec::new();
    for name in listed {
        let version = tx
            .artifacts()
            .version(card.id, &name, named.get(&name).copied())
            .await?;
        let Some(version) = version else {
            return Err(OpError::FailedPrecondition(match named.get(&name) {
                Some(n) => format!("{name} has no version {n}"),
                None => {
                    format!("{name} has no saved version to approve; send the card back instead")
                }
            }));
        };
        approved.push(Approved {
            name,
            version: u32::try_from(version.version).unwrap_or(0),
        });
    }
    Ok(approved)
}

/// A line for the card's activity: what was answered, said as the person did it.
fn summary_of(item: &InboxItem, choice: &str) -> String {
    let stage = &item.stage;
    let said = match choice {
        ANSWER_APPROVE => format!("approved {stage}"),
        ANSWER_REJECT => format!("sent {stage} back"),
        ANSWER_REPLY => format!("answered {stage}"),
        ANSWER_SKIP => format!("skipped {stage}"),
        ANSWER_STOP => "stopped the run".to_owned(),
        ANSWER_RETRY if item.kind == InboxKind::Failure => "started the run again".to_owned(),
        ANSWER_RETRY => format!("asked {stage} to carry on"),
        ANSWER_DISMISS => "dismissed the failure".to_owned(),
        other => other.to_owned(),
    };
    said.chars().take(MAX_SUMMARY_CHARS).collect()
}

/// What an answer says for itself: a person's text, or the reason of the engine's rule.
struct Answered<'a> {
    auto: Option<&'a str>,
    text: &'a str,
}

/// Closes the item with the answer, and writes what a person reads back later: the item's
/// event and an entry in the card's activity with the reason.
async fn resolve(
    tx: &mut dyn Tx,
    ctx: &Ctx,
    item: &mut InboxItem,
    choice: &str,
    by: &Answered<'_>,
    summary: &str,
    run: &Run,
) -> Result<(), OpError> {
    if !tx
        .inbox()
        .resolve(item.id, choice, &ctx.actor.pubkey)
        .await?
    {
        return Err(OpError::FailedPrecondition(format!(
            "item {} is already answered",
            item.id
        )));
    }
    item.resolved_at = Some(chrono::Utc::now());
    item.resolved_by = Some(ctx.actor.pubkey.clone());
    item.answer = Some(choice.to_owned());
    append(
        tx,
        ctx,
        "inbox.resolved",
        run,
        resolved_payload(item, choice, &ctx.actor.pubkey),
    )
    .await?;
    append(
        tx,
        ctx,
        "activity.added",
        run,
        json!({
            "type": if by.auto.is_some() { ACTIVITY_AUTO } else { "answer" },
            "summary": match by.auto {
                Some(_) => format!("auto mode: {summary}"),
                None => summary.to_owned(),
            },
            "detail": by.auto.unwrap_or(by.text).trim(),
            "stage": item.stage,
            "visit": item.visit,
            "kind": item.kind.as_str(),
            "choice": choice,
            "item": item.id,
            "title": item.title,
        }),
    )
    .await
}

/// The stage's agent asks a person a question and waits for the reply. `for_stage` and `visit`
/// are checked as [`super::run::stage_done`] checks them.
pub async fn ask_gate(
    env: &Env<'_>,
    ctx: &Ctx,
    card_id: Uuid,
    for_stage: &str,
    visit: u32,
    question: &str,
) -> Result<(Run, Vec<Event>), OpError> {
    require_position(for_stage, visit)?;
    let question = question.trim();
    if question.is_empty() {
        return Err(OpError::InvalidArgument("question is required".into()));
    }
    if question.chars().count() > MAX_TEXT_CHARS {
        return Err(OpError::InvalidArgument(format!(
            "the question is over {MAX_TEXT_CHARS} characters"
        )));
    }
    let mut tx = env.store.begin(ctx.community).await?;
    let (mut card, run, package) = locked(env, &mut *tx, card_id).await?;
    let stage_id = running_stage(&run)?;
    check_agent(&mut *tx, ctx, &run, &stage_id).await?;
    check_position(&run, &stage_id, for_stage, visit)?;
    let facts = facts(&mut *tx, &card, package, Some(&stage_id)).await?;
    let stepped = step(
        &run,
        package,
        &facts,
        Input::Ask {
            stage: stage_id,
            question: question.to_owned(),
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

/// A person writes to the agent of the stage the card is at, in the stage's thread. The run
/// is not changed. A person only. Returns the message's id, which its outbox key ends with.
pub async fn send_message(
    env: &Env<'_>,
    ctx: &Ctx,
    card_id: Uuid,
    text: &str,
) -> Result<(String, Vec<Event>), OpError> {
    people_only(ctx, "write to a stage's agent")?;
    let text = text.trim();
    if text.is_empty() {
        return Err(OpError::InvalidArgument("text is required".into()));
    }
    if text.len() > MAX_MESSAGE_BYTES {
        return Err(OpError::InvalidArgument(format!(
            "the message is over {MAX_MESSAGE_BYTES} bytes"
        )));
    }
    let mut tx = env.store.begin(ctx.community).await?;
    let (card, run, _) = locked(env, &mut *tx, card_id).await?;
    let (stage, visit) = match (run.state, run.current_stage.clone(), run.current_step()) {
        (RunState::Running | RunState::Waiting | RunState::Paused, Some(stage), Some(step)) => {
            (stage, step.visit)
        }
        _ => {
            return Err(OpError::FailedPrecondition(
                "the card has no stage with an agent to write to".into(),
            ))
        }
    };
    let queued = tx
        .outbox()
        .by_key(&task_key(card.id, &stage, visit))
        .await?
        .map(|row| row.posting);
    let Some(Posting::StageTask {
        agent, agent_name, ..
    }) = queued
    else {
        return Err(OpError::FailedPrecondition(format!(
            "stage {stage} has no thread to write in"
        )));
    };
    let channel = super::settings::update_channel(&mut *tx)
        .await?
        .ok_or_else(|| {
            OpError::FailedPrecondition(
                "no update channel is set; choose one in Settings (update_channel), then try again"
                    .into(),
            )
        })?;
    let message_id = Uuid::new_v4().simple().to_string();
    let key = format!("{}:message:{message_id}", task_key(card.id, &stage, visit));
    let posting = Posting::StageReply {
        channel,
        stage: stage.clone(),
        visit,
        agent_name: agent_name.clone(),
        agent,
        text: format!("A person wrote: {text}"),
    };
    tx.outbox().enqueue(&key, card.id, &posting).await?;
    append(
        &mut *tx,
        ctx,
        "activity.added",
        &run,
        json!({
            "type": "message",
            "summary": format!("wrote to {agent_name}"),
            "detail": text,
            "stage": stage,
            "visit": visit,
        }),
    )
    .await?;
    Ok((message_id, tx.commit().await?))
}
