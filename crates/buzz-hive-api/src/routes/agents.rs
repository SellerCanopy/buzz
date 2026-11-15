//! The Buzz agents a workflow stage can name.

use crate::app::App;
use crate::error::HiveError;
use crate::proto::{AgentInfo, ListAgentsRequest, ListAgentsResponse};
use crate::unary::Ctx;

pub async fn list_agents(
    app: App,
    ctx: Ctx,
    _req: ListAgentsRequest,
) -> Result<ListAgentsResponse, HiveError> {
    let agents = app.directory.list_agents(ctx.community).await?;
    Ok(ListAgentsResponse {
        agents: agents
            .into_iter()
            .map(|a| AgentInfo {
                name: a.name,
                pubkey: a.pubkey,
                owner_pubkey: a.owner.unwrap_or_default(),
            })
            .collect(),
    })
}
