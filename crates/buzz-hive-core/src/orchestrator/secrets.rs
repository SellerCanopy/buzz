//! Agent secrets and team secrets: set by a person, written only, and filled in by the host
//! supervisor.
//!
//! The engine puts a value into the secret store and keeps only its name, who set it and when.
//! It never reads a value back: the supervisor reads each value from the store and writes it
//! into the agent's environment, for the agent's own secrets and for each team secret granted
//! to it. An agent with no grant gets no team secret. As with tool servers, a change is saved
//! first and applied second, so a supervisor that is down leaves the change kept and says so.
//!
//! A value is held in a [`SecretString`] from the request to the store and appears in no
//! event, log line, error or return value here.

use serde_json::json;

use super::agent_tools::{apply, known};
use super::{Ctx, OpError};
use crate::access::ActorKind;
use crate::domain::events::Event;
use crate::domain::secret::{check_value, check_var, SecretNames, MAX_SECRETS_PER_AGENT};
use crate::ports::secrets::{SecretError, SecretScope, SecretSink, SecretString};
use crate::ports::store::{AgentSecret, Store, StoreError, TeamSecret};
use crate::ports::supervisor::{Supervisor, SupervisorError};

const KIND: &str = "secret.changed";

/// Every secret's name, and who has which team secret.
pub struct Listed {
    pub team: Vec<TeamSecret>,
    pub agents: Vec<AgentSecret>,
    /// `(agent, var)`, by agent then name.
    pub grants: Vec<(String, String)>,
}

/// One agent a change was applied to.
#[derive(Debug)]
pub struct Applied {
    pub agent: String,
    /// Whether the agent restarted, or why it did not get the change.
    pub result: Result<bool, SupervisorError>,
}

/// What a change did: the events it committed, and the agents it was applied to.
#[derive(Debug)]
pub struct Saved {
    pub events: Vec<Event>,
    /// Empty when no agent holds the secret yet. The change is saved either way.
    pub applied: Vec<Applied>,
}

pub async fn list(store: &dyn Store, ctx: &Ctx) -> Result<Listed, OpError> {
    people_only(ctx)?;
    let mut tx = store.begin(ctx.community).await?;
    Ok(Listed {
        team: tx.secrets().team().await?,
        agents: tx.secrets().agent_secrets().await?,
        grants: tx.secrets().grants().await?,
    })
}

/// Sets or replaces the team secret `var`, then applies every agent it is granted to, so a
/// rotated value reaches them.
pub async fn set_team(
    store: &dyn Store,
    sink: Option<&dyn SecretSink>,
    supervisor: Option<&dyn Supervisor>,
    ctx: &Ctx,
    var: &str,
    value: SecretString,
) -> Result<Saved, OpError> {
    people_only(ctx)?;
    check(var, &value)?;
    put(sink, SecretScope::Team, var, value).await?;
    let mut tx = store.begin(ctx.community).await?;
    tx.secrets().set_team(var, &ctx.actor.pubkey).await?;
    changed(&mut *tx, ctx, None, var, "set").await?;
    // Read with the change, so a failed read cannot leave a saved change with no agent applied.
    let granted: Vec<String> = tx
        .secrets()
        .grants()
        .await?
        .into_iter()
        .filter(|(_, v)| v == var)
        .map(|(agent, _)| agent)
        .collect();
    let events = tx.commit().await?;
    log(ctx, None, var, "set");
    Ok(Saved {
        events,
        applied: apply_all(store, supervisor, ctx, granted).await,
    })
}

/// Sets or replaces the agent's own secret `var`, then applies the agent.
pub async fn set_agent(
    store: &dyn Store,
    sink: Option<&dyn SecretSink>,
    supervisor: Option<&dyn Supervisor>,
    ctx: &Ctx,
    agent: &str,
    var: &str,
    value: SecretString,
) -> Result<Saved, OpError> {
    people_only(ctx)?;
    check(var, &value)?;
    {
        // A fast path, so a refusal does not wait for the store: the count is taken again
        // under the agent's lock before the name is saved.
        let mut tx = store.begin(ctx.community).await?;
        known(&mut *tx, agent).await?;
        check_room(&mut *tx, agent, |n| n.own.iter().any(|v| v == var)).await?;
    }
    put(sink, SecretScope::Agent(agent.to_owned()), var, value).await?;
    let mut tx = store.begin(ctx.community).await?;
    // The count above was taken before the (slow) put, in a transaction that is gone. Count
    // again under the agent's lock, so a grant or secret that landed meanwhile is seen.
    known(&mut *tx, agent).await?;
    check_room(&mut *tx, agent, |n| n.own.iter().any(|v| v == var)).await?;
    tx.secrets()
        .set_agent(agent, var, &ctx.actor.pubkey)
        .await?;
    changed(&mut *tx, ctx, Some(agent), var, "set").await?;
    let events = tx.commit().await?;
    log(ctx, Some(agent), var, "set");
    Ok(Saved {
        events,
        applied: apply_all(store, supervisor, ctx, vec![agent.to_owned()]).await,
    })
}

