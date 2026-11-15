//! The outbox drain against the in-memory store. Postgres runs the same scenarios in
//! `buzz-hive-store/tests/postgres_runs.rs`.

use std::time::Duration;

use buzz_hive_core::domain::card::{Card, CardState};
use buzz_hive_core::ports::store::{CommunityId, Posting, Store};
use buzz_hive_core::testing::{outbox_scenarios, MemStore};
use uuid::Uuid;

const LEASE: Duration = Duration::from_secs(60);

fn community() -> CommunityId {
    CommunityId::from_u128(1)
}

#[tokio::test]
async fn one_cards_failure_does_not_block_another() {
    outbox_scenarios::one_cards_failure_does_not_block_another(&MemStore::new(), community()).await;
}

#[tokio::test]
async fn a_stopped_drain_sends_nothing() {
    outbox_scenarios::a_stopped_drain_sends_nothing(&MemStore::new(), community()).await;
}

fn posting(text: &str) -> Posting {
    Posting::Status {
        channel: Uuid::from_u128(77),
        text: text.into(),
    }
}

async fn queue(store: &MemStore, rows: &[(&str, u128)]) {
    let mut tx = store.begin(community()).await.unwrap();
    for (key, card) in rows {
        let id = Uuid::from_u128(*card);
        if tx.cards().get(id).await.unwrap().is_none() {
            tx.cards()
                .insert(&Card {
                    id,
                    title: "card".into(),
                    description: String::new(),
                    state: CardState::Idle,
                    workflow: String::new(),
                    archived: false,
                    created_by: "aa".into(),
                })
                .await
                .unwrap();
        }
        tx.outbox().enqueue(key, id, &posting(key)).await.unwrap();
    }
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn a_claimed_post_holds_back_its_cards_later_posts_but_not_other_cards() {
    let store = MemStore::new();
    queue(&store, &[("a1", 1), ("a2", 1), ("b1", 2)]).await;

    let mut tx = store.begin(community()).await.unwrap();
    assert_eq!(
        tx.outbox()
            .claim_next(&[], LEASE)
            .await
            .unwrap()
            .unwrap()
            .key,
        "a1"
    );
    tx.commit().await.unwrap();

    // a1 is in flight. a2 must wait behind it, and card two is free to go.
    let mut tx = store.begin(community()).await.unwrap();
    assert_eq!(
        tx.outbox()
            .claim_next(&[], LEASE)
            .await
            .unwrap()
            .unwrap()
            .key,
        "b1"
    );
    tx.commit().await.unwrap();
    let mut tx = store.begin(community()).await.unwrap();
    assert!(tx.outbox().claim_next(&[], LEASE).await.unwrap().is_none());
}

#[tokio::test]
async fn a_claim_that_ran_out_is_offered_again_and_a_failure_ends_the_claim() {
    let store = MemStore::new();
    queue(&store, &[("a1", 1)]).await;

    let mut tx = store.begin(community()).await.unwrap();
    let row = tx.outbox().claim_next(&[], LEASE).await.unwrap().unwrap();
    tx.commit().await.unwrap();

    let mut tx = store.begin(community()).await.unwrap();
    assert!(tx.outbox().claim_next(&[], LEASE).await.unwrap().is_none());
    // With no lease left the same post comes back.
    assert_eq!(
        tx.outbox()
            .claim_next(&[], Duration::ZERO)
            .await
            .unwrap()
            .unwrap()
            .key,
        "a1"
    );
    tx.outbox().mark_failed(row.id, "down").await.unwrap();
    // A failed post is free to claim at once, and it keeps its attempt count.
    let again = tx.outbox().claim_next(&[], LEASE).await.unwrap().unwrap();
    assert_eq!((again.key.as_str(), again.attempts), ("a1", 1));
}
