//! The tool servers each agent starts, and the settings every stage agent needs.
//!
//! The rows here are the truth: a person changes them, and the host supervisor writes each
//! agent's `mcp.json` from them and restarts the agent. A change is saved first and applied
//! second, so a supervisor that is down leaves the change kept and says so, and the next
//! change or start applies the whole list again. Agents are known from the folders under
//! `deploy/aws/agents/` read at a start (see [`seed`]); a call naming any other agent is
//! refused, so nothing a caller sends picks a path.

use serde_json::json;

use super::{Ctx, OpError};
use crate::access::ActorKind;
use crate::domain::events::Event;
use crate::domain::tool_server::{
    check_agent, check_name, parse_mcp, ToolServer, MAX_SERVERS_PER_AGENT,
};
use crate::ports::store::{CommunityId, Store, StoreError};
use crate::ports::supervisor::{Supervisor, SupervisorError};

const KIND: &str = "agent_tools.changed";

/// Every known agent and every server, by agent then name.
pub struct Listed {
    pub agents: Vec<String>,
    pub servers: Vec<(String, ToolServer)>,
}

/// What a change did: the events it committed, and whether the agent has it yet.
pub struct Saved {
    pub events: Vec<Event>,
    /// Whether the agent restarted, or why it did not get the change. The change is saved
    /// either way.
    pub applied: Result<bool, SupervisorError>,
}

pub async fn list(store: &dyn Store, ctx: &Ctx) -> Result<Listed, OpError> {
    let mut tx = store.begin(ctx.community).await?;
    Ok(Listed {
        agents: tx.agent_tools().agents().await?,
        servers: tx.agent_tools().list().await?,
    })
}

/// Adds the server to the agent, or replaces the one with its name, then applies the agent's
/// whole list.
pub async fn set(
    store: &dyn Store,
    supervisor: Option<&dyn Supervisor>,
    ctx: &Ctx,
    agent: &str,
    server: ToolServer,
) -> Result<Saved, OpError> {
    people_only(ctx)?;
    check_name(&server.name).map_err(OpError::InvalidArgument)?;
    // The checks run again on the parts, so a server built by hand is held to them too.
    let server = ToolServer::new(&server.name, server.transport, &server.spec)
        .map_err(OpError::InvalidArgument)?;
    let mut tx = store.begin(ctx.community).await?;
    known(&mut *tx, agent).await?;
    let have = tx.agent_tools().of_agent(agent).await?;
    if !have.iter().any(|s| s.name == server.name) && have.len() >= MAX_SERVERS_PER_AGENT {
        return Err(OpError::FailedPrecondition(format!(
            "an agent may have at most {MAX_SERVERS_PER_AGENT} tool servers"
        )));
    }
    tx.agent_tools().set(agent, &server).await?;
    changed(&mut *tx, ctx, agent, &server.name, "set").await?;
    let events = tx.commit().await?;
    let applied = apply(store, supervisor, ctx.community, agent).await;
    log(ctx, agent, &server.name, "set", &applied);
    Ok(Saved { events, applied })
}

/// Takes the server off the agent, then applies the agent's whole list. `NotFound` when the
/// agent has no server of that name.
pub async fn remove(
    store: &dyn Store,
    supervisor: Option<&dyn Supervisor>,
    ctx: &Ctx,
    agent: &str,
    name: &str,
) -> Result<Saved, OpError> {
    people_only(ctx)?;
    let mut tx = store.begin(ctx.community).await?;
    known(&mut *tx, agent).await?;
    if !tx.agent_tools().remove(agent, name).await? {
        return Err(OpError::NotFound(format!(
            "agent {agent} has no tool server named {name}"
        )));
    }
    changed(&mut *tx, ctx, agent, name, "removed").await?;
    let events = tx.commit().await?;
    let applied = apply(store, supervisor, ctx.community, agent).await;
    log(ctx, agent, name, "removed", &applied);
    Ok(Saved { events, applied })
}

/// An agent changes what tools it has only through a person: a tool server runs code with the
/// agent's keys.
fn people_only(ctx: &Ctx) -> Result<(), OpError> {
    if ctx.actor.kind == ActorKind::Agent {
        return Err(OpError::PermissionDenied(
            "an agent may not change tool servers; ask a person".into(),
        ));
    }
    Ok(())
}

pub(super) async fn known(
    tx: &mut dyn crate::ports::store::Tx,
    agent: &str,
) -> Result<(), OpError> {
    check_agent(agent).map_err(OpError::InvalidArgument)?;
    // Holds the agent until the transaction ends, so a cap checked after this stays true at
    // commit: two changes for one agent take turns.
    if !tx.agent_tools().lock_agent(agent).await? {
        return Err(OpError::NotFound(format!(
            "no agent {agent} has a folder in deploy/aws/agents; add the folder and deploy first"
        )));
    }
    Ok(())
}

