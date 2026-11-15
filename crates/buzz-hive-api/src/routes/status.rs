//! `GetStatus` and `WhoAmI`.

use buzz_hive_core::domain::card::CardState;

use crate::app::App;
use crate::error::HiveError;
use crate::proto::{
    GetStatusRequest, GetStatusResponse, GetUsageRequest, GetUsageResponse, Person, UsageWeek,
    WhoAmIRequest, WhoAmIResponse,
};
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

/// What agents did, counted from the event log: runs and how long they took, and what agents
/// started in each of the last weeks. There are no token or cost figures.
pub async fn get_usage(
    app: App,
    ctx: Ctx,
    _req: GetUsageRequest,
) -> Result<GetUsageResponse, HiveError> {
    let usage =
        buzz_hive_core::orchestrator::usage::get(app.store.as_ref(), &ctx.op(), &app.hive_pubkey)
            .await?;
    let count = |n: u32| i32::try_from(n).unwrap_or(i32::MAX);
    let seconds = |n: u64| i64::try_from(n).unwrap_or(i64::MAX);
    Ok(GetUsageResponse {
        runs: count(usage.runs),
        ended_runs: count(usage.ended_runs),
        average_run_seconds: seconds(usage.average_run_seconds),
        longest_run_seconds: seconds(usage.longest_run_seconds),
        weeks: usage
            .weeks
            .into_iter()
            .map(|week| UsageWeek {
                starts: week.starts.format("%Y-%m-%d").to_string(),
                cards_created_by_agents: count(week.cards_created_by_agents),
                runs_started_by_agents: count(week.runs_started_by_agents),
            })
            .collect(),
    })
}
