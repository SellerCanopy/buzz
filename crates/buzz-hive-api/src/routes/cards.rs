//! Card procedures. Phase 3 serves `ListCards` only.

use buzz_hive_core::domain::card::Card as DomainCard;

use crate::app::App;
use crate::error::HiveError;
use crate::proto::{Card, ListCardsRequest, ListCardsResponse};
use crate::unary::Ctx;

pub async fn list_cards(
    app: App,
    ctx: Ctx,
    req: ListCardsRequest,
) -> Result<ListCardsResponse, HiveError> {
    let mut tx = app.store.begin(ctx.community).await?;
    let cards = tx.cards().list(req.archived).await?;
    Ok(ListCardsResponse {
        cards: cards.into_iter().map(to_proto).collect(),
        // The newest event seq. There is no event log until Phase 4.
        seq: 0,
    })
}

fn to_proto(card: DomainCard) -> Card {
    Card {
        // `Card.id` is an int64 in the Go proto and a uuid here. Phase 4 settles it
        // when cards become creatable; until then no card can exist.
        id: 0,
        title: card.title,
        workflow: card.workflow,
        state: card.state.as_str().to_owned(),
        archived: card.archived,
        ..Default::default()
    }
}
