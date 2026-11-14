//! `PgStore` keeps access levels and settings per community and rolls back on drop.

mod support;

use buzz_hive_core::access::{ActorKind, Level};
use buzz_hive_core::ports::store::{CommunityId, Store};
use buzz_hive_store::pg::PgStore;

async fn store(prefix: &str) -> (PgStore, support::Scratch, sqlx::PgPool) {
    let (relay_pool, guard) = support::scratch_relay_db(prefix).await;
    let pool = buzz_hive_store::migrate::connect(&support::url_for(&guard))
        .await
        .unwrap();
    buzz_hive_store::migrate::migrate(&pool).await.unwrap();
    (PgStore::new(pool), guard, relay_pool)
}

const A: &str = "aa";

#[tokio::test]
#[ignore = "requires Postgres"]
async fn access_levels_round_trip_and_stay_in_their_community() {
    let (store, _guard, _relay) = store("hive_store").await;
    let one = CommunityId::from_u128(1);
    let two = CommunityId::from_u128(2);

    let mut tx = store.begin(one).await.unwrap();
    assert_eq!(tx.access().level(A).await.unwrap(), None);
    tx.access()
        .set_level(A, Level::Run, ActorKind::Agent, "admin")
        .await
        .unwrap();
    tx.access()
        .set_level(A, Level::Read, ActorKind::Agent, "admin")
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let mut tx = store.begin(one).await.unwrap();
    assert_eq!(
        tx.access().level(A).await.unwrap(),
        Some((Level::Read, ActorKind::Agent))
    );
    assert_eq!(tx.access().list().await.unwrap().len(), 1);
    drop(tx);

    let mut tx = store.begin(two).await.unwrap();
    assert_eq!(tx.access().level(A).await.unwrap(), None);
    assert!(tx.access().list().await.unwrap().is_empty());
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn a_dropped_transaction_rolls_back() {
    let (store, _guard, _relay) = store("hive_store_rb").await;
    let community = CommunityId::from_u128(1);
    {
        let mut tx = store.begin(community).await.unwrap();
        tx.access()
            .set_level(A, Level::Admin, ActorKind::Person, "admin")
            .await
            .unwrap();
    }
    let mut tx = store.begin(community).await.unwrap();
    assert_eq!(tx.access().level(A).await.unwrap(), None);
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn settings_round_trip_and_update() {
    let (store, _guard, _relay) = store("hive_store_set").await;
    let community = CommunityId::from_u128(1);
    let mut tx = store.begin(community).await.unwrap();
    assert_eq!(tx.settings().get("engine", "", "k").await.unwrap(), None);
    tx.settings()
        .set("engine", "", "k", serde_json::json!({"a": 1}))
        .await
        .unwrap();
    tx.settings()
        .set("engine", "", "k", serde_json::json!({"a": 2}))
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let mut tx = store.begin(community).await.unwrap();
    assert_eq!(
        tx.settings().get("engine", "", "k").await.unwrap(),
        Some(serde_json::json!({"a": 2}))
    );
}
