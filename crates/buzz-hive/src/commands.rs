//! The work of the two one-shot commands the host's deploy scripts run, apart from reading the
//! environment and printing, so each can be tested on its own.

use buzz_hive_core::orchestrator::agent_tools;
use buzz_hive_core::ports::store::{CommunityId, Store};
use buzz_hive_core::ports::supervisor::Supervisor;
use buzz_hive_core::supervisor_wire::Signer;

/// The `NAME=value` lines `buzz-hive keys` prints for the supervisor's environment file: the
/// key the engine signs calls with, derived from `token_secret`, and the Beehive account's
/// public key, from `account_key` as an nsec or 64 hex characters. Only public keys are in the
/// lines.
pub fn key_lines(token_secret: &str, account_key: &str) -> Result<[String; 2], String> {
    let signer = Signer::from_secret(token_secret.as_bytes())?;
    let account = nostr::Keys::parse(account_key)
        .map_err(|_| "BUZZ_HIVE_NOSTR_KEY is not an nsec or 64 hex characters".to_owned())?;
    Ok([
        format!("BUZZ_HIVE_SUPERVISOR_PUBKEY={}", signer.public_hex()),
        format!(
            "BUZZ_HIVE_SUPERVISOR_HIVE_PUBKEY={}",
            account.public_key().to_hex()
        ),
    ])
}

/// What `buzz-hive apply-agents` reports for every agent, and whether it was a success.
pub struct Applied {
    /// One line per agent, in order.
    pub lines: Vec<String>,
    /// How many agents were not applied.
    pub failed: usize,
}

impl Applied {
    /// `Ok` when every agent was applied, otherwise the count that were not.
    pub fn result(&self, total: usize) -> Result<(), String> {
        match self.failed {
            0 => Ok(()),
            n => Err(format!("{n} of {total} agents were not applied")),
        }
    }
}

/// Sends each agent's tool servers to the supervisor, one after the other. An agent that fails
/// does not stop the ones after it.
pub async fn apply_all(
    store: &dyn Store,
    supervisor: &dyn Supervisor,
    community: CommunityId,
    agents: &[String],
) -> Applied {
    let mut applied = Applied {
        lines: Vec::new(),
        failed: 0,
    };
    for agent in agents {
        match agent_tools::apply(store, Some(supervisor), community, agent).await {
            Ok(restarted) => applied
                .lines
                .push(format!("{agent}: applied, restarted: {restarted}")),
            Err(error) => {
                applied.failed += 1;
                applied.lines.push(format!("{agent}: not applied: {error}"));
            }
        }
    }
    applied
}
