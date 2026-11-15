//! Needs you: list and answer the items, decide an approval gate, write to a stage's agent, and
//! the agent's own way to ask a person.

use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap};

use buzz_hive_core::domain::inbox::InboxItem as DomainItem;
use buzz_hive_core::orchestrator::inbox as ops;
use uuid::Uuid;

use crate::app::App;
use crate::error::HiveError;
use crate::proto::{
    AnswerInboxRequest, ApproveRequest, AskGateRequest, Card, InboxItem, ListInboxRequest,
    ListInboxResponse, Run, SendBackRequest, SendMessageRequest, SendMessageResponse,
};
use crate::routes::cards::reply as card_reply;
use crate::routes::poll::timestamp;
use crate::routes::runs::{parse_id, to_proto as run_to_proto};
use crate::unary::Ctx;

/// Open items first, then the resolved ones if asked for, with the newest event seq read
/// before the items, as `ListCards` reads it.
pub async fn list_inbox(
    app: App,
    ctx: Ctx,
    req: ListInboxRequest,
) -> Result<ListInboxResponse, HiveError> {
    let card = (!req.card_id.is_empty())
        .then(|| parse_id(&req.card_id, "card_id"))
        .transpose()?;
    let mut tx = app.store.begin(ctx.community).await?;
    let seq = tx.events().latest_seq().await?;
    let found = tx.inbox().list(card, req.include_resolved).await?;
    let mut titles: HashMap<Uuid, String> = HashMap::new();
    for item in &found {
        if let Entry::Vacant(slot) = titles.entry(item.card_id) {
            let title = tx
                .cards()
                .get(item.card_id)
                .await?
                .map(|card| card.title)
                .unwrap_or_default();
            slot.insert(title);
        }
    }
    Ok(ListInboxResponse {
        items: found
            .into_iter()
            .map(|item| {
                let title = titles.get(&item.card_id).cloned().unwrap_or_default();
                to_proto(item, title)
            })
            .collect(),
        seq,
    })
}

/// A person answers an open item with one of its choices.
pub async fn answer_inbox(
    app: App,
    ctx: Ctx,
    req: AnswerInboxRequest,
) -> Result<InboxItem, HiveError> {
    let id = parse_id(&req.item_id, "item_id")?;
    if !req.values_json.is_empty() || !req.answers.is_empty() {
        return Err(HiveError::InvalidArgument(
            "values_json and answers are not used; give a choice and, when it needs one, text"
                .into(),
        ));
    }
    let op = app.op(true)?;
    let (item, events) = ops::answer(&op.env(), &ctx.op(), id, &req.choice, &req.text).await?;
    op.settle(&events);
    let title = card_title(&app, &ctx, item.card_id).await?;
    Ok(to_proto(item, title))
}

/// Approves the card's open approval stage. A version left out is approved at its latest.
pub async fn approve(app: App, ctx: Ctx, req: ApproveRequest) -> Result<Card, HiveError> {
    let id = parse_id(&req.card_id, "card_id")?;
    let mut versions = BTreeMap::new();
    for artifact in &req.artifacts {
        if artifact.name.is_empty() {
            return Err(HiveError::InvalidArgument(
                "an artifact needs a name".into(),
            ));
        }
        // Zero means "not named": the latest.
        if artifact.version > 0 {
            versions.insert(artifact.name.clone(), artifact.version);
        }
    }
    let op = app.op(true)?;
    let (card, events) = ops::approve(&op.env(), &ctx.op(), id, &versions).await?;
    op.settle(&events);
    card_reply(&app, &ctx, card).await
}

/// A person sends the card's open approval stage back with comments.
pub async fn send_back(app: App, ctx: Ctx, req: SendBackRequest) -> Result<Card, HiveError> {
    let id = parse_id(&req.card_id, "card_id")?;
    let op = app.op(true)?;
    let (card, events) = ops::send_back(&op.env(), &ctx.op(), id, &req.comments).await?;
    op.settle(&events);
    card_reply(&app, &ctx, card).await
}

/// A person writes to the agent of the card's stage, in the stage's thread.
pub async fn send_message(
    app: App,
    ctx: Ctx,
    req: SendMessageRequest,
) -> Result<SendMessageResponse, HiveError> {
    let id = parse_id(&req.card_id, "card_id")?;
    let op = app.op(true)?;
    let (message_id, events) = ops::send_message(&op.env(), &ctx.op(), id, &req.text).await?;
    op.settle(&events);
    Ok(SendMessageResponse { message_id })
}

/// The agent of a running stage asks a person a question and waits for the reply.
pub async fn ask_gate(app: App, ctx: Ctx, req: AskGateRequest) -> Result<Run, HiveError> {
    let card = parse_id(&req.card_id, "card_id")?;
    if req.stage.is_empty() {
        return Err(HiveError::InvalidArgument("stage is required".into()));
    }
    let visit = u32::try_from(req.visit)
        .ok()
        .filter(|v| *v > 0)
        .ok_or_else(|| HiveError::InvalidArgument("visit is required, from 1".into()))?;
    let op = app.op(true)?;
    let (run, events) =
        ops::ask_gate(&op.env(), &ctx.op(), card, &req.stage, visit, &req.question).await?;
    op.settle(&events);
    Ok(run_to_proto(run))
}

async fn card_title(app: &App, ctx: &Ctx, card: Uuid) -> Result<String, HiveError> {
    let mut tx = app.store.begin(ctx.community).await?;
    Ok(tx
        .cards()
        .get(card)
        .await?
        .map(|card| card.title)
        .unwrap_or_default())
}

fn to_proto(item: DomainItem, card_title: String) -> InboxItem {
    let state = if item.is_open() { "open" } else { "resolved" };
    InboxItem {
        choices: item.choices().iter().map(|c| (*c).to_owned()).collect(),
        id: item.id.to_string(),
        kind: item.kind.as_str().to_owned(),
        run_id: item.run_id.to_string(),
        card_id: item.card_id.to_string(),
        state: state.to_owned(),
        title: item.title,
        detail_json: item.detail.to_string(),
        answer_json: item.answer.unwrap_or_default(),
        created_at: Some(timestamp(item.opened_at)),
        resolved_at: item.resolved_at.map(timestamp),
        card_title,
        stage: item.stage,
        visit: i32::try_from(item.visit).unwrap_or(i32::MAX),
        resolved_by: item.resolved_by.unwrap_or_default(),
    }
}
