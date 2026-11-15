//! Create, change, archive, restore and remove cards, and add a note to one.

use std::collections::BTreeSet;

use serde_json::json;
use uuid::Uuid;

use super::{Ctx, Env, OpError};
use crate::domain::card::{Card, CardState};
use crate::domain::events::Event;
use crate::ports::store::{Store, StoreError, Tx};

pub const MAX_TITLE_CHARS: usize = 200;
pub const MAX_DESCRIPTION_CHARS: usize = 65_536;
pub const MAX_NOTE_SUMMARY_CHARS: usize = 200;
pub const MAX_NOTE_DETAIL_CHARS: usize = 10_000;

#[derive(Debug, Clone, Default)]
pub struct NewCard {
    pub title: String,
    pub description: String,
    /// Empty puts the card in Backlog.
    pub workflow: String,
}

/// The changes `update_card` can make. A field left `None` stays as it is.
#[derive(Debug, Clone, Default)]
pub struct CardPatch {
    pub workflow: Option<String>,
    pub state: Option<CardState>,
}

/// The fields a client needs to draw a card, carried by the events that add one to a view.
fn summary(card: &Card) -> serde_json::Value {
    json!({ "title": card.title, "workflow": card.workflow, "state": card.state.as_str() })
}

async fn append(
    tx: &mut dyn Tx,
    ctx: &Ctx,
    kind: &str,
    card: Uuid,
    payload: serde_json::Value,
) -> Result<(), OpError> {
    let event = Event::new(kind, Some(card), None, ctx.actor.clone(), payload);
    tx.events().append(event).await?;
    Ok(())
}

/// A card that must exist. `NotFound` otherwise.
async fn lock(tx: &mut dyn Tx, id: Uuid) -> Result<Card, OpError> {
    tx.cards()
        .get_for_update(id)
        .await?
        .ok_or_else(|| OpError::NotFound(format!("card {id} does not exist")))
}

fn is_busy(card: &Card) -> bool {
    matches!(card.state, CardState::Running | CardState::Waiting)
}

pub async fn create_card(
    store: &dyn Store,
    ctx: &Ctx,
    input: NewCard,
) -> Result<(Card, Vec<Event>), OpError> {
    let title = input.title.trim();
    if title.is_empty() || title.chars().count() > MAX_TITLE_CHARS {
        return Err(OpError::InvalidArgument(format!(
            "title must be 1 to {MAX_TITLE_CHARS} characters"
        )));
    }
    if input.description.chars().count() > MAX_DESCRIPTION_CHARS {
        return Err(OpError::InvalidArgument(format!(
            "description must be at most {MAX_DESCRIPTION_CHARS} characters"
        )));
    }
    let card = Card {
        id: Uuid::new_v4(),
        title: title.to_owned(),
        description: input.description,
        state: CardState::Idle,
        workflow: input.workflow.trim().to_owned(),
        archived: false,
        created_by: ctx.actor.pubkey.clone(),
    };
    let mut tx = store.begin(ctx.community).await?;
    tx.cards().insert(&card).await?;
    append(&mut *tx, ctx, "card.created", card.id, summary(&card)).await?;
    let events = tx.commit().await?;
    Ok((card, events))
}

/// Moves a card to another workflow (`card.moved`) or changes its state (`card.state`).
/// A patch that changes nothing appends nothing. An archived card is refused.
pub async fn update_card(
    store: &dyn Store,
    ctx: &Ctx,
    id: Uuid,
    patch: CardPatch,
) -> Result<(Card, Vec<Event>), OpError> {
    let mut tx = store.begin(ctx.community).await?;
    let mut card = lock(&mut *tx, id).await?;
    if card.archived {
        return Err(OpError::FailedPrecondition(
            "restore the card before changing it".into(),
        ));
    }
    let before = card.clone();
    if let Some(workflow) = patch.workflow {
        card.workflow = workflow.trim().to_owned();
    }
    if let Some(state) = patch.state {
        card.state = state;
    }
    if card != before {
        tx.cards().update(&card).await?;
    }
    if card.workflow != before.workflow {
        let payload = json!({ "workflow": card.workflow, "from": before.workflow });
        append(&mut *tx, ctx, "card.moved", id, payload).await?;
    }
    if card.state != before.state {
        let payload = json!({ "state": card.state.as_str(), "from": before.state.as_str() });
        append(&mut *tx, ctx, "card.state", id, payload).await?;
    }
    let events = tx.commit().await?;
    Ok((card, events))
}

/// One card's new workflow for [`set_card_workflow`].
#[derive(Debug, Clone)]
pub struct CardMove {
    pub card: Uuid,
    /// Empty sends the card back to Backlog.
    pub workflow: String,
}

