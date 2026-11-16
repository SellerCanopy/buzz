use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::domain::agent_login::{CallbackUrl, LoginState};
use crate::domain::secret::SecretNames;
use crate::domain::tool_server::ToolServer;
use crate::ports::secrets::SecretString;
use crate::ports::supervisor::{AgentSettings, Supervisor, SupervisorError};

#[derive(Default)]
struct Inner {
    /// What each deployed agent has, by name. An agent not here is not deployed.
    agents: BTreeMap<String, Deployed>,
    /// Errors the next applies fail with, oldest first.
    failures: Vec<SupervisorError>,
    /// Every apply, as the agent and the names it was given.
    applied: Vec<(String, Vec<String>)>,
    /// Sign-ins started and not yet pasted, by agent and server.
    waiting: Vec<(String, String)>,
    /// Servers an agent is signed in to.
    signed_in: Vec<(String, String)>,
    /// Every address pasted, in order. Test-only: the real engine never keeps one.
    pasted: Vec<String>,
    /// Every call to a login method, as `method agent server`.
    login_calls: Vec<String>,
}

struct Deployed {
    settings: AgentSettings,
    servers: Vec<ToolServer>,
    secrets: SecretNames,
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
                secrets: SecretNames::default(),
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

    /// The secret names the agent was last given.
    pub fn secrets(&self, agent: &str) -> Option<SecretNames> {
        self.inner
            .lock()
            .unwrap()
            .agents
            .get(agent)
            .map(|d| d.secrets.clone())
    }

    /// Every apply made, with the server names each carried.
    pub fn applied(&self) -> Vec<(String, Vec<String>)> {
        self.inner.lock().unwrap().applied.clone()
    }

    /// The link `login_start` returns for a server.
    pub fn link(agent: &str, server: &str) -> String {
        format!("https://auth.example.org/authorize?agent={agent}&server={server}&state=fake")
    }

    /// Every address pasted, in order.
    pub fn pasted(&self) -> Vec<String> {
        self.inner.lock().unwrap().pasted.clone()
    }

    /// Every login call made, as `method agent server`.
    pub fn login_calls(&self) -> Vec<String> {
        self.inner.lock().unwrap().login_calls.clone()
    }
}

#[async_trait::async_trait]
impl Supervisor for FakeSupervisor {
    async fn apply(
        &self,
        agent: &str,
        servers: &[ToolServer],
        secrets: &SecretNames,
    ) -> Result<bool, SupervisorError> {
        let mut inner = self.inner.lock().unwrap();
        if !inner.failures.is_empty() {
            return Err(inner.failures.remove(0));
        }
        let Some(deployed) = inner.agents.get_mut(agent) else {
            return Err(SupervisorError::NotFound(agent.to_owned()));
        };
        let changed = deployed.servers != servers
            || deployed.secrets != *secrets
            || deployed.settings.session_policy != "thread"
            || !deployed.settings.allowlisted;
        deployed.servers = servers.to_vec();
        deployed.secrets = secrets.clone();
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

    async fn login_start(
        &self,
        agent: &str,
        server: &str,
    ) -> Result<SecretString, SupervisorError> {
        let mut inner = self.inner.lock().unwrap();
        inner.login_calls.push(format!("start {agent} {server}"));
        if !inner.agents.contains_key(agent) {
            return Err(SupervisorError::NotFound(agent.to_owned()));
        }
        let key = (agent.to_owned(), server.to_owned());
        inner.waiting.retain(|k| *k != key);
        inner.waiting.push(key);
        Ok(SecretString::new(Self::link(agent, server)))
    }

    async fn login_paste(
        &self,
        agent: &str,
        server: &str,
        url: &CallbackUrl,
    ) -> Result<(), SupervisorError> {
        let mut inner = self.inner.lock().unwrap();
        inner.login_calls.push(format!("paste {agent} {server}"));
        let key = (agent.to_owned(), server.to_owned());
        let Some(at) = inner.waiting.iter().position(|k| *k == key) else {
            return Err(SupervisorError::NotFound(
                "no sign-in is waiting for this tool server; start it again".into(),
            ));
        };
        inner.waiting.remove(at);
        inner.pasted.push(url.expose().to_owned());
        inner.signed_in.push(key);
        Ok(())
    }

    async fn login_status(&self, agent: &str, server: &str) -> Result<LoginState, SupervisorError> {
        let mut inner = self.inner.lock().unwrap();
        inner.login_calls.push(format!("status {agent} {server}"));
        if !inner.agents.contains_key(agent) {
            return Err(SupervisorError::NotFound(agent.to_owned()));
        }
        let key = (agent.to_owned(), server.to_owned());
        Ok(if inner.signed_in.contains(&key) {
            LoginState::Connected
        } else {
            LoginState::NeedsSignIn
        })
    }
}
