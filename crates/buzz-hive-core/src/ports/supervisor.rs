//! The host supervisor: the small service outside the agents' containers that writes each
//! agent's tool servers and the Beehive settings every stage agent needs, then restarts it.
//!
//! An agent's containers cannot be reached from here, so the engine asks the supervisor. The
//! supervisor knows its agents by the folders under `deploy/aws/agents/` and takes nothing
//! else from a call.

use crate::domain::tool_server::ToolServer;

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
    /// The supervisor refused the call, for example a spec it will not write.
    #[error("the supervisor refused the call: {0}")]
    Refused(String),
    /// The supervisor could not be reached or did not finish. Worth trying again.
    #[error("the supervisor is unavailable: {0}")]
    Unavailable(String),
}

#[async_trait::async_trait]
pub trait Supervisor: Send + Sync {
    /// Makes `servers` the agent's whole tool list, sets the settings every stage agent
    /// needs, and restarts the agent when its files changed. Returns whether it restarted.
    /// The call is safe to repeat: the same list changes nothing the second time.
    async fn apply(&self, agent: &str, servers: &[ToolServer]) -> Result<bool, SupervisorError>;

    /// Reads the agent's settings. Changes nothing.
    async fn settings(&self, agent: &str) -> Result<AgentSettings, SupervisorError>;
}
