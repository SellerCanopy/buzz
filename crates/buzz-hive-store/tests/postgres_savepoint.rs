//! A savepoint on a Postgres transaction undoes what happened after it, events and the log
//! lock included, and the usage counts leave out the Beehive account's own events.

mod support;

use std::time::Duration;

use buzz_hive_core::access::ActorKind;
use buzz_hive_core::domain::card::{Card, CardState};
use buzz_hive_core::domain::events::{Actor, Event};
use buzz_hive_core::ports::store::{CommunityId, Store, StoreError};
use buzz_hive_store::pg::PgStore;
use uuid::Uuid;

async fn store(prefix: &str) -> (PgStore, support::Scratch, sqlx::PgPool) {
    let (relay_pool, guard) = support::scratch_relay_db(prefix).await;
    let pool = buzz_hive_store::migrate::connect(&support::url_for(&guard))
        .await
        .unwrap();
    buzz_hive_store::migrate::migrate(&pool).await.unwrap();
    (PgStore::new(pool), guard, relay_pool)
}

fn card(n: u128) -> Card {
    Card {
        id: Uuid::from_u128(n),
        title: format!("card {n}"),
        description: String::new(),
        state: CardState::Idle,
        workflow: String::new(),
        archived: false,
        created_by: "aa".into(),
    }
}

fn event(kind: &str, pubkey: &str, actor_kind: ActorKind) -> Event {
    Event::new(
        kind,
        None,
        None,
        Actor {
            pubkey: pubkey.into(),
            kind: actor_kind,
        },
        serde_json::json!({}),
    )
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn rolling_back_a_savepoint_undoes_its_writes_and_events_and_keeps_the_rest() {
    let (store, _guard, _relay) = store("hive_savepoint").await;
    let community = CommunityId::from_u128(1);
    let mut tx = store.begin(community).await.unwrap();
    tx.cards().insert(&card(1)).await.unwrap();
    tx.events()
        .append(event("card.created", "aa", ActorKind::Person))
        .await
        .unwrap();

    tx.savepoint().await.unwrap();
    tx.cards().insert(&card(2)).await.unwrap();
    tx.events()
        .append(event("card.state", "aa", ActorKind::Person))
        .await
        .unwrap();
    // A savepoint inside a savepoint is released on its own.
    tx.savepoint().await.unwrap();
    tx.cards().insert(&card(3)).await.unwrap();
    tx.release_savepoint().await.unwrap();
    tx.rollback_savepoint().await.unwrap();

    assert!(
        matches!(tx.rollback_savepoint().await, Err(StoreError::Conflict(_))),
        "no savepoint is open"
    );
    let committed = tx.commit().await.unwrap();
    assert_eq!(
        committed
            .iter()
            .map(|e| e.kind.as_str())
            .collect::<Vec<_>>(),
        ["card.created"],
        "only the event before the savepoint was committed"
    );

    let mut tx = store.begin(community).await.unwrap();
    assert!(tx.cards().get(card(1).id).await.unwrap().is_some());
    assert!(tx.cards().get(card(2).id).await.unwrap().is_none());
    assert!(tx.cards().get(card(3).id).await.unwrap().is_none());
    assert_eq!(tx.events().after(0, 10).await.unwrap().len(), 1);
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn a_released_savepoint_keeps_what_happened_inside_it() {
    let (store, _guard, _relay) = store("hive_savepoint_release").await;
    let community = CommunityId::from_u128(1);
    let mut tx = store.begin(community).await.unwrap();
    tx.savepoint().await.unwrap();
    tx.cards().insert(&card(1)).await.unwrap();
    tx.events()
        .append(event("card.created", "aa", ActorKind::Person))
        .await
        .unwrap();
    tx.release_savepoint().await.unwrap();
    assert_eq!(tx.commit().await.unwrap().len(), 1);

    let mut tx = store.begin(community).await.unwrap();
    assert!(tx.cards().get(card(1).id).await.unwrap().is_some());
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn a_rolled_back_savepoint_gives_back_the_log_lock_it_took() {
    let (store, _guard, _relay) = store("hive_savepoint_lock").await;
    let community = CommunityId::from_u128(1);
    let mut first = store.begin(community).await.unwrap();
    first.savepoint().await.unwrap();
    first
        .events()
        .append(event("card.created", "aa", ActorKind::Person))
        .await
        .unwrap();
    first.rollback_savepoint().await.unwrap();

    // The lock was first taken inside the savepoint and went with it, so another transaction
    // of the community is not made to wait.
    let mut other = store.begin(community).await.unwrap();
    tokio::time::timeout(
        Duration::from_secs(5),
        other
            .events()
            .append(event("card.created", "aa", ActorKind::Person)),
    )
    .await
    .expect("the lock went with the savepoint")
    .unwrap();
    let after_other = other.commit().await.unwrap()[0].seq;

    // The first transaction takes the lock again on its next append, after the other commits,
    // so seqs stay in commit order.
    first
        .events()
        .append(event("card.state", "aa", ActorKind::Person))
        .await
        .unwrap();
    let committed = first.commit().await.unwrap();
    assert_eq!(committed.len(), 1);
    // A rolled-back insert may burn a seq, so the number is not 2, but it follows the other's.
    assert!(committed[0].seq > after_other);
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn usage_leaves_out_what_the_engine_account_did() {
    let (store, _guard, _relay) = store("hive_usage_engine").await;
    let community = CommunityId::from_u128(1);
    let mut tx = store.begin(community).await.unwrap();
    for (kind, pubkey) in [
        ("run.started", "agent1"),
        ("run.started", "hive"),
        ("run.started", "hive"),
        ("card.created", "agent1"),
        ("card.created", "hive"),
    ] {
        tx.events()
            .append(event(kind, pubkey, ActorKind::Agent))
            .await
            .unwrap();
    }
    tx.commit().await.unwrap();

    let mut tx = store.begin(community).await.unwrap();
    let with_engine_out = tx.usage().usage(4, "hive").await.unwrap();
    let this_week = with_engine_out.weeks.last().unwrap();
    assert_eq!(this_week.runs_started_by_agents, 1);
    assert_eq!(this_week.cards_created_by_agents, 1);
    // Naming nobody counts every agent, the engine's account too.
    let everyone = tx.usage().usage(4, "nobody").await.unwrap();
    let this_week = everyone.weeks.last().unwrap();
    assert_eq!(this_week.runs_started_by_agents, 3);
    assert_eq!(this_week.cards_created_by_agents, 2);
}
