//! The host supervisor: the small service outside the agents' containers that writes each
//! agent's tool servers and the Beehive settings every stage agent needs, then restarts it.
//!
//! An agent's containers cannot be reached from here, so the engine asks the supervisor. The
//! supervisor knows its agents by the folders under `deploy/aws/agents/` and takes nothing
//! else from a call.

use crate::domain::agent_login::{CallbackUrl, LoginState};
use crate::domain::secret::SecretNames;
use crate::domain::tool_server::ToolServer;
use crate::ports::secrets::SecretString;

/// What the supervisor reports about one agent's settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSettings {
    /// `BUZZ_ACP_SESSION_POLICY` as rendered, `channel` when unset.
    pub session_policy: String,
    /// Whether the Beehive account may start the agent's turns.
    pub allowlisted: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum SupervisorError {
    /// The agent has no folder in the supervisor's list, or no rendered environment yet.
    #[error("the supervisor does not know the agent: {0}")]
    NotFound(String),
    /// The supervisor refused the call, for example a spec it will not write, or a pasted
    /// address the sign-in did not accept.
    #[error("the supervisor refused the call: {0}")]
    Refused(String),
    /// The supervisor could not be reached or did not finish. Worth trying again.
    #[error("the supervisor is unavailable: {0}")]
    Unavailable(String),
}

#[async_trait::async_trait]
pub trait Supervisor: Send + Sync {
    /// Makes `servers` the agent's whole tool list, sets the settings every stage agent
    /// needs, fills its environment with the secrets `secrets` names (the supervisor reads
    /// the values itself, so none pass through here), and restarts the agent when its files
    /// changed. Returns whether it restarted. The call is safe to repeat: the same list
    /// changes nothing the second time.
    async fn apply(
        &self,
        agent: &str,
        servers: &[ToolServer],
        secrets: &SecretNames,
    ) -> Result<bool, SupervisorError>;

    /// Reads the agent's settings. Changes nothing.
    async fn settings(&self, agent: &str) -> Result<AgentSettings, SupervisorError>;

    /// Starts the OAuth sign-in for the agent's tool server and returns the link to open. The
    /// supervisor keeps the waiting process for ten minutes, one per agent and server, and a
    /// second start replaces it. The link prints as `[redacted]`.
    async fn login_start(&self, agent: &str, server: &str)
        -> Result<SecretString, SupervisorError>;

    /// Hands the address the browser ended on to the process `login_start` left waiting, and
    /// returns once the CLI took it. `NotFound` when nothing is waiting or it expired.
    async fn login_paste(
        &self,
        agent: &str,
        server: &str,
        url: &CallbackUrl,
    ) -> Result<(), SupervisorError>;

    /// Reads whether the agent's CLI holds a sign-in for the server. Changes nothing.
    async fn login_status(&self, agent: &str, server: &str) -> Result<LoginState, SupervisorError>;
}
