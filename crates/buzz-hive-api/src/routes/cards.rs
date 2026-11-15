//! Card procedures: list, read, create, archive, restore, remove and add a note.

use buzz_hive_core::domain::card::{Card as DomainCard, CardState};
use buzz_hive_core::domain::events::Event;
use buzz_hive_core::domain::intake::RepoRef;
use buzz_hive_core::orchestrator::auto;
use buzz_hive_core::orchestrator::cards::{self, CardMove, NewCard};
use buzz_hive_core::orchestrator::intake::card_from_issue;
use uuid::Uuid;

use crate::app::App;
use crate::error::HiveError;
use crate::proto::{
    ActivityAuthor, ActivityEntry, AddNoteRequest, AddNoteResponse, ArchiveCardsRequest,
    ArchiveCardsResponse, Card, CreateCardRequest, CreateTaskRequest, GetCardRequest,
    ListCardsRequest, ListCardsResponse, RemoveCardRequest, RemoveCardResponse, RestoreCardRequest,
    SetCardWorkflowRequest, SetCardWorkflowResponse, StageThread,
};
use crate::routes::intake::{fill_issues, fill_one};
use crate::unary::Ctx;

pub async fn list_cards(
    app: App,
    ctx: Ctx,
    req: ListCardsRequest,
) -> Result<ListCardsResponse, HiveError> {
    let mut tx = app.store.begin(ctx.community).await?;
    // Read the seq before the cards. A change that commits between the two reads is
    // then in the cards and also in the events after `seq`, which the page applies
    // as a no-op. The other order would lose it.
    let seq = tx.events().latest_seq().await?;
    let cards = tx.cards().list(req.archived).await?;
    drop(tx);
    let mut cards: Vec<Card> = cards.into_iter().map(|c| to_proto(c, false)).collect();
    fill_issues(&app, &ctx, &mut cards).await?;
    Ok(ListCardsResponse { cards, seq })
}

pub async fn get_card(app: App, ctx: Ctx, req: GetCardRequest) -> Result<Card, HiveError> {
    let id = parse_id(&req.card_id, "card_id")?;
    let mut tx = app.store.begin(ctx.community).await?;
    let card = tx
        .cards()
        .get(id)
        .await?
        .ok_or_else(|| HiveError::NotFound(format!("card {id} does not exist")))?;
    // `body` is set by GetCard only: it can be 64 KiB. So are the stage threads, which the
    // Live tab reads, and the stage the card's newest run is in.
    // A card that was reset is idle and starts again at the first stage, so it has no current one.
    let idle = card.state == CardState::Idle;
    let mut proto = to_proto(card, true);
    proto.auto_mode = auto::is_on(&mut *tx, id).await?;
    proto.stage_id = tx
        .runs()
        .list(Some(id))
        .await?
        .into_iter()
        .next()
        .and_then(|run| run.current_stage)
        .filter(|_| !idle)
        .unwrap_or_default();
    proto.stages = tx
        .runs()
        .threads(id)
        .await?
        .into_iter()
        .map(|t| StageThread {
            stage: t.stage,
            visit: i32::try_from(t.visit).unwrap_or(i32::MAX),
            root_event_id: t.root_event_id,
            agent_pubkey: t.agent_pubkey,
            channel_id: t.channel_id.to_string(),
        })
        .collect();
    drop(tx);
    if let Some(row) = fill_one(&app, &ctx, &mut proto).await? {
        proto.issue_labels = row.issue.labels;
    }
    Ok(proto)
}

pub async fn create_card(app: App, ctx: Ctx, req: CreateCardRequest) -> Result<Card, HiveError> {
    if let Some(issue) = req.issue {
        return create_from_issue(&app, &ctx, issue, &req.workflow).await;
    }
    create(
        &app,
        &ctx,
        NewCard {
            title: req.title,
            description: req.description,
            workflow: req.workflow,
        },
    )
    .await
}

