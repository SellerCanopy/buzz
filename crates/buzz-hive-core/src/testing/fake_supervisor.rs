use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::domain::tool_server::ToolServer;
use crate::ports::supervisor::{AgentSettings, Supervisor, SupervisorError};

#[derive(Default)]
struct Inner {
    /// What each deployed agent has, by name. An agent not here is not deployed.
    agents: BTreeMap<String, Deployed>,
    /// Errors the next applies fail with, oldest first.
    failures: Vec<SupervisorError>,
    /// Every apply, as the agent and the names it was given.
    applied: Vec<(String, Vec<String>)>,
}

struct Deployed {
    settings: AgentSettings,
    servers: Vec<ToolServer>,
}

/// A [`Supervisor`] held in memory. An agent applied to becomes `thread` and allowlisted, as
/// the real one makes it, and a test can start one with other settings.
#[derive(Clone, Default)]
pub struct FakeSupervisor {
    inner: Arc<Mutex<Inner>>,
}

impl FakeSupervisor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Deploys an agent with the settings a hand-made one might have.
    pub fn deploy(&self, agent: &str, session_policy: &str, allowlisted: bool) {
        self.inner.lock().unwrap().agents.insert(
            agent.to_owned(),
            Deployed {
                settings: AgentSettings {
                    session_policy: session_policy.to_owned(),
                    allowlisted,
                },
                servers: Vec::new(),
            },
        );
    }

    /// Makes the next apply fail with `error`.
    pub fn fail_next_apply(&self, error: SupervisorError) {
        self.inner.lock().unwrap().failures.push(error);
    }

    /// The servers the agent was last given.
    pub fn servers(&self, agent: &str) -> Option<Vec<ToolServer>> {
        self.inner
            .lock()
            .unwrap()
            .agents
            .get(agent)
            .map(|d| d.servers.clone())
    }

    /// Every apply made, with the server names each carried.
    pub fn applied(&self) -> Vec<(String, Vec<String>)> {
        self.inner.lock().unwrap().applied.clone()
    }
}

#[async_trait::async_trait]
impl Supervisor for FakeSupervisor {
    async fn apply(&self, agent: &str, servers: &[ToolServer]) -> Result<bool, SupervisorError> {
        let mut inner = self.inner.lock().unwrap();
        if !inner.failures.is_empty() {
            return Err(inner.failures.remove(0));
        }
        let Some(deployed) = inner.agents.get_mut(agent) else {
            return Err(SupervisorError::NotFound(agent.to_owned()));
        };
        let changed = deployed.servers != servers
            || deployed.settings.session_policy != "thread"
            || !deployed.settings.allowlisted;
        deployed.servers = servers.to_vec();
        deployed.settings = AgentSettings {
            session_policy: "thread".into(),
            allowlisted: true,
        };
        inner.applied.push((
            agent.to_owned(),
            servers.iter().map(|s| s.name.clone()).collect(),
        ));
        Ok(changed)
    }

    async fn settings(&self, agent: &str) -> Result<AgentSettings, SupervisorError> {
        self.inner
            .lock()
            .unwrap()
            .agents
            .get(agent)
            .map(|d| d.settings.clone())
            .ok_or_else(|| SupervisorError::NotFound(agent.to_owned()))
    }
}
