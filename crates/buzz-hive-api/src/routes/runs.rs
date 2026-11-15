//! Run procedures: start a card's workflow, read one run and list runs.

use buzz_hive_core::domain::run::{Run as DomainRun, RunStep as DomainStep};
use buzz_hive_core::orchestrator::run::start_run_at;
use uuid::Uuid;

use crate::app::App;
use crate::error::HiveError;
use crate::proto::{
    GetRunRequest, ListRunsRequest, ListRunsResponse, Run, RunStep, StartStageRequest,
};
use crate::routes::poll::timestamp;
use crate::unary::Ctx;

/// Starts the card's workflow at `stage_id`, or at its first stage, with the run's own
/// parameters. The posts reach the update channel when the outbox drains, which this call
/// wakes.
pub async fn start_stage(app: App, ctx: Ctx, req: StartStageRequest) -> Result<Run, HiveError> {
    let card = parse_id(&req.card_id, "card_id")?;
    let op = app.op(true)?;
    let stage = (!req.stage_id.is_empty()).then_some(req.stage_id.as_str());
    let parameters = req.parameters.into_iter().collect();
    let (run, events) = start_run_at(&op.env(), &ctx.op(), card, stage, parameters).await?;
    op.settle(&events);
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

pub(super) fn parse_id(value: &str, field: &str) -> Result<Uuid, HiveError> {
    if value.is_empty() {
        return Err(HiveError::InvalidArgument(format!("{field} is required")));
    }
    Uuid::parse_str(value).map_err(|_| HiveError::InvalidArgument(format!("{field} is not an id")))
}

pub(super) fn to_proto(run: DomainRun) -> Run {
    let (result, summary) = run
        .steps
        .iter()
        .rev()
        .find(|step| !step.result.is_empty())
        .map(|step| (step.result.clone(), step.summary.clone()))
        .unwrap_or_default();
    Run {
        result,
        summary,
        id: run.id.to_string(),
        card_id: run.card_id.to_string(),
        stage_id: run.current_stage.unwrap_or_default(),
        state: run.state.as_str().to_owned(),
        // Retries arrive with Phase 11; every run is its stage's first attempt until then.
        attempt: 1,
        started_at: Some(timestamp(run.started_at)),
        ended_at: run.ended_at.map(timestamp),
        visit: i32::try_from(run.visit).unwrap_or(i32::MAX),
        steps: run.steps.into_iter().map(step_to_proto).collect(),
        ..Default::default()
    }
}

fn step_to_proto(step: DomainStep) -> RunStep {
    let count = |n: u32| i32::try_from(n).unwrap_or(i32::MAX);
    RunStep {
        stage: step.stage,
        visit: count(step.visit),
        round: count(step.round),
        result: step.result,
        summary: step.summary,
        routed_to: step.routed_to.unwrap_or_default(),
        backward: step.backward,
        held: step.held,
        skipped: step.skipped,
        resumes: count(step.resumes),
    }
}