/// Puts an issue on the board, or returns the card it already has.
async fn create_from_issue(
    app: &App,
    ctx: &Ctx,
    issue: crate::proto::IssueRef,
    workflow: &str,
) -> Result<Card, HiveError> {
    if !issue.host.is_empty() && !issue.host.eq_ignore_ascii_case(crate::routes::intake::HOST) {
        return Err(HiveError::InvalidArgument(format!(
            "host {:?} is not supported; the only host is {}",
            issue.host,
            crate::routes::intake::HOST
        )));
    }
    let repo = RepoRef::parse(&format!("{}/{}", issue.owner, issue.repo))
        .map_err(HiveError::InvalidArgument)?;
    let number = u64::try_from(issue.number)
        .ok()
        .filter(|n| *n > 0)
        .ok_or_else(|| HiveError::InvalidArgument("issue number must be at least 1".into()))?;
    let host = app.require_host()?;
    let op = app.op(false)?;
    let (card, _, events) = card_from_issue(
        &op.env(),
        &ctx.op(),
        host,
        &repo,
        number,
        workflow,
        chrono::Utc::now(),
    )
    .await?;
    op.settle(&events);
    let mut proto = to_proto(card, false);
    fill_one(app, ctx, &mut proto).await?;
    Ok(proto)
}

/// `CreateTask` with the Go task shape. A task is a card with a title and no issue.
pub async fn create_task(app: App, ctx: Ctx, req: CreateTaskRequest) -> Result<Card, HiveError> {
    if !req.repo.is_empty() || !req.parameters.is_empty() {
        return Err(HiveError::InvalidArgument(
            "repo and parameters are not supported yet".into(),
        ));
    }
    create(
        &app,
        &ctx,
        NewCard {
            title: req.sentence,
            description: req.description,
            workflow: req.workflow,
        },
    )
    .await
}

async fn create(app: &App, ctx: &Ctx, input: NewCard) -> Result<Card, HiveError> {
    let (card, events) = cards::create_card(app.store.as_ref(), &ctx.op(), input).await?;
    app.bus.publish(&events);
    Ok(to_proto(card, false))
}

pub async fn archive_cards(
    app: App,
    ctx: Ctx,
    req: ArchiveCardsRequest,
) -> Result<ArchiveCardsResponse, HiveError> {
    if req.card_ids.is_empty() {
        return Err(HiveError::InvalidArgument("card_ids is required".into()));
    }
    let ids = req
        .card_ids
        .iter()
        .map(|id| parse_id(id, "card_ids"))
        .collect::<Result<Vec<_>, _>>()?;
    let events = cards::archive_cards(app.store.as_ref(), &ctx.op(), &ids).await?;
    app.bus.publish(&events);
    Ok(ArchiveCardsResponse {})
}

pub async fn restore_card(app: App, ctx: Ctx, req: RestoreCardRequest) -> Result<Card, HiveError> {
    let id = parse_id(&req.card_id, "card_id")?;
    let (card, events) = cards::restore_card(app.store.as_ref(), &ctx.op(), id).await?;
    app.bus.publish(&events);
    reply(&app, &ctx, card).await
}

pub async fn remove_card(
    app: App,
    ctx: Ctx,
    req: RemoveCardRequest,
) -> Result<RemoveCardResponse, HiveError> {
    let id = parse_id(&req.card_id, "card_id")?;
    let events = cards::delete_card(app.store.as_ref(), &ctx.op(), id).await?;
    app.bus.publish(&events);
    Ok(RemoveCardResponse {})
}