async fn changed(
    tx: &mut dyn crate::ports::store::Tx,
    ctx: &Ctx,
    agent: &str,
    server: &str,
    action: &str,
) -> Result<(), StoreError> {
    tx.events()
        .append(Event::new(
            KIND,
            None,
            None,
            ctx.actor.clone(),
            json!({ "agent": agent, "server": server, "action": action }),
        ))
        .await
}

/// Sends the agent's whole saved list and the names of its secrets to the supervisor.
pub async fn apply(
    store: &dyn Store,
    supervisor: Option<&dyn Supervisor>,
    community: CommunityId,
    agent: &str,
) -> Result<bool, SupervisorError> {
    let Some(supervisor) = supervisor else {
        return Err(SupervisorError::Unavailable(
            "no supervisor is configured (BUZZ_HIVE_SUPERVISOR_SOCKET)".into(),
        ));
    };
    let (servers, secrets) = async {
        let mut tx = store.begin(community).await?;
        Ok::<_, StoreError>((
            tx.agent_tools().of_agent(agent).await?,
            tx.secrets().names_of(agent).await?,
        ))
    }
    .await
    .map_err(|e| SupervisorError::Unavailable(e.to_string()))?;
    supervisor.apply(agent, &servers, &secrets).await
}

fn log(
    ctx: &Ctx,
    agent: &str,
    server: &str,
    action: &str,
    applied: &Result<bool, SupervisorError>,
) {
    match applied {
        Ok(restarted) => tracing::info!(
            actor = %ctx.actor.pubkey, agent, server, action, restarted, "tool server changed"
        ),
        Err(error) => tracing::warn!(
            actor = %ctx.actor.pubkey, agent, server, action, %error,
            "tool server changed but not applied"
        ),
    }
}

/// Reads one agent folder's `mcp.json` into the store, once. Returns whether it seeded: `false`
/// when the agent was seeded before, so a server a person removed does not come back.
pub async fn seed(
    store: &dyn Store,
    community: CommunityId,
    agent: &str,
    mcp_json: Option<&str>,
) -> Result<bool, OpError> {
    check_agent(agent).map_err(OpError::InvalidArgument)?;
    let servers = match mcp_json {
        Some(text) => parse_mcp(text)
            .map_err(|e| OpError::InvalidArgument(format!("{agent}/mcp.json: {e}")))?,
        None => Vec::new(),
    };
    let mut tx = store.begin(community).await?;
    let seeded = tx.agent_tools().seed(agent, &servers).await?;
    tx.commit().await?;
    Ok(seeded)
}

/// One agent the engine found wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentIssue {
    pub agent: String,
    pub session_policy: String,
    pub allowlisted: bool,
    pub problem: String,
}

/// What the supervisor says about every known agent: `("yes", [])` when each is on
/// `session_policy: thread` and answers the Beehive account, `("no", issues)` when some is
/// not, and `("unknown", [])` when there is no supervisor or it did not answer. An agent the
/// supervisor does not know is not deployed and is left out.
pub async fn settings_check(
    store: &dyn Store,
    supervisor: Option<&dyn Supervisor>,
    ctx: &Ctx,
) -> Result<(String, Vec<AgentIssue>), OpError> {
    let Some(supervisor) = supervisor else {
        return Ok(("unknown".into(), Vec::new()));
    };
    let agents = {
        let mut tx = store.begin(ctx.community).await?;
        tx.agent_tools().agents().await?
    };
    let mut issues = Vec::new();
    for agent in agents {
        match supervisor.settings(&agent).await {
            Ok(settings) => {
                let mut wrong = Vec::new();
                if settings.session_policy != "thread" {
                    wrong.push(format!(
                        "session_policy is {}, and a stage thread needs thread",
                        settings.session_policy
                    ));
                }
                if !settings.allowlisted {
                    wrong.push("the Beehive account is not in its respond-to list".to_owned());
                }
                if !wrong.is_empty() {
                    issues.push(AgentIssue {
                        agent,
                        session_policy: settings.session_policy,
                        allowlisted: settings.allowlisted,
                        problem: wrong.join("; "),
                    });
                }
            }
            Err(SupervisorError::NotFound(_)) => {}
            Err(error) => {
                tracing::warn!(%agent, %error, "the supervisor could not report an agent's settings");
                return Ok(("unknown".into(), Vec::new()));
            }
        }
    }
    let answer = if issues.is_empty() { "yes" } else { "no" };
    Ok((answer.into(), issues))
}
