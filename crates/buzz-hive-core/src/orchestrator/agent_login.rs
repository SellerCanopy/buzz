//! Signing an agent in to an OAuth tool server, by a person.
//!
//! The admin starts the sign-in, opens the link, signs in at the provider and pastes back the
//! address the browser ended on. The host supervisor runs the Claude CLI's login in the agent's
//! container and holds the process; the engine only relays the link and the pasted address. It
//! keeps neither: no table, no event, no log line holds the link or the address, because the
//! address can carry the authorization code. A log line names who did what to which agent and
//! server.

use super::agent_tools::known;
use super::{Ctx, OpError};
use crate::access::ActorKind;
use crate::domain::agent_login::{CallbackUrl, LoginState};
use crate::domain::tool_server::check_name;
use crate::ports::secrets::SecretString;
use crate::ports::store::Store;
use crate::ports::supervisor::{Supervisor, SupervisorError};

/// Starts the sign-in and returns the link to open.
pub async fn start(
    store: &dyn Store,
    supervisor: Option<&dyn Supervisor>,
    ctx: &Ctx,
    agent: &str,
    server: &str,
) -> Result<SecretString, OpError> {
    let supervisor = checked(store, supervisor, ctx, agent, server).await?;
    let link = supervisor
        .login_start(agent, server)
        .await
        .map_err(refusal)?;
    tracing::info!(actor = %ctx.actor.pubkey, agent, server, "agent sign-in started");
    Ok(link)
}

/// Hands the pasted address to the waiting sign-in. `address` is checked here as well as in the
/// supervisor, and a refusal never repeats it.
pub async fn paste(
    store: &dyn Store,
    supervisor: Option<&dyn Supervisor>,
    ctx: &Ctx,
    agent: &str,
    server: &str,
    address: &str,
) -> Result<(), OpError> {
    let supervisor = checked(store, supervisor, ctx, agent, server).await?;
    let url = CallbackUrl::parse(address.trim()).map_err(|e| OpError::InvalidArgument(e.into()))?;
    let result = supervisor.login_paste(agent, server, &url).await;
    match &result {
        Ok(()) => tracing::info!(actor = %ctx.actor.pubkey, agent, server, "agent signed in"),
        Err(error) => tracing::warn!(
            actor = %ctx.actor.pubkey, agent, server, %error, "agent sign-in did not finish"
        ),
    }
    result.map_err(refusal)
}

/// Whether the agent holds a sign-in for the server.
pub async fn status(
    store: &dyn Store,
    supervisor: Option<&dyn Supervisor>,
    ctx: &Ctx,
    agent: &str,
    server: &str,
) -> Result<LoginState, OpError> {
    let supervisor = checked(store, supervisor, ctx, agent, server).await?;
    supervisor
        .login_status(agent, server)
        .await
        .map_err(refusal)
}

/// The checks every call starts with: a person, a known agent, a well formed server name and a
/// supervisor to ask.
async fn checked<'a>(
    store: &dyn Store,
    supervisor: Option<&'a dyn Supervisor>,
    ctx: &Ctx,
    agent: &str,
    server: &str,
) -> Result<&'a dyn Supervisor, OpError> {
    // A sign-in gives an agent a credential for a tool server, so only a person does it.
    if ctx.actor.kind == ActorKind::Agent {
        return Err(OpError::PermissionDenied(
            "an agent may not sign an agent in; ask a person".into(),
        ));
    }
    check_name(server).map_err(OpError::InvalidArgument)?;
    {
        let mut tx = store.begin(ctx.community).await?;
        known(&mut *tx, agent).await?;
    }
    supervisor.ok_or_else(|| {
        OpError::FailedPrecondition(
            "no supervisor is configured (BUZZ_HIVE_SUPERVISOR_SOCKET), so no agent can be signed in"
                .into(),
        )
    })
}

fn refusal(error: SupervisorError) -> OpError {
    match error {
        SupervisorError::NotFound(message) => OpError::NotFound(message),
        SupervisorError::Refused(message) => OpError::InvalidArgument(message),
        SupervisorError::Unavailable(message) => {
            OpError::FailedPrecondition(format!("the supervisor could not do that: {message}"))
        }
    }
}
