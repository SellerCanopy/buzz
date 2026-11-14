//! Starting a run. Every later input to a run goes through the same row lock.

use serde_json::json;
use uuid::Uuid;

use super::settings;
use super::task_message::{render_task, CardText};
use super::{Ctx, Env, OpError};
use crate::config::{self, Layer, Layers};
use crate::domain::card::CardState;
use crate::domain::events::Event;
use crate::domain::run::Run;
use crate::ports::store::{Posting, Tx};
use crate::rules::run::{entry_stage, step, Effect, Input};

/// Starts a run of the card's workflow at its first stage.
///
/// One transaction holds the card, makes the run, moves the card to `running`, appends the
/// events and queues the two posts, so a crash between them loses neither. The posts reach
/// the relay when the outbox is drained. Every refusal is a sentence the person can act on.
pub async fn start_run(
    env: &Env<'_>,
    ctx: &Ctx,
    card_id: Uuid,
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
    let (stage, agent_name) = entry_stage(package).map_err(OpError::FailedPrecondition)?;
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
    let agent = find_agent(env, ctx, agent_name, &package.meta.name, &stage.id).await?;
    if !env
        .directory
        .member_of_channel(ctx.community, channel, &agent)
        .await?
    {
        return Err(OpError::FailedPrecondition(format!(
            "add {agent_name} to the update channel, then start the run again"
        )));
    }

    let params = resolved_params(env, &mut *tx, &card, &stage.id).await?;
    let task = render_task(
        package,
        stage,
        &CardText {
            title: &card.title,
            description: &card.description,
        },
        &params,
    )
    .map_err(OpError::FailedPrecondition)?;

    let mut run = Run::new(card_id, &package.meta.name, &package.hash());
    run.visit = tx.runs().visits(card_id, &stage.id).await?;
    let stepped = step(
        &run,
        package,
        Input::Start {
            requester: Some(ctx.actor.pubkey.clone()),
        },
    );
    let run = stepped.run;
    tx.runs().insert(&run).await?;
    let before = card.state;
    card.state = CardState::Running;
    tx.cards().update(&card).await?;
    if before != card.state {
        let payload = json!({ "state": card.state.as_str(), "from": before.as_str() });
        append(&mut *tx, ctx, "card.state", &run, payload).await?;
    }
    for effect in stepped.effects {
        match effect {
            Effect::Emit { kind, payload } => append(&mut *tx, ctx, kind, &run, payload).await?,
            Effect::PostStatus {
                card: id,
                key,
                text,
            } => {
                let posting = Posting::Status {
                    channel,
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
            } => {
                let posting = Posting::StageTask {
                    channel,
                    stage,
                    visit,
                    agent_name,
                    agent: agent.clone(),
                    text: task.clone(),
                };
                tx.outbox().enqueue(&key, id, &posting).await?;
            }
        }
    }
    let events = tx.commit().await?;
    Ok((run, events))
}

async fn append(
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
async fn find_agent(
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

/// The parameter values the stage sees: package defaults, then the workflow's and the
/// card's own, by name.
async fn resolved_params(
    env: &Env<'_>,
    tx: &mut dyn Tx,
    card: &crate::domain::card::Card,
    stage: &str,
) -> Result<std::collections::BTreeMap<String, String>, OpError> {
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
        params: package
            .map(|p| p.meta.parameters.clone())
            .unwrap_or_default(),
        ..Layers::default()
    };
    let stage = package.and_then(|p| p.stage(stage)).map(|(s, _)| s);
    Ok(config::resolve(&card.workflow, stage, &layers, false).param_values())
}
