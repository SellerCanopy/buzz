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

#[tokio::test]
#[ignore = "requires Postgres"]
async fn locking_an_agent_makes_another_transaction_for_it_wait_until_the_first_ends() {
    let (store, _guard, relay) = store().await;
    let store = std::sync::Arc::new(store);
    let community = CommunityId::from_u128(1);
    let mut tx = store.begin(community).await.unwrap();
    tx.agent_tools().seed("coder", &[]).await.unwrap();
    tx.commit().await.unwrap();

    let mut first = store.begin(community).await.unwrap();
    assert!(first.agent_tools().lock_agent("coder").await.unwrap());
    let other = store.clone();
    let second = tokio::spawn(async move {
        let mut tx = other.begin(community).await.unwrap();
        let known = tx.agent_tools().lock_agent("coder").await.unwrap();
        // An agent nobody seeded is not known, and is not locked.
        let stranger = tx.agent_tools().lock_agent("stranger").await.unwrap();
        (known, stranger)
    });
    // The second transaction is waiting on the row lock: Postgres says so.
    let mut waiting = false;
    for _ in 0..250 {
        let n: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_stat_activity WHERE wait_event_type = 'Lock' \
             AND query LIKE '%agent_tool_agents%FOR UPDATE%'",
        )
        .fetch_one(&relay)
        .await
        .unwrap();
        if n == 1 {
            waiting = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(waiting, "the second lock did not wait for the first");
    assert!(!second.is_finished());
    first.commit().await.unwrap();
    assert_eq!(second.await.unwrap(), (true, false));
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn simultaneous_sets_for_an_agent_one_short_of_the_cap_never_pass_it() {
    use buzz_hive_core::access::ActorKind;
    use buzz_hive_core::domain::events::Actor;
    use buzz_hive_core::domain::tool_server::MAX_SERVERS_PER_AGENT;
    use buzz_hive_core::orchestrator::agent_tools::set;
    use buzz_hive_core::orchestrator::Ctx;

    let (store, _guard, _relay) = store().await;
    let store = std::sync::Arc::new(store);
    let community = CommunityId::from_u128(1);
    let held: Vec<ToolServer> = (0..MAX_SERVERS_PER_AGENT - 1)
        .map(|i| stdio(&format!("held-{i}"), "x"))
        .collect();
    let mut tx = store.begin(community).await.unwrap();
    tx.agent_tools().seed("coder", &held).await.unwrap();
    tx.commit().await.unwrap();

    let ctx = Ctx {
        community,
        actor: Actor {
            pubkey: "aa".into(),
            kind: ActorKind::Person,
        },
        requester: None,
    };
    let mut sets = Vec::new();
    for i in 0..8 {
        let (store, ctx) = (store.clone(), ctx.clone());
        sets.push(tokio::spawn(async move {
            set(
                &*store,
                None,
                &ctx,
                "coder",
                stdio(&format!("new-{i}"), "x"),
            )
            .await
        }));
    }
    let mut saved = 0;
    for done in sets {
        if done.await.unwrap().is_ok() {
            saved += 1;
        }
    }
    let mut tx = store.begin(community).await.unwrap();
    let have = tx.agent_tools().of_agent("coder").await.unwrap().len();
    assert_eq!(saved, 1);
    assert_eq!(have, MAX_SERVERS_PER_AGENT);
}
