//! First-start seed of the agents' tool servers from the folders under `deploy/aws/agents/`.
//!
//! Each folder is an agent. Its `mcp.json`, when it has one, becomes the agent's servers the
//! first time the engine sees the folder. After that the rows are the truth, so a server a
//! person removed does not come back at the next start, and a second start changes nothing.

use std::path::Path;

use buzz_hive_core::domain::tool_server::check_agent;
use buzz_hive_core::orchestrator::agent_tools;
use buzz_hive_core::ports::store::{CommunityId, Store};

/// Reads every agent folder under `dir` and seeds the new ones. Returns how many it seeded.
/// A folder whose name is not an agent name, or whose `mcp.json` the checks refuse, is
/// logged and left out, so one bad file never stops the engine from starting.
pub async fn seed_agents(
    store: &dyn Store,
    community: CommunityId,
    dir: &Path,
) -> Result<usize, String> {
    let mut folders = Vec::new();
    for entry in
        std::fs::read_dir(dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?
    {
        let entry = entry.map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
        if entry.path().is_dir() {
            folders.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    folders.sort();
    let mut seeded = 0;
    for agent in folders {
        if let Err(reason) = check_agent(&agent) {
            tracing::warn!(%agent, %reason, "an agent folder has a name no agent can have");
            continue;
        }
        let file = dir.join(&agent).join("mcp.json");
        let text = match std::fs::read_to_string(&file) {
            Ok(text) => Some(text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => {
                tracing::warn!(%agent, error = %e, "cannot read the agent's mcp.json");
                continue;
            }
        };
        match agent_tools::seed(store, community, &agent, text.as_deref()).await {
            Ok(true) => seeded += 1,
            Ok(false) => {}
            Err(error) => tracing::warn!(%agent, %error, "the agent's mcp.json was not seeded"),
        }
    }
    Ok(seeded)
}
