//! Workflow versions are stored once by hash, the current pointer moves, and options stay
//! with their community.

mod support;

use buzz_hive_core::ports::store::{CommunityId, Store};
use buzz_hive_core::workflow::options::WorkflowOptions;
use buzz_hive_store::pg::PgStore;
use serde_json::json;

async fn store(prefix: &str) -> (PgStore, support::Scratch, sqlx::PgPool) {
    let (relay_pool, guard) = support::scratch_relay_db(prefix).await;
    let pool = buzz_hive_store::migrate::connect(&support::url_for(&guard))
        .await
        .unwrap();
    buzz_hive_store::migrate::migrate(&pool).await.unwrap();
    (PgStore::new(pool), guard, relay_pool)
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn a_version_is_stored_once_and_the_current_pointer_moves() {
    let (store, _guard, _relay) = store("hive_workflows").await;
    let one = CommunityId::from_u128(1);
    let two = CommunityId::from_u128(2);

    let mut tx = store.begin(one).await.unwrap();
    let files = json!({"workflow.yaml": "version: 2\n"});
    tx.workflows()
        .record_version("demo", "h1", files.clone(), "builtin", true)
        .await
        .unwrap();
    tx.workflows()
        .record_version(
            "demo",
            "h2",
            json!({"workflow.yaml": "version: 2\nstages: []\n"}),
            "builtin",
            true,
        )
        .await
        .unwrap();
    // The same hash again keeps its first body and moves the pointer back.
    tx.workflows()
        .record_version("demo", "h1", json!({"other": "x"}), "someone", true)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let mut tx = store.begin(one).await.unwrap();
    let packages = tx.workflows().packages().await.unwrap();
    assert_eq!(packages.len(), 1);
    assert_eq!(packages[0].name, "demo");
    assert!(packages[0].builtin);
    assert_eq!(packages[0].current_hash, "h1");
    assert_eq!(
        tx.workflows().version("demo", "h1").await.unwrap(),
        Some(files)
    );
    assert!(tx
        .workflows()
        .version("demo", "h2")
        .await
        .unwrap()
        .is_some());
    assert_eq!(tx.workflows().version("demo", "h3").await.unwrap(), None);
    drop(tx);

    let mut tx = store.begin(two).await.unwrap();
    assert!(tx.workflows().packages().await.unwrap().is_empty());
    assert_eq!(tx.workflows().version("demo", "h1").await.unwrap(), None);
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn options_round_trip_per_community() {
    let (store, _guard, _relay) = store("hive_workflow_options").await;
    let one = CommunityId::from_u128(1);
    let two = CommunityId::from_u128(2);
    let options = WorkflowOptions {
        paused_stages: Some(vec!["plan".into()]),
        next_workflow: "task".into(),
        labels: vec!["bug".into()],
    };

    let mut tx = store.begin(one).await.unwrap();
    assert!(tx.workflows().options().await.unwrap().is_empty());
    tx.workflows()
        .set_options("demo", &WorkflowOptions::default())
        .await
        .unwrap();
    tx.workflows().set_options("demo", &options).await.unwrap();
    tx.commit().await.unwrap();

    let mut tx = store.begin(one).await.unwrap();
    let all = tx.workflows().options().await.unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all["demo"], options);
    drop(tx);

    let mut tx = store.begin(two).await.unwrap();
    assert!(tx.workflows().options().await.unwrap().is_empty());
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn a_dropped_transaction_keeps_no_version() {
    let (store, _guard, _relay) = store("hive_workflows_rb").await;
    let community = CommunityId::from_u128(1);
    {
        let mut tx = store.begin(community).await.unwrap();
        tx.workflows()
            .record_version("demo", "h1", json!({}), "builtin", true)
            .await
            .unwrap();
    }
    let mut tx = store.begin(community).await.unwrap();
    assert!(tx.workflows().packages().await.unwrap().is_empty());
}
