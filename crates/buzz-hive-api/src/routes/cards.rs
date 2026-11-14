//! Card procedures: list, read, create, archive, restore, remove and add a note.

use buzz_hive_core::domain::card::Card as DomainCard;
use buzz_hive_core::domain::events::Event;
use buzz_hive_core::orchestrator::cards::{self, NewCard};
use uuid::Uuid;

use crate::app::App;
use crate::error::HiveError;
use crate::proto::{
    ActivityAuthor, ActivityEntry, AddNoteRequest, AddNoteResponse, ArchiveCardsRequest,
    ArchiveCardsResponse, Card, CreateCardRequest, CreateTaskRequest, GetCardRequest,
    ListCardsRequest, ListCardsResponse, RemoveCardRequest, RemoveCardResponse, RestoreCardRequest,
};
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
    Ok(ListCardsResponse {
        cards: cards.into_iter().map(|c| to_proto(c, false)).collect(),
        seq,
    })
}

pub async fn get_card(app: App, ctx: Ctx, req: GetCardRequest) -> Result<Card, HiveError> {
    let id = parse_id(&req.card_id, "card_id")?;
    let mut tx = app.store.begin(ctx.community).await?;
    let card = tx
        .cards()
        .get(id)
        .await?
        .ok_or_else(|| HiveError::NotFound(format!("card {id} does not exist")))?;
    // `body` is set by GetCard only: it can be 64 KiB.
    Ok(to_proto(card, true))
}

pub async fn create_card(app: App, ctx: Ctx, req: CreateCardRequest) -> Result<Card, HiveError> {
    if req.issue.is_some() {
        return Err(HiveError::InvalidArgument(
            "a card has no issue until issue intake exists; leave issue empty".into(),
        ));
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
    Ok(to_proto(card, false))
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

fn to_proto(card: DomainCard, with_body: bool) -> Card {
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

fn to_activity(event: Event) -> ActivityEntry {
    let text = |key: &str| event.payload[key].as_str().unwrap_or_default().to_owned();
    ActivityEntry {
        seq: event.seq,
        key: format!("note:{}", event.seq),
        card_id: event.card_id.map(|id| id.to_string()).unwrap_or_default(),
        event_seq: event.seq,
        at: Some(crate::routes::poll::timestamp(event.at)),
        r#type: text("type"),
        author: Some(ActivityAuthor {
            kind: event.actor.kind.as_str().to_owned(),
            id: event.actor.pubkey.clone(),
        }),
        summary: text("summary"),
        detail: text("detail"),
        fields_json: "{}".to_owned(),
        v: 1,
        ..Default::default()
    }
}