/// Gives the agent the team secret `var`, then applies the agent. `NotFound` when no team
/// secret has that name, and no change when the agent already has it.
pub async fn grant(
    store: &dyn Store,
    supervisor: Option<&dyn Supervisor>,
    ctx: &Ctx,
    agent: &str,
    var: &str,
) -> Result<Saved, OpError> {
    people_only(ctx)?;
    check_var(var).map_err(OpError::InvalidArgument)?;
    let mut tx = store.begin(ctx.community).await?;
    known(&mut *tx, agent).await?;
    check_room(&mut *tx, agent, |n| n.team.iter().any(|v| v == var)).await?;
    let added = tx.secrets().grant(agent, var).await?;
    if added {
        changed(&mut *tx, ctx, Some(agent), var, "granted").await?;
    }
    let events = tx.commit().await?;
    log(ctx, Some(agent), var, "granted");
    Ok(Saved {
        events,
        applied: apply_all(store, supervisor, ctx, vec![agent.to_owned()]).await,
    })
}

/// Takes the team secret `var` from the agent, then applies the agent, which removes it from
/// the agent's environment. `NotFound` when the agent did not have it.
pub async fn revoke(
    store: &dyn Store,
    supervisor: Option<&dyn Supervisor>,
    ctx: &Ctx,
    agent: &str,
    var: &str,
) -> Result<Saved, OpError> {
    people_only(ctx)?;
    let mut tx = store.begin(ctx.community).await?;
    known(&mut *tx, agent).await?;
    if !tx.secrets().revoke(agent, var).await? {
        return Err(OpError::NotFound(format!(
            "agent {agent} is not granted a team secret named {var}"
        )));
    }
    changed(&mut *tx, ctx, Some(agent), var, "revoked").await?;
    let events = tx.commit().await?;
    log(ctx, Some(agent), var, "revoked");
    Ok(Saved {
        events,
        applied: apply_all(store, supervisor, ctx, vec![agent.to_owned()]).await,
    })
}

/// A secret is set only by a person: an agent that could set or grant one could hand itself
/// another agent's keys.
fn people_only(ctx: &Ctx) -> Result<(), OpError> {
    if ctx.actor.kind == ActorKind::Agent {
        return Err(OpError::PermissionDenied(
            "an agent may not manage secrets; ask a person".into(),
        ));
    }
    Ok(())
}

/// The name and value checks. A refusal says what is wrong and never repeats the value.
fn check(var: &str, value: &SecretString) -> Result<(), OpError> {
    check_var(var).map_err(OpError::InvalidArgument)?;
    check_value(value.expose()).map_err(OpError::InvalidArgument)
}

/// Refuses a name that is new to the agent (`held` says whether it already has it) when the
/// agent already holds the most it may. Call it after [`known`], which takes the agent's lock,
/// so the count holds until the commit.
async fn check_room(
    tx: &mut dyn crate::ports::store::Tx,
    agent: &str,
    held: impl FnOnce(&SecretNames) -> bool,
) -> Result<(), OpError> {
    let names = tx.secrets().names_of(agent).await?;
    if !held(&names) && names.own.len() + names.team.len() >= MAX_SECRETS_PER_AGENT {
        return Err(OpError::FailedPrecondition(format!(
            "an agent may have at most {MAX_SECRETS_PER_AGENT} secrets"
        )));
    }
    Ok(())
}

async fn put(
    sink: Option<&dyn SecretSink>,
    scope: SecretScope,
    var: &str,
    value: SecretString,
) -> Result<(), OpError> {
    let Some(sink) = sink else {
        return Err(OpError::FailedPrecondition(
            "no secret store is configured: set BUZZ_HIVE_SECRETS_DIR (local) or BUZZ_HIVE_SECRETS_BACKEND=ssm, then restart Beehive".into(),
        ));
    };
    sink.put(scope, var, value)
        .await
        .map_err(|SecretError::Unavailable(cause)| {
            StoreError::Unavailable(format!("the secret store: {cause}")).into()
        })
}

async fn changed(
    tx: &mut dyn crate::ports::store::Tx,
    ctx: &Ctx,
    agent: Option<&str>,
    var: &str,
    action: &str,
) -> Result<(), StoreError> {
    let payload = match agent {
        Some(agent) => json!({ "scope": "agent", "agent": agent, "var": var, "action": action }),
        None => json!({ "scope": "team", "var": var, "action": action }),
    };
    tx.events()
        .append(Event::new(KIND, None, None, ctx.actor.clone(), payload))
        .await
}

async fn apply_all(
    store: &dyn Store,
    supervisor: Option<&dyn Supervisor>,
    ctx: &Ctx,
    agents: Vec<String>,
) -> Vec<Applied> {
    let mut done = Vec::new();
    for agent in agents {
        let result = apply(store, supervisor, ctx.community, &agent).await;
        match &result {
            Ok(restarted) => tracing::info!(%agent, restarted, "secrets applied"),
            Err(error) => tracing::warn!(%agent, %error, "secret saved but not applied"),
        }
        done.push(Applied { agent, result });
    }
    done
}

fn log(ctx: &Ctx, agent: Option<&str>, var: &str, action: &str) {
    tracing::info!(actor = %ctx.actor.pubkey, agent, var, action, "secret changed");
}
