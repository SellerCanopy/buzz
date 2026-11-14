//! Run procedures: start a card's workflow, read one run and list runs.

use buzz_hive_core::domain::run::Run as DomainRun;
use buzz_hive_core::orchestrator::run::start_run;
use buzz_hive_core::orchestrator::Env;
use uuid::Uuid;

use crate::app::App;
use crate::error::HiveError;
use crate::proto::{GetRunRequest, ListRunsRequest, ListRunsResponse, Run, StartStageRequest};
use crate::routes::poll::timestamp;
use crate::unary::Ctx;

/// Starts the card's workflow at its first stage. The posts reach the update channel when
/// the outbox drains, which this call wakes.
pub async fn start_stage(app: App, ctx: Ctx, req: StartStageRequest) -> Result<Run, HiveError> {
    if !req.stage_id.is_empty() {
        return Err(HiveError::InvalidArgument(
            "starting at a named stage is not supported yet; leave stage_id empty".into(),
        ));
    }
    if !req.parameters.is_empty() {
        return Err(HiveError::InvalidArgument(
            "run parameters are not supported yet; set them on the card or the workflow".into(),
        ));
    }
    let card = parse_id(&req.card_id, "card_id")?;
    let hive = app.require_hive()?.to_owned();
    let catalog = app.catalog.load();
    let env = Env {
        store: app.store.as_ref(),
        directory: app.directory.as_ref(),
        catalog: &catalog,
        hive: &hive,
    };
    let (run, events) = start_run(&env, &ctx.op(), card).await?;
    app.bus.publish(&events);
    app.outbox.notify_one();
    Ok(to_proto(run))
}

pub async fn get_run(app: App, ctx: Ctx, req: GetRunRequest) -> Result<Run, HiveError> {
    let id = parse_id(&req.run_id, "run_id")?;
    let mut tx = app.store.begin(ctx.community).await?;
    let run = tx
        .runs()
        .get(id)
        .await?
        .ok_or_else(|| HiveError::NotFound(format!("run {id} does not exist")))?;
    Ok(to_proto(run))
}

pub async fn list_runs(
    app: App,
    ctx: Ctx,
    req: ListRunsRequest,
) -> Result<ListRunsResponse, HiveError> {
    let card = if req.card_id.is_empty() {
        None
    } else {
        Some(parse_id(&req.card_id, "card_id")?)
    };
    let mut tx = app.store.begin(ctx.community).await?;
    // The seq first, as ListCards does, so a change between the reads is never lost.
    let seq = tx.events().latest_seq().await?;
    let runs = tx.runs().list(card).await?;
    Ok(ListRunsResponse {
        runs: runs.into_iter().map(to_proto).collect(),
        seq,
    })
}

fn parse_id(value: &str, field: &str) -> Result<Uuid, HiveError> {
    if value.is_empty() {
        return Err(HiveError::InvalidArgument(format!("{field} is required")));
    }
    Uuid::parse_str(value).map_err(|_| HiveError::InvalidArgument(format!("{field} is not an id")))
}

fn to_proto(run: DomainRun) -> Run {
    Run {
        id: run.id.to_string(),
        card_id: run.card_id.to_string(),
        stage_id: run.current_stage.unwrap_or_default(),
        state: run.state.as_str().to_owned(),
        // Retries arrive with Phase 11; every run is its stage's first attempt until then.
        attempt: 1,
        started_at: Some(timestamp(run.started_at)),
        ended_at: run.ended_at.map(timestamp),
        visit: i32::try_from(run.visit).unwrap_or(i32::MAX),
        ..Default::default()
    }
}
