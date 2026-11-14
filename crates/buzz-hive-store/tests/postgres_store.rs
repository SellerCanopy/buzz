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

#[tokio::test]
#[ignore = "requires Postgres"]
async fn cards_list_newest_first_by_board_or_archive_and_stay_in_their_community() {
    let (store, guard, _relay) = store("hive_store_cards").await;
    let admin = sqlx::PgPool::connect(guard.url()).await.unwrap();
    let one = CommunityId::from_u128(1);
    let two = CommunityId::from_u128(2);
    let ids: Vec<uuid::Uuid> = (1..=4).map(uuid::Uuid::from_u128).collect();
    // (community, id, title, archived, minutes ago)
    for (community, id, title, archived, ago) in [
        (one, ids[0], "oldest", false, 30),
        (one, ids[1], "newest", false, 1),
        (one, ids[2], "archived", true, 10),
        (two, ids[3], "elsewhere", false, 5),
    ] {
        sqlx::query(
            "INSERT INTO hive.cards (community_id, id, title, created_by, created_at, archived_at) \
             VALUES ($1, $2, $3, 'aa', now() - make_interval(mins => $4), \
                     CASE WHEN $5 THEN now() END)",
        )
        .bind(community)
        .bind(id)
        .bind(title)
        .bind(ago)
        .bind(archived)
        .execute(&admin)
        .await
        .unwrap();
    }

    let mut tx = store.begin(one).await.unwrap();
    let board: Vec<String> = tx
        .cards()
        .list(false)
        .await
        .unwrap()
        .into_iter()
        .map(|c| c.title)
        .collect();
    assert_eq!(board, ["newest", "oldest"]);
    let archive = tx.cards().list(true).await.unwrap();
    assert_eq!(archive.len(), 1);
    assert!(archive[0].archived);
    assert_eq!(
        archive[0].state,
        buzz_hive_core::domain::card::CardState::Idle
    );

    assert_eq!(
        tx.cards().get(ids[1]).await.unwrap().unwrap().title,
        "newest"
    );
    assert_eq!(
        tx.cards().get(ids[3]).await.unwrap(),
        None,
        "another community's card"
    );
}
