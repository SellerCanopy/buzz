//! Stage procedures: the agent of a running stage reports its result or moves its card, and
//! a person skips, holds or resumes a stage.

use buzz_hive_core::orchestrator::run::{
    move_card as move_to, pause_card as hold, resume_card as release, skip_stage as skip,
    stage_done as report,
};

use crate::app::App;
use crate::error::HiveError;
use crate::proto::{
    Card, MoveCardRequest, PauseCardRequest, ResumeCardRequest, Run, SkipStageRequest,
    StageDoneRequest,
};
use crate::routes::cards::{card_of, to_proto as card_to_proto};
use crate::routes::runs::{parse_id, to_proto};
use crate::unary::Ctx;

/// The visit a request names. Zero is "not given", and a visit counts from 1.
fn visit_of(visit: i32) -> Result<u32, HiveError> {
    u32::try_from(visit)
        .ok()
        .filter(|v| *v > 0)
        .ok_or_else(|| HiveError::InvalidArgument("visit is required, from 1".into()))
}

/// Reports the result of the card's running stage. The caller must be the agent the stage
/// task mentioned: the level the route asks for is the least an agent needs, and the
/// orchestrator checks who is calling.
pub async fn stage_done(app: App, ctx: Ctx, req: StageDoneRequest) -> Result<Run, HiveError> {
    let card = parse_id(&req.card_id, "card_id")?;
    if req.result.is_empty() {
        return Err(HiveError::InvalidArgument("result is required".into()));
    }
    if req.stage.is_empty() {
        return Err(HiveError::InvalidArgument("stage is required".into()));
    }
    let visit = visit_of(req.visit)?;
    // A result can route the card to a stage whose task waits in the outbox.
    let op = app.op(true)?;
    let (run, events) = report(
        &op.env(),
        &ctx.op(),
        card,
        &req.stage,
        visit,
        &req.result,
        &req.summary,
    )
    .await?;
    op.settle(&events);
    Ok(to_proto(run))
}

/// The stage's agent moves its card to another stage. The workflow must allow it.
pub async fn move_card(app: App, ctx: Ctx, req: MoveCardRequest) -> Result<Run, HiveError> {
    let card = parse_id(&req.card_id, "card_id")?;
    if req.stage_id.is_empty() {
        return Err(HiveError::InvalidArgument("stage_id is required".into()));
    }
    if req.stage.is_empty() {
        return Err(HiveError::InvalidArgument("stage is required".into()));
    }
    let visit = visit_of(req.visit)?;
    let op = app.op(true)?;
    let (run, events) =
        move_to(&op.env(), &ctx.op(), card, &req.stage, visit, &req.stage_id).await?;
    op.settle(&events);
    Ok(to_proto(run))
}

/// A person moves a waiting or held card on, as if its stage had passed.
pub async fn skip_stage(app: App, ctx: Ctx, req: SkipStageRequest) -> Result<Card, HiveError> {
    let id = parse_id(&req.card_id, "card_id")?;
    let op = app.op(true)?;
    let (_, events) = skip(&op.env(), &ctx.op(), id).await?;
    op.settle(&events);
    Ok(card_to_proto(card_of(&app, &ctx, id).await?, false))
}

/// A person holds a running card.
pub async fn pause_card(app: App, ctx: Ctx, req: PauseCardRequest) -> Result<Card, HiveError> {
    let id = parse_id(&req.card_id, "card_id")?;
    let op = app.op(true)?;
    let (_, events) = hold(&op.env(), &ctx.op(), id).await?;
    op.settle(&events);
    Ok(card_to_proto(card_of(&app, &ctx, id).await?, false))
}

/// A person lets a held stage start, or a paused one carry on.
pub async fn resume_card(app: App, ctx: Ctx, req: ResumeCardRequest) -> Result<Run, HiveError> {
    let id = parse_id(&req.card_id, "card_id")?;
    let op = app.op(true)?;
    let (run, events) = release(&op.env(), &ctx.op(), id).await?;
    op.settle(&events);
    Ok(to_proto(run))
}
