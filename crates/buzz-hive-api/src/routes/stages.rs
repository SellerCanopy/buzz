//! Stage procedures: the agent of a running stage reports its result.

use buzz_hive_core::orchestrator::run::stage_done as report;
use buzz_hive_core::orchestrator::Env;

use crate::app::App;
use crate::error::HiveError;
use crate::proto::{Run, StageDoneRequest};
use crate::routes::runs::{parse_id, to_proto};
use crate::unary::Ctx;

/// Reports the result of the card's running stage. The caller must be the agent the stage
/// task mentioned: the level the route asks for is the least an agent needs, and the
/// orchestrator checks who is calling.
pub async fn stage_done(app: App, ctx: Ctx, req: StageDoneRequest) -> Result<Run, HiveError> {
    let card = parse_id(&req.card_id, "card_id")?;
    if req.result.is_empty() {
        return Err(HiveError::InvalidArgument("result is required".into()));
    }
    let hive = app.hive_pubkey.clone();
    let catalog = app.catalog.load();
    let env = Env {
        store: app.store.as_ref(),
        directory: app.directory.as_ref(),
        catalog: &catalog,
        hive: &hive,
    };
    let (run, events) = report(&env, &ctx.op(), card, &req.result, &req.summary).await?;
    app.bus.publish(&events);
    Ok(to_proto(run))
}
