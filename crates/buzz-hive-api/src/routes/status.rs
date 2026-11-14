//! `GetStatus` and `WhoAmI`.

use buzz_hive_core::access::ActorKind;

use crate::app::App;
use crate::error::HiveError;
use crate::proto::{GetStatusRequest, GetStatusResponse, Person, WhoAmIRequest, WhoAmIResponse};
use crate::unary::Ctx;

/// The engine API version the page compares with its own.
const API_VERSION: i32 = 1;

pub async fn get_status(
    _app: App,
    ctx: Ctx,
    _req: GetStatusRequest,
) -> Result<GetStatusResponse, HiveError> {
    Ok(GetStatusResponse {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        community_id: ctx.community.to_string(),
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
        kind: match actor.kind {
            ActorKind::Person => "person",
            ActorKind::Agent => "agent",
        }
        .to_owned(),
        owner: actor.owner.unwrap_or_default(),
        ..Default::default()
    })
}