/// Sorts cards onto workflows, as a person does by dragging or from the Move menu. All or
/// nothing: one refused card refuses the batch. A card that has run, is not idle or is archived
/// is refused, and a workflow that is not loaded is `NotFound`. A card already on its workflow
/// appends nothing. The cards come back in the order asked.
pub async fn set_card_workflow(
    env: &Env<'_>,
    ctx: &Ctx,
    moves: &[CardMove],
) -> Result<(Vec<Card>, Vec<Event>), OpError> {
    if moves.is_empty() {
        return Err(OpError::InvalidArgument("moves is empty".into()));
    }
    let ids: BTreeSet<Uuid> = moves.iter().map(|m| m.card).collect();
    if ids.len() != moves.len() {
        return Err(OpError::InvalidArgument(
            "a card appears more than once in moves".into(),
        ));
    }
    let mut tx = env.store.begin(ctx.community).await?;
    // Lock every card first, in id order, before the first append takes the event-log lock.
    let mut locked = std::collections::BTreeMap::new();
    for id in &ids {
        locked.insert(*id, lock(&mut *tx, *id).await?);
    }
    let mut moved = Vec::with_capacity(moves.len());
    for m in moves {
        let workflow = m.workflow.trim();
        let mut card = locked[&m.card].clone();
        if card.archived {
            return Err(OpError::FailedPrecondition(format!(
                "card {} is archived; restore it first",
                card.id
            )));
        }
        if card.state != CardState::Idle {
            return Err(OpError::FailedPrecondition(format!(
                "card {} is {}; only an idle card can be sorted",
                card.id,
                card.state.as_str()
            )));
        }
        if !tx.runs().list(Some(card.id)).await?.is_empty() {
            return Err(OpError::FailedPrecondition(format!(
                "card {} has run; it cannot be sorted again",
                card.id
            )));
        }
        if !workflow.is_empty() && env.catalog.get(workflow).is_none() {
            return Err(OpError::NotFound(env.catalog.absent(workflow)));
        }
        let from = std::mem::replace(&mut card.workflow, workflow.to_owned());
        if card.workflow != from {
            tx.cards().update(&card).await?;
            let payload = json!({ "workflow": card.workflow, "from": from });
            append(&mut *tx, ctx, "card.moved", card.id, payload).await?;
        }
        moved.push(card);
    }
    let events = tx.commit().await?;
    Ok((moved, events))
}

/// Sets cards aside, all or nothing. A card already archived is left alone and appends
/// nothing. A card with work in flight is refused until that work is stopped.
pub async fn archive_cards(
    store: &dyn Store,
    ctx: &Ctx,
    ids: &[Uuid],
) -> Result<Vec<Event>, OpError> {
    let mut tx = store.begin(ctx.community).await?;
    // Lock every card first, in id order, before the first append takes the event-log
    // lock. Two calls over the same cards then queue instead of deadlocking.
    let ids: BTreeSet<Uuid> = ids.iter().copied().collect();
    let mut cards = Vec::with_capacity(ids.len());
    for id in &ids {
        let card = lock(&mut *tx, *id).await?;
        if !card.archived && is_busy(&card) {
            return Err(OpError::FailedPrecondition(format!(
                "card {id} is {}; stop it before archiving",
                card.state.as_str()
            )));
        }
        cards.push(card);
    }
    for card in cards.iter().filter(|c| !c.archived) {
        tx.cards().set_archived(card.id, true).await?;
        append(&mut *tx, ctx, "card.archived", card.id, summary(card)).await?;
    }
    Ok(tx.commit().await?)
}

/// Brings an archived card back to Backlog.
///
/// The Go engine returns a card that has run to its workflow. No run exists yet, so
/// every card goes to Backlog, idle and with no workflow, whatever state it was archived in;
/// Phase 6 keeps the workflow of a card that has run.
pub async fn restore_card(
    store: &dyn Store,
    ctx: &Ctx,
    id: Uuid,
) -> Result<(Card, Vec<Event>), OpError> {
    let mut tx = store.begin(ctx.community).await?;
    let mut card = lock(&mut *tx, id).await?;
    if !card.archived {
        return Err(OpError::FailedPrecondition(format!(
            "card {id} is not archived"
        )));
    }
    card.archived = false;
    card.workflow.clear();
    card.state = CardState::Idle;
    tx.cards().set_archived(id, false).await?;
    tx.cards().update(&card).await?;
    append(&mut *tx, ctx, "card.restored", id, summary(&card)).await?;
    let events = tx.commit().await?;
    Ok((card, events))
}

/// Takes a card off the board for good. A card with work in flight is refused.
pub async fn delete_card(store: &dyn Store, ctx: &Ctx, id: Uuid) -> Result<Vec<Event>, OpError> {
    let mut tx = store.begin(ctx.community).await?;
    let card = lock(&mut *tx, id).await?;
    if is_busy(&card) {
        return Err(OpError::FailedPrecondition(format!(
            "card {id} is {}; stop it before removing",
            card.state.as_str()
        )));
    }
    tx.cards().delete(id).await?;
    append(&mut *tx, ctx, "card.removed", id, json!({})).await?;
    Ok(tx.commit().await?)
}

/// Writes a person's note on a card to the log as `activity.added`.
///
/// The Go engine refuses a note on a card at no stage. A Beehive card has no stage
/// until Phase 6, so a note is allowed on any card.
pub async fn add_note(
    store: &dyn Store,
    ctx: &Ctx,
    id: Uuid,
    summary: &str,
    detail: &str,
) -> Result<Event, OpError> {
    let summary = summary.trim();
    if summary.is_empty() || summary.chars().count() > MAX_NOTE_SUMMARY_CHARS {
        return Err(OpError::InvalidArgument(format!(
            "summary must be 1 to {MAX_NOTE_SUMMARY_CHARS} characters"
        )));
    }
    if detail.chars().count() > MAX_NOTE_DETAIL_CHARS {
        return Err(OpError::InvalidArgument(format!(
            "detail must be at most {MAX_NOTE_DETAIL_CHARS} characters"
        )));
    }
    let mut tx = store.begin(ctx.community).await?;
    let card = tx
        .cards()
        .get(id)
        .await?
        .ok_or_else(|| OpError::NotFound(format!("card {id} does not exist")))?;
    append(
        &mut *tx,
        ctx,
        "activity.added",
        card.id,
        json!({ "type": "note", "summary": summary, "detail": detail }),
    )
    .await?;
    let mut events = tx.commit().await?;
    events
        .pop()
        .ok_or_else(|| OpError::Store(StoreError::Unavailable("note was not committed".into())))
}
