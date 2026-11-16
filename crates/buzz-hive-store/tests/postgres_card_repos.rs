//! The repos added to a card, a run's events, and the age of the oldest unsent post, in Postgres.

mod support;

use buzz_hive_core::access::ActorKind;
use buzz_hive_core::domain::card::{Card, CardState};
use buzz_hive_core::domain::events::{Actor, Event};
use buzz_hive_core::ports::store::{CommunityId, Posting, Store, StoreError};
use buzz_hive_store::pg::PgStore;
use uuid::Uuid;

async fn store() -> (PgStore, support::Scratch, sqlx::PgPool) {
    let (relay_pool, guard) = support::scratch_relay_db("hive_card_repos").await;
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
        workflow: "demo".into(),
        archived: false,
        created_by: "aa".into(),
    }
}

fn event(kind: &str, card: Uuid, run: Option<Uuid>) -> Event {
    Event::new(
        kind,
        Some(card),
        run,
        Actor {
            pubkey: "aa".into(),
            kind: ActorKind::Person,
        },
        serde_json::json!({}),
    )
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn a_card_keeps_its_repos_in_the_order_added_and_loses_them_with_the_card() {
    let (store, _guard, _relay) = store().await;
    let community = CommunityId::from_u128(1);
    let mut tx = store.begin(community).await.unwrap();
    tx.cards().insert(&card(1)).await.unwrap();
    tx.cards().insert(&card(2)).await.unwrap();
    let one = Uuid::from_u128(1);
    for repo in ["example-org/web", "example-org/api", "example-org/docs"] {
        tx.card_repos().add(one, repo).await.unwrap();
    }
    tx.commit().await.unwrap();
    // A repo the card has is a conflict, and it ends that transaction.
    let mut tx = store.begin(community).await.unwrap();
    let again = tx.card_repos().add(one, "example-org/api").await;
    assert!(matches!(again, Err(StoreError::Conflict(_))), "{again:?}");
    drop(tx);
    let mut tx = store.begin(community).await.unwrap();
    // Dropping the middle repo and adding it back puts it last, never in a taken place.
    assert!(tx
        .card_repos()
        .remove(one, "example-org/api")
        .await
        .unwrap());
    assert!(!tx
        .card_repos()
        .remove(one, "example-org/api")
        .await
        .unwrap());
    tx.card_repos().add(one, "example-org/api").await.unwrap();
    assert_eq!(
        tx.card_repos().of_card(one).await.unwrap(),
        ["example-org/web", "example-org/docs", "example-org/api"]
    );
    assert!(tx
        .card_repos()
        .of_card(Uuid::from_u128(2))
        .await
        .unwrap()
        .is_empty());
    tx.commit().await.unwrap();

    let mut tx = store.begin(community).await.unwrap();
    tx.cards().delete(one).await.unwrap();
    assert!(tx.card_repos().of_card(one).await.unwrap().is_empty());
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn a_runs_events_are_read_after_a_seq_and_no_other_runs_are_mixed_in() {
    let (store, _guard, _relay) = store().await;
    let community = CommunityId::from_u128(1);
    let mut tx = store.begin(community).await.unwrap();
    tx.cards().insert(&card(1)).await.unwrap();
    let (a, b) = (Uuid::from_u128(10), Uuid::from_u128(11));
    for (run, kind) in [
        (Some(a), "run.started"),
        (Some(b), "run.started"),
        (None, "activity.added"),
        (Some(a), "run.ended"),
    ] {
        tx.events()
            .append(event(kind, Uuid::from_u128(1), run))
            .await
            .unwrap();
    }
    tx.commit().await.unwrap();

    let mut tx = store.begin(community).await.unwrap();
    let card = Uuid::from_u128(1);
    let all = tx.events().of_run(card, a, 0, 10).await.unwrap();
    let kinds: Vec<_> = all.iter().map(|e| e.kind.as_str()).collect();
    assert_eq!(kinds, ["run.started", "run.ended"]);
    let after = tx.events().of_run(card, a, all[0].seq, 10).await.unwrap();
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].kind, "run.ended");
    assert_eq!(tx.events().of_run(card, a, 0, 1).await.unwrap().len(), 1);
    assert!(tx
        .events()
        .of_run(card, Uuid::from_u128(99), 0, 10)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn the_backlog_says_when_the_oldest_unsent_post_was_queued() {
    let (store, _guard, _relay) = store().await;
    let community = CommunityId::from_u128(1);
    let mut tx = store.begin(community).await.unwrap();
    tx.cards().insert(&card(1)).await.unwrap();
    let empty = tx.outbox().backlog().await.unwrap();
    assert_eq!((empty.pending, empty.oldest), (0, None));
    let post = Posting::Status {
        channel: Uuid::from_u128(77),
        text: "hello".into(),
    };
    tx.outbox()
        .enqueue("k1", Uuid::from_u128(1), &post)
        .await
        .unwrap();
    let backlog = tx.outbox().backlog().await.unwrap();
    assert_eq!(backlog.pending, 1);
    assert!(backlog.oldest.is_some());
}
