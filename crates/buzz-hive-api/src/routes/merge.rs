//! Merge procedures: a person merges a card's pull request, or sets the order its repos merge in.
//!
//! An agent is refused by the orchestrator, so neither has a tool. A workflow merges for itself
//! with a `merge` stage, which is the workflow author's choice.

use buzz_hive_core::orchestrator::merge::{merge_card as merge, set_merge_order as order};

use crate::app::App;
use crate::error::HiveError;
use crate::proto::{Card, GetCardRequest, MergeCardRequest, SetMergeOrderRequest};
use crate::routes::cards::get_card;
use crate::routes::runs::parse_id;
use crate::unary::Ctx;

/// Merges the card's pull request, reading the host first.
pub async fn merge_card(app: App, ctx: Ctx, req: MergeCardRequest) -> Result<Card, HiveError> {
    let id = parse_id(&req.card_id, "card_id")?;
    let host = app.require_host()?;
    let op = app.op(true)?;
    let events = merge(&op.env(), &ctx.op(), host, id, None).await?;
    op.settle(&events);
    get_card(
        app,
        ctx,
        GetCardRequest {
            card_id: req.card_id,
        },
    )
    .await
}

/// Checks the order of the card's repos. A card has one repo, so nothing is stored.
pub async fn set_merge_order(
    app: App,
    ctx: Ctx,
    req: SetMergeOrderRequest,
) -> Result<Card, HiveError> {
    let id = parse_id(&req.card_id, "card_id")?;
    order(&app.op(false)?.env(), &ctx.op(), id, &req.repos).await?;
    get_card(
        app,
        ctx,
        GetCardRequest {
            card_id: req.card_id,
        },
    )
    .await
}
