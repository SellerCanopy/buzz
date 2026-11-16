//! An agent's tool servers, in Postgres: seeded once, set, replaced and removed.

mod support;

use buzz_hive_core::domain::tool_server::{ToolServer, Transport};
use buzz_hive_core::ports::store::{CommunityId, Store};
use buzz_hive_store::pg::PgStore;
use serde_json::json;

async fn store() -> (PgStore, support::Scratch, sqlx::PgPool) {
    let (relay_pool, guard) = support::scratch_relay_db("hive_agent_tools").await;
    let pool = buzz_hive_store::migrate::connect(&support::url_for(&guard))
        .await
        .unwrap();
    buzz_hive_store::migrate::migrate(&pool).await.unwrap();
    (PgStore::new(pool), guard, relay_pool)
}

fn stdio(name: &str, command: &str) -> ToolServer {
    ToolServer::new(name, Transport::Stdio, &json!({"command": command})).unwrap()
}

fn docs() -> ToolServer {
    ToolServer::new(
        "docs",
        Transport::Http,
        &json!({"url": "https://mcp.example.org/mcp", "headers": {"Authorization": "Bearer ${DOCS_TOKEN}"}}),
    )
    .unwrap()
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn a_second_seed_changes_nothing_and_a_removed_server_stays_removed() {
    let (store, _guard, _relay) = store().await;
    let community = CommunityId::from_u128(1);
    let servers = [stdio("filesystem", "npx"), docs()];

    let mut tx = store.begin(community).await.unwrap();
    assert!(tx.agent_tools().seed("coder", &servers).await.unwrap());
    // A folder with no file is a known agent with no servers.
    assert!(tx.agent_tools().seed("researcher", &[]).await.unwrap());
    tx.commit().await.unwrap();

    let mut tx = store.begin(community).await.unwrap();
    assert!(tx
        .agent_tools()
        .remove("coder", "filesystem")
        .await
        .unwrap());
    tx.commit().await.unwrap();

    // The next start reads the same folder: nothing comes back and nothing is added.
    let mut tx = store.begin(community).await.unwrap();
    assert!(!tx.agent_tools().seed("coder", &servers).await.unwrap());
    assert!(!tx
        .agent_tools()
        .seed("researcher", &[stdio("x", "y")])
        .await
        .unwrap());
    assert_eq!(
        tx.agent_tools().agents().await.unwrap(),
        ["coder", "researcher"]
    );
    let listed = tx.agent_tools().list().await.unwrap();
    assert_eq!(listed, [("coder".to_owned(), docs())]);
    assert!(tx
        .agent_tools()
        .of_agent("researcher")
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn set_adds_or_replaces_and_remove_says_whether_it_was_there() {
    let (store, _guard, _relay) = store().await;
    let community = CommunityId::from_u128(1);
    let mut tx = store.begin(community).await.unwrap();
    tx.agent_tools().seed("coder", &[]).await.unwrap();
    tx.agent_tools()
        .set("coder", &stdio("b", "one"))
        .await
        .unwrap();
    tx.agent_tools().set("coder", &docs()).await.unwrap();
    // The same name again replaces it, and the transport can change with it.
    tx.agent_tools()
        .set("coder", &stdio("docs", "local"))
        .await
        .unwrap();
    let mine = tx.agent_tools().of_agent("coder").await.unwrap();
    assert_eq!(
        mine.iter()
            .map(|s| (s.name.as_str(), s.transport))
            .collect::<Vec<_>>(),
        [("b", Transport::Stdio), ("docs", Transport::Stdio)]
    );
    assert_eq!(mine[1], stdio("docs", "local"));
    assert!(tx.agent_tools().remove("coder", "b").await.unwrap());
    assert!(!tx.agent_tools().remove("coder", "b").await.unwrap());
    tx.commit().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn each_community_has_its_own_agents_and_servers() {
    let (store, _guard, _relay) = store().await;
    let (one, two) = (CommunityId::from_u128(1), CommunityId::from_u128(2));
    let mut tx = store.begin(one).await.unwrap();
    tx.agent_tools()
        .seed("coder", &[stdio("a", "x")])
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let mut tx = store.begin(two).await.unwrap();
    assert!(tx.agent_tools().agents().await.unwrap().is_empty());
    assert!(tx.agent_tools().list().await.unwrap().is_empty());
    assert!(tx.agent_tools().seed("coder", &[]).await.unwrap());
    assert!(tx.agent_tools().of_agent("coder").await.unwrap().is_empty());
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn the_table_refuses_a_transport_it_does_not_know() {
    let (store, _guard, relay) = store().await;
    let mut tx = store.begin(CommunityId::from_u128(1)).await.unwrap();
    tx.agent_tools().seed("coder", &[]).await.unwrap();
    tx.commit().await.unwrap();
    let bad = sqlx::query(
        "INSERT INTO hive.agent_tool_servers (community_id, agent, name, transport, spec) \
         VALUES ($1, 'coder', 'x', 'websocket', '{}')",
    )
    .bind(CommunityId::from_u128(1))
    .execute(&relay)
    .await;
    assert!(bad.is_err());
}
