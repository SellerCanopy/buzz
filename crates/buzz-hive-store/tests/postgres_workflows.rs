//! Workflow versions are stored once by hash, the current pointer moves, and options stay
//! with their community.

mod support;

use buzz_hive_core::domain::card::{Card, CardState};
use buzz_hive_core::domain::run::{Run, RunState};
use buzz_hive_core::ports::store::{CommunityId, Store, StoreError};
use buzz_hive_core::workflow::options::WorkflowOptions;
use buzz_hive_store::pg::PgStore;
use serde_json::json;
use uuid::Uuid;

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

#[tokio::test]
#[ignore = "requires Postgres"]
async fn a_save_moves_the_pointer_only_from_the_hash_it_was_made_against() {
    let (store, _guard, _relay) = store("hive_workflows_save").await;
    let community = CommunityId::from_u128(1);
    let mut tx = store.begin(community).await.unwrap();
    // A new package starts with no base. Taking the name again is a conflict.
    tx.workflows()
        .save_version("mine", None, "h1", json!({"a": "1"}), "aa")
        .await
        .unwrap();
    let taken = tx
        .workflows()
        .save_version("mine", None, "h2", json!({"a": "2"}), "aa")
        .await;
    assert!(matches!(taken, Err(StoreError::Conflict(_))), "{taken:?}");
    tx.workflows()
        .save_version("mine", Some("h1"), "h2", json!({"a": "2"}), "aa")
        .await
        .unwrap();
    // A save made against h1 is stale now that h2 is current.
    let stale = tx
        .workflows()
        .save_version("mine", Some("h1"), "h3", json!({"a": "3"}), "bb")
        .await;
    assert!(matches!(stale, Err(StoreError::Conflict(_))), "{stale:?}");
    tx.commit().await.unwrap();

    let mut tx = store.begin(community).await.unwrap();
    let packages = tx.workflows().packages().await.unwrap();
    assert_eq!(packages[0].current_hash, "h2");
    assert!(!packages[0].builtin);
    // Both saved versions stay readable.
    assert!(tx
        .workflows()
        .version("mine", "h1")
        .await
        .unwrap()
        .is_some());
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn a_built_in_is_not_saved_over_or_deleted_through_a_save() {
    let (store, _guard, _relay) = store("hive_workflows_builtin").await;
    let mut tx = store.begin(CommunityId::from_u128(1)).await.unwrap();
    tx.workflows()
        .record_version("task", "h1", json!({}), "builtin", true)
        .await
        .unwrap();
    let refused = tx
        .workflows()
        .save_version("task", Some("h1"), "h2", json!({"x": 1}), "aa")
        .await;
    assert!(
        matches!(refused, Err(StoreError::Conflict(_))),
        "{refused:?}"
    );
    assert_eq!(
        tx.workflows().packages().await.unwrap()[0].current_hash,
        "h1"
    );
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn delete_removes_the_package_and_options_but_keeps_versions() {
    let (store, _guard, _relay) = store("hive_workflows_delete").await;
    let community = CommunityId::from_u128(1);
    let mut tx = store.begin(community).await.unwrap();
    tx.workflows()
        .save_version("mine", None, "h1", json!({"a": "1"}), "aa")
        .await
        .unwrap();
    tx.workflows()
        .set_options("mine", &WorkflowOptions::default())
        .await
        .unwrap();
    tx.workflows().delete_package("mine").await.unwrap();
    assert!(tx.workflows().packages().await.unwrap().is_empty());
    assert!(tx.workflows().options().await.unwrap().is_empty());
    assert!(tx
        .workflows()
        .version("mine", "h1")
        .await
        .unwrap()
        .is_some());
    let missing = tx.workflows().delete_package("mine").await;
    assert!(
        matches!(missing, Err(StoreError::NotFound(_))),
        "{missing:?}"
    );
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn kept_versions_are_the_current_saved_ones_and_those_an_active_run_uses() {
    let (store, _guard, _relay) = store("hive_workflows_kept").await;
    let community = CommunityId::from_u128(1);
    let mut tx = store.begin(community).await.unwrap();
    tx.workflows()
        .record_version("task", "b1", json!({}), "builtin", true)
        .await
        .unwrap();
    for (base, hash) in [(None, "h1"), (Some("h1"), "h2"), (Some("h2"), "h3")] {
        tx.workflows()
            .save_version("mine", base, hash, json!({"v": hash}), "aa")
            .await
            .unwrap();
    }
    // A run on h1 keeps it, one that ended on h2 does not.
    for (n, hash, ended) in [(1u128, "h1", false), (2, "h2", true)] {
        let mut card = Card {
            id: Uuid::from_u128(n),
            title: String::new(),
            description: String::new(),
            state: CardState::Idle,
            workflow: "mine".into(),
            archived: false,
            created_by: "aa".into(),
        };
        card.title = format!("card {n}");
        tx.cards().insert(&card).await.unwrap();
        let mut run = Run::new(Uuid::from_u128(n), "mine", hash);
        if ended {
            run.state = RunState::Ended;
        }
        tx.runs().insert(&run).await.unwrap();
    }
    let kept = tx.workflows().kept_versions().await.unwrap();
    let seen: Vec<(&str, &str, bool)> = kept
        .iter()
        .map(|k| (k.name.as_str(), k.hash.as_str(), k.current))
        .collect();
    assert_eq!(seen, [("mine", "h1", false), ("mine", "h3", true)]);
    assert!(kept.iter().all(|k| !k.builtin));
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn proposals_are_kept_per_community_and_listed_oldest_first() {
    let (store, _guard, _relay) = store("hive_proposals").await;
    let one = CommunityId::from_u128(1);
    let mut tx = store.begin(one).await.unwrap();
    let first = tx
        .proposals()
        .insert("workflow", json!({"name": "a"}), "aa")
        .await
        .unwrap();
    let second = tx
        .proposals()
        .insert("workflow", json!({"name": "b"}), "bb")
        .await
        .unwrap();
    tx.proposals()
        .insert("ticket", json!({"title": "t"}), "aa")
        .await
        .unwrap();
    assert!(second > first);
    tx.commit().await.unwrap();

    let mut tx = store.begin(one).await.unwrap();
    let open = tx.proposals().open("workflow").await.unwrap();
    let names: Vec<_> = open.iter().map(|p| p.body["name"].clone()).collect();
    assert_eq!(names, [json!("a"), json!("b")]);
    assert_eq!(open[0].requester, "aa");
    assert_eq!(open[0].kind, "workflow");
    drop(tx);

    let mut tx = store.begin(CommunityId::from_u128(2)).await.unwrap();
    assert!(tx.proposals().open("workflow").await.unwrap().is_empty());
}
