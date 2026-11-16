//! The first-start seed: agent folders become known agents with their servers, and a second
//! start changes nothing.

use buzz_hive::agent_seed::seed_agents;
use buzz_hive_core::ports::store::{CommunityId, Store};
use buzz_hive_core::testing::MemStore;

const COMMUNITY: CommunityId = CommunityId::from_u128(1);
const MCP: &str = r#"{"mcpServers":{"filesystem":{"command":"npx","args":["-y","fs"]}}}"#;

fn folders() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("coder")).unwrap();
    std::fs::write(dir.path().join("coder/mcp.json"), MCP).unwrap();
    // A folder with no file is a known agent with no servers.
    std::fs::create_dir_all(dir.path().join("researcher")).unwrap();
    // A file that is not an agent, a name no agent can have and a broken file seed nothing.
    std::fs::write(dir.path().join("README.md"), "docs").unwrap();
    std::fs::create_dir_all(dir.path().join("Bad Name")).unwrap();
    std::fs::create_dir_all(dir.path().join("broken")).unwrap();
    std::fs::write(dir.path().join("broken/mcp.json"), "{not json").unwrap();
    dir
}

#[tokio::test]
async fn folders_seed_agents_once_and_a_removed_server_stays_removed() {
    let dir = folders();
    let store = MemStore::new();
    assert_eq!(seed_agents(&store, COMMUNITY, dir.path()).await.unwrap(), 2);

    let mut tx = store.begin(COMMUNITY).await.unwrap();
    assert_eq!(
        tx.agent_tools().agents().await.unwrap(),
        ["coder", "researcher"]
    );
    assert_eq!(tx.agent_tools().of_agent("coder").await.unwrap().len(), 1);
    assert!(tx
        .agent_tools()
        .remove("coder", "filesystem")
        .await
        .unwrap());
    tx.commit().await.unwrap();

    // The next start reads the same folders and changes nothing.
    assert_eq!(seed_agents(&store, COMMUNITY, dir.path()).await.unwrap(), 0);
    let mut tx = store.begin(COMMUNITY).await.unwrap();
    assert!(tx.agent_tools().of_agent("coder").await.unwrap().is_empty());
    assert_eq!(tx.agent_tools().agents().await.unwrap().len(), 2);
}

#[tokio::test]
async fn a_folder_that_cannot_be_read_stops_the_start() {
    let store = MemStore::new();
    let error = seed_agents(
        &store,
        COMMUNITY,
        std::path::Path::new("/nonexistent/agents"),
    )
    .await
    .unwrap_err();
    assert!(error.contains("/nonexistent/agents"), "{error}");
}
