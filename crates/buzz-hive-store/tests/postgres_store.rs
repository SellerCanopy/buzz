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

fn actor(pubkey: &str) -> buzz_hive_core::domain::events::Actor {
    buzz_hive_core::domain::events::Actor {
        pubkey: pubkey.into(),
        kind: ActorKind::Person,
    }
}

fn event(kind: &str, card: Option<uuid::Uuid>) -> buzz_hive_core::domain::events::Event {
    buzz_hive_core::domain::events::Event::new(
        kind,
        card,
        None,
        actor("aa"),
        serde_json::json!({ "n": 1 }),
    )
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn cards_insert_update_archive_restore_and_delete() {
    use buzz_hive_core::domain::card::{Card, CardState};
    let (store, _guard, _relay) = store("hive_store_cardops").await;
    let community = CommunityId::from_u128(1);
    let card = Card {
        id: uuid::Uuid::new_v4(),
        title: "t".into(),
        description: "d".into(),
        state: CardState::Idle,
        workflow: String::new(),
        archived: false,
        created_by: "aa".into(),
    };

    let mut tx = store.begin(community).await.unwrap();
    tx.cards().insert(&card).await.unwrap();
    assert!(
        matches!(
            tx.cards().insert(&card).await,
            Err(buzz_hive_core::ports::store::StoreError::Conflict(_))
        ),
        "a taken id"
    );
    tx.commit().await.unwrap();

    let mut tx = store.begin(community).await.unwrap();
    let mut changed = tx.cards().get_for_update(card.id).await.unwrap().unwrap();
    assert_eq!(changed, card);
    changed.workflow = "rpi".into();
    changed.state = CardState::Running;
    tx.cards().update(&changed).await.unwrap();
    tx.cards().set_archived(card.id, true).await.unwrap();
    tx.commit().await.unwrap();

    let mut tx = store.begin(community).await.unwrap();
    let stored = tx.cards().get(card.id).await.unwrap().unwrap();
    assert_eq!(
        (stored.workflow.as_str(), stored.state, stored.archived),
        ("rpi", CardState::Running, true)
    );
    assert_eq!(stored.description, "d");
    tx.cards().set_archived(card.id, false).await.unwrap();
    tx.cards().delete(card.id).await.unwrap();
    assert!(matches!(
        tx.cards().delete(card.id).await,
        Err(buzz_hive_core::ports::store::StoreError::NotFound(_))
    ));
    assert!(matches!(
        tx.cards().update(&card).await,
        Err(buzz_hive_core::ports::store::StoreError::NotFound(_))
    ));
    tx.commit().await.unwrap();

    let mut tx = store.begin(community).await.unwrap();
    assert_eq!(tx.cards().get(card.id).await.unwrap(), None);
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn events_get_a_seq_on_commit_and_a_rolled_back_append_leaves_none() {
    let (store, _guard, _relay) = store("hive_store_events").await;
    let one = CommunityId::from_u128(1);
    let two = CommunityId::from_u128(2);
    let card = uuid::Uuid::new_v4();

    let mut tx = store.begin(one).await.unwrap();
    tx.events()
        .append(event("card.created", Some(card)))
        .await
        .unwrap();
    tx.events()
        .append(event("card.state", Some(card)))
        .await
        .unwrap();
    let committed = tx.commit().await.unwrap();
    assert_eq!(committed.len(), 2);
    assert!(committed[0].seq > 0 && committed[1].seq == committed[0].seq + 1);

    {
        let mut tx = store.begin(one).await.unwrap();
        tx.events()
            .append(event("card.removed", Some(card)))
            .await
            .unwrap();
    }
    let mut tx = store.begin(two).await.unwrap();
    tx.events()
        .append(event("card.created", None))
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let mut tx = store.begin(one).await.unwrap();
    let all = tx.events().after(0, 10).await.unwrap();
    assert_eq!(
        all.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(),
        ["card.created", "card.state"],
        "no rolled-back event and nothing from another community"
    );
    assert_eq!(
        all[0], committed[0],
        "an event reads back as it was committed apart from `at` precision"
    );
    assert_eq!(
        tx.events().after(committed[0].seq, 10).await.unwrap().len(),
        1
    );
    assert_eq!(tx.events().after(0, 1).await.unwrap().len(), 1);
    assert_eq!(tx.events().latest_seq().await.unwrap(), committed[1].seq);
    assert_eq!(
        store.events_after(one, committed[1].seq, 10).await.unwrap(),
        vec![]
    );
    let mut empty = store.begin(CommunityId::from_u128(3)).await.unwrap();
    assert_eq!(empty.events().latest_seq().await.unwrap(), 0);
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn one_communitys_open_append_does_not_block_another_community() {
    use std::time::Duration;

    let (store, _guard, _relay) = store("hive_store_lock_scope").await;
    let mut holder = store.begin(CommunityId::from_u128(1)).await.unwrap();
    holder
        .events()
        .append(event("card.created", None))
        .await
        .unwrap();
    // The first community's transaction is still open and holds its log lock.
    let mut other = store.begin(CommunityId::from_u128(2)).await.unwrap();
    tokio::time::timeout(
        Duration::from_secs(5),
        other.events().append(event("card.created", None)),
    )
    .await
    .expect("a second community must not wait for the first")
    .unwrap();
    other.commit().await.unwrap();

    // The same community still queues, so its seqs stay in commit order.
    let mut same = store.begin(CommunityId::from_u128(1)).await.unwrap();
    let blocked = tokio::time::timeout(
        Duration::from_millis(300),
        same.events().append(event("card.created", None)),
    )
    .await;
    assert!(blocked.is_err(), "the same community must wait its turn");
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn an_unlisted_event_kind_is_refused() {
    let (store, _guard, _relay) = store("hive_store_kind").await;
    let mut tx = store.begin(CommunityId::from_u128(1)).await.unwrap();
    assert!(tx
        .events()
        .append(event("card.exploded", None))
        .await
        .is_err());
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn parallel_commits_never_show_a_reader_a_gap() {
    use std::sync::Arc;
    use std::time::Duration;

    let (store, _guard, _relay) = store("hive_store_gap").await;
    let store = Arc::new(store);
    let community = CommunityId::from_u128(1);
    const WRITERS: usize = 20;

    let mut writers = Vec::new();
    for _ in 0..WRITERS {
        let store = store.clone();
        writers.push(tokio::spawn(async move {
            let mut tx = store.begin(community).await.unwrap();
            tx.events()
                .append(event("card.created", None))
                .await
                .unwrap();
            // Hold the transaction open so a later writer would commit first if appends did not queue.
            tokio::time::sleep(Duration::from_millis(15)).await;
            tx.commit().await.unwrap();
        }));
    }
    let reader = {
        let store = store.clone();
        tokio::spawn(async move {
            loop {
                let seqs: Vec<i64> = store
                    .events_after(community, 0, 100)
                    .await
                    .unwrap()
                    .iter()
                    .map(|e| e.seq)
                    .collect();
                let first = seqs.first().copied().unwrap_or(1);
                let expected: Vec<i64> = (first..first + seqs.len() as i64).collect();
                assert_eq!(seqs, expected, "visible seqs must be consecutive");
                if seqs.len() == WRITERS {
                    return seqs;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
    };
    for writer in writers {
        writer.await.unwrap();
    }
    let seqs = tokio::time::timeout(Duration::from_secs(10), reader)
        .await
        .expect("the reader sees every event")
        .unwrap();
    assert_eq!(seqs.len(), WRITERS);
}
