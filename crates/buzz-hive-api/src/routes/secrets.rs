//! Agent secrets and team secrets: write-only. A value goes to the secret store and comes back
//! nowhere, and a name is all a response, an event or a log line holds.

use buzz_hive_core::orchestrator::secrets::{self, Saved};
use buzz_hive_core::ports::secrets::SecretString;

use crate::app::App;
use crate::error::HiveError;
use crate::proto::{
    AgentSecretName, GrantTeamSecretRequest, ListSecretNamesRequest, ListSecretNamesResponse,
    RevokeTeamSecretRequest, SecretApplied, SecretChange, SecretGrant, SecretName,
    SetAgentSecretRequest, SetTeamSecretRequest,
};
use crate::routes::poll::timestamp;
use crate::unary::Ctx;

/// The routes whose request body holds a secret value. A body that does not decode is refused
/// without echoing any of it.
pub const WRITE_ONLY: &[&str] = &["SetTeamSecret", "SetAgentSecret"];

pub async fn list_secret_names(
    app: App,
    ctx: Ctx,
    _req: ListSecretNamesRequest,
) -> Result<ListSecretNamesResponse, HiveError> {
    let listed = secrets::list(app.store.as_ref(), &ctx.op()).await?;
    Ok(ListSecretNamesResponse {
        team: listed
            .team
            .into_iter()
            .map(|s| name(s.var, s.updated_by, s.updated_at))
            .collect(),
        agents: listed
            .agents
            .into_iter()
            .map(|s| AgentSecretName {
                agent: s.agent,
                secret: Some(name(s.var, s.updated_by, s.updated_at)),
            })
            .collect(),
        grants: listed
            .grants
            .into_iter()
            .map(|(agent, name)| SecretGrant { agent, name })
            .collect(),
    })
}

pub async fn set_team_secret(
    app: App,
    ctx: Ctx,
    req: SetTeamSecretRequest,
) -> Result<SecretChange, HiveError> {
    let saved = secrets::set_team(
        app.store.as_ref(),
        app.secrets.as_deref(),
        app.supervisor.as_deref(),
        &ctx.op(),
        &req.name,
        SecretString::new(req.value),
    )
    .await?;
    finish(&app, saved)
}

pub async fn set_agent_secret(
    app: App,
    ctx: Ctx,
    req: SetAgentSecretRequest,
) -> Result<SecretChange, HiveError> {
    let saved = secrets::set_agent(
        app.store.as_ref(),
        app.secrets.as_deref(),
        app.supervisor.as_deref(),
        &ctx.op(),
        &req.agent,
        &req.name,
        SecretString::new(req.value),
    )
    .await?;
    finish(&app, saved)
}

pub async fn grant_team_secret(
    app: App,
    ctx: Ctx,
    req: GrantTeamSecretRequest,
) -> Result<SecretChange, HiveError> {
    let saved = secrets::grant(
        app.store.as_ref(),
        app.supervisor.as_deref(),
        &ctx.op(),
        &req.agent,
        &req.name,
    )
    .await?;
    finish(&app, saved)
}

pub async fn revoke_team_secret(
    app: App,
    ctx: Ctx,
    req: RevokeTeamSecretRequest,
) -> Result<SecretChange, HiveError> {
    let saved = secrets::revoke(
        app.store.as_ref(),
        app.supervisor.as_deref(),
        &ctx.op(),
        &req.agent,
        &req.name,
    )
    .await?;
    finish(&app, saved)
}

/// Publishes the change's events and says which agents it reached.
fn finish(app: &App, saved: Saved) -> Result<SecretChange, HiveError> {
    app.op(false)?.settle(&saved.events);
    Ok(SecretChange {
        applied: saved
            .applied
            .into_iter()
            .map(|a| match a.result {
                Ok(restarted) => SecretApplied {
                    agent: a.agent,
                    applied: true,
                    restarted,
                    apply_error: String::new(),
                },
                Err(error) => SecretApplied {
                    agent: a.agent,
                    applied: false,
                    restarted: false,
                    apply_error: error.to_string(),
                },
            })
            .collect(),
    })
}

fn name(var: String, updated_by: String, updated_at: chrono::DateTime<chrono::Utc>) -> SecretName {
    SecretName {
        name: var,
        updated_by,
        updated_at: Some(timestamp(updated_at)),
    }
}