/// Sorts cards onto workflows, all or nothing.
pub async fn set_card_workflow(
    app: App,
    ctx: Ctx,
    req: SetCardWorkflowRequest,
) -> Result<SetCardWorkflowResponse, HiveError> {
    let moves = req
        .moves
        .iter()
        .map(|m| {
            Ok(CardMove {
                card: parse_id(&m.card_id, "card_id")?,
                workflow: m.workflow.clone(),
            })
        })
        .collect::<Result<Vec<_>, HiveError>>()?;
    // Sorting cards queues no post, so it needs no relay account.
    let op = app.op(false)?;
    let (cards, events) = cards::set_card_workflow(&op.env(), &ctx.op(), &moves).await?;
    op.settle(&events);
    let mut cards: Vec<Card> = cards.into_iter().map(|c| to_proto(c, false)).collect();
    fill_issues(&app, &ctx, &mut cards).await?;
    Ok(SetCardWorkflowResponse { cards })
}

pub async fn add_note(
    app: App,
    ctx: Ctx,
    req: AddNoteRequest,
) -> Result<AddNoteResponse, HiveError> {
    let id = parse_id(&req.card_id, "card_id")?;
    let event =
        cards::add_note(app.store.as_ref(), &ctx.op(), id, &req.summary, &req.detail).await?;
    app.bus.publish(std::slice::from_ref(&event));
    Ok(AddNoteResponse {
        entry: Some(to_activity(event)),
    })
}

fn parse_id(value: &str, field: &str) -> Result<Uuid, HiveError> {
    if value.is_empty() {
        return Err(HiveError::InvalidArgument(format!("{field} is required")));
    }
    Uuid::parse_str(value)
        .map_err(|_| HiveError::InvalidArgument(format!("{field} is not a card id")))
}

pub(super) fn to_proto(card: DomainCard, with_body: bool) -> Card {
    Card {
        id: card.id.to_string(),
        title: card.title,
        workflow: card.workflow,
        state: card.state.as_str().to_owned(),
        archived: card.archived,
        body: if with_body {
            card.description
        } else {
            String::new()
        },
        ..Default::default()
    }
}

/// A card for a reply: its issue is filled in, so a client that replaces the card it holds
/// with the reply does not lose it.
pub(super) async fn reply(app: &App, ctx: &Ctx, card: DomainCard) -> Result<Card, HiveError> {
    let mut proto = to_proto(card, false);
    fill_one(app, ctx, &mut proto).await?;
    Ok(proto)
}

/// The card as it is now, for a procedure that returns the card it changed.
pub(super) async fn card_of(app: &App, ctx: &Ctx, id: Uuid) -> Result<DomainCard, HiveError> {
    let mut tx = app.store.begin(ctx.community).await?;
    tx.cards()
        .get(id)
        .await?
        .ok_or_else(|| HiveError::NotFound(format!("card {id} does not exist")))
}

pub(super) fn to_activity(event: Event) -> ActivityEntry {
    let text = |key: &str| event.payload[key].as_str().unwrap_or_default().to_owned();
    // What the entry holds beyond the fields every entry has.
    let fields: serde_json::Map<String, serde_json::Value> = event
        .payload
        .as_object()
        .map(|p| {
            p.iter()
                .filter(|(key, _)| {
                    !matches!(
                        key.as_str(),
                        "type" | "summary" | "detail" | "stage" | "visit"
                    )
                })
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect()
        })
        .unwrap_or_default();
    ActivityEntry {
        seq: event.seq,
        key: format!("{}:{}", text("type"), event.seq),
        card_id: event.card_id.map(|id| id.to_string()).unwrap_or_default(),
        stage_id: text("stage"),
        visit: event.payload["visit"]
            .as_i64()
            .and_then(|v| i32::try_from(v).ok())
            .unwrap_or_default(),
        run_id: event.run_id.map(|id| id.to_string()).unwrap_or_default(),
        event_seq: event.seq,
        at: Some(crate::routes::poll::timestamp(event.at)),
        r#type: text("type"),
        author: Some(ActivityAuthor {
            kind: event.actor.kind.as_str().to_owned(),
            id: event.actor.pubkey.clone(),
        }),
        summary: text("summary"),
        detail: text("detail"),
        fields_json: serde_json::Value::Object(fields).to_string(),
        v: 1,
        ..Default::default()
    }
}
