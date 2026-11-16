//! The tool servers each agent starts, set by a person and applied by the host supervisor.

use buzz_hive_core::domain::tool_server::{ToolServer, Transport};
use buzz_hive_core::orchestrator::agent_tools::{self, Saved};
use buzz_hive_core::ports::supervisor::SupervisorError;

use crate::app::App;
use crate::error::HiveError;
use crate::proto::{
    AgentToolServer, AgentToolServers, AgentToolsChange, ListAgentToolServersRequest,
    ListAgentToolServersResponse, RemoveAgentToolServerRequest, SetAgentToolServerRequest,
};
use crate::unary::Ctx;

pub async fn list_agent_tool_servers(
    app: App,
    ctx: Ctx,
    _req: ListAgentToolServersRequest,
) -> Result<ListAgentToolServersResponse, HiveError> {
    let listed = agent_tools::list(app.store.as_ref(), &ctx.op()).await?;
    Ok(ListAgentToolServersResponse {
        agents: listed
            .agents
            .iter()
            .map(|agent| {
                grouped(
                    agent,
                    listed
                        .servers
                        .iter()
                        .filter(|(a, _)| a == agent)
                        .map(|(_, s)| s),
                )
            })
            .collect(),
    })
}

pub async fn set_agent_tool_server(
    app: App,
    ctx: Ctx,
    req: SetAgentToolServerRequest,
) -> Result<AgentToolsChange, HiveError> {
    let given = req
        .server
        .ok_or_else(|| HiveError::InvalidArgument("server is required".into()))?;
    let transport = Transport::parse(&given.transport).ok_or_else(|| {
        HiveError::InvalidArgument(format!(
            "transport {:?} is not stdio or http",
            given.transport
        ))
    })?;
    let spec = serde_json::from_str(&given.spec_json)
        .map_err(|e| HiveError::InvalidArgument(format!("spec_json is not JSON: {e}")))?;
    let server = ToolServer {
        name: given.name,
        transport,
        spec,
    };
    let saved = agent_tools::set(
        app.store.as_ref(),
        app.supervisor.as_deref(),
        &ctx.op(),
        &req.agent,
        server,
    )
    .await?;
    finish(&app, ctx, &req.agent, saved).await
}

pub async fn remove_agent_tool_server(
    app: App,
    ctx: Ctx,
    req: RemoveAgentToolServerRequest,
) -> Result<AgentToolsChange, HiveError> {
    let saved = agent_tools::remove(
        app.store.as_ref(),
        app.supervisor.as_deref(),
        &ctx.op(),
        &req.agent,
        &req.name,
    )
    .await?;
    finish(&app, ctx, &req.agent, saved).await
}

/// Publishes the change's events and answers with the agent's list as it now stands.
async fn finish(
    app: &App,
    ctx: Ctx,
    agent: &str,
    saved: Saved,
) -> Result<AgentToolsChange, HiveError> {
    app.op(false)?.settle(&saved.events);
    let now = agent_tools::list(app.store.as_ref(), &ctx.op()).await?;
    let (applied, restarted, apply_error) = match saved.applied {
        Ok(restarted) => (true, restarted, String::new()),
        Err(error) => (false, false, reason(&error)),
    };
    Ok(AgentToolsChange {
        agent: Some(grouped(
            agent,
            now.servers
                .iter()
                .filter(|(a, _)| a == agent)
                .map(|(_, s)| s),
        )),
        applied,
        restarted,
        apply_error,
    })
}

fn reason(error: &SupervisorError) -> String {
    error.to_string()
}

fn grouped<'a>(agent: &str, servers: impl Iterator<Item = &'a ToolServer>) -> AgentToolServers {
    AgentToolServers {
        agent: agent.to_owned(),
        servers: servers
            .map(|s| AgentToolServer {
                name: s.name.clone(),
                transport: s.transport.as_str().to_owned(),
                spec_json: s.spec.to_string(),
            })
            .collect(),
    }
}
