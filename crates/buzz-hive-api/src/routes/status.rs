//! `GetStatus` and `WhoAmI`.

use buzz_hive_core::domain::card::CardState;

use crate::app::App;
use crate::error::HiveError;
use crate::proto::{GetStatusRequest, GetStatusResponse, Person, WhoAmIRequest, WhoAmIResponse};
use crate::unary::Ctx;

/// The engine API version the page compares with its own.
const API_VERSION: i32 = 1;

/// The seq first, as `ListCards` does, then the counts, all in one transaction.
pub async fn get_status(
    app: App,
    ctx: Ctx,
    _req: GetStatusRequest,
) -> Result<GetStatusResponse, HiveError> {
    let mut tx = app.store.begin(ctx.community).await?;
    let head_seq = tx.events().latest_seq().await?;
    let running = tx
        .cards()
        .list(false)
        .await?
        .iter()
        .filter(|card| card.state == CardState::Running)
        .count();
    let backlog = tx.outbox().backlog().await?;
    Ok(GetStatusResponse {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        community_id: ctx.community.to_string(),
        head_seq,
        running: i64::try_from(running).unwrap_or(i64::MAX),
        outbox_pending: backlog.pending,
        outbox_failed: backlog.failed,
        ..Default::default()
    })
}

pub async fn who_am_i(
    _app: App,
    ctx: Ctx,
    _req: WhoAmIRequest,
) -> Result<WhoAmIResponse, HiveError> {
    let actor = ctx.actor;
    Ok(WhoAmIResponse {
        me: Some(Person {
            user_id: actor.pubkey,
            ..Default::default()
        }),
        company_id: ctx.community.to_string(),
        role: actor.level.as_str().to_owned(),
        api_version: API_VERSION,
        kind: actor.kind.as_str().to_owned(),
        owner: actor.owner.unwrap_or_default(),
        ..Default::default()
    })
}
