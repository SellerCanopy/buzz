//! The pull request of a card in Postgres: one row per card, replaced when the card opens
//! another, and gone with the card.

mod support;

use buzz_hive_core::domain::card::{Card, CardState, PullRequest};
use buzz_hive_core::ports::store::{CommunityId, Store};
use buzz_hive_store::pg::PgStore;
use uuid::Uuid;

async fn store() -> (PgStore, support::Scratch, sqlx::PgPool) {
    let (relay_pool, guard) = support::scratch_relay_db("hive_pulls").await;
    let pool = buzz_hive_store::migrate::connect(&support::url_for(&guard))
        .await
        .unwrap();
    buzz_hive_store::migrate::migrate(&pool).await.unwrap();
    (PgStore::new(pool), guard, relay_pool)
}

fn card(id: u128) -> Card {
    Card {
        id: Uuid::from_u128(id),
        title: "card".into(),
        description: String::new(),
        state: CardState::Idle,
        workflow: String::new(),
        archived: false,
        created_by: "aa".into(),
    }
}

fn pull(number: u64, state: &str) -> PullRequest {
    PullRequest {
        repo: "example-org/api".into(),
        number,
        url: format!("https://github.example.org/example-org/api/pull/{number}"),
        head: "buzz/00000005-card".into(),
        base: "main".into(),
        state: state.into(),
        draft: true,
        approved: false,
        checks: String::new(),
    }
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn a_card_keeps_one_pull_request_and_loses_it_with_the_card() {
    let (store, _guard, _relay) = store().await;
    let community = CommunityId::from_u128(1);
    let mut tx = store.begin(community).await.unwrap();
    tx.cards().insert(&card(5)).await.unwrap();
    let id = Uuid::from_u128(5);
    assert!(tx.pull_requests().by_card(id).await.unwrap().is_none());

    tx.pull_requests().put(id, &pull(12, "open")).await.unwrap();
    assert_eq!(
        tx.pull_requests().by_card(id).await.unwrap(),
        Some(pull(12, "open"))
    );

    // Putting it again, as a repeated call does, replaces the row instead of adding one.
    tx.pull_requests().put(id, &pull(12, "open")).await.unwrap();
    tx.pull_requests().put(id, &pull(13, "open")).await.unwrap();
    assert_eq!(
        tx.pull_requests()
            .by_card(id)
            .await
            .unwrap()
            .unwrap()
            .number,
        13
    );

    tx.cards().delete(id).await.unwrap();
    assert!(tx.pull_requests().by_card(id).await.unwrap().is_none());
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn another_communitys_card_has_its_own_pull_request() {
    let (store, _guard, _relay) = store().await;
    let id = Uuid::from_u128(5);
    for community in [CommunityId::from_u128(1), CommunityId::from_u128(2)] {
        let mut tx = store.begin(community).await.unwrap();
        tx.cards().insert(&card(5)).await.unwrap();
        tx.pull_requests()
            .put(id, &pull(community.as_u128() as u64, "open"))
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }
    let mut tx = store.begin(CommunityId::from_u128(2)).await.unwrap();
    assert_eq!(
        tx.pull_requests()
            .by_card(id)
            .await
            .unwrap()
            .unwrap()
            .number,
        2
    );
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn what_a_sync_read_is_kept_and_only_open_pull_requests_are_watched() {
    let (store, _guard, _relay) = store().await;
    let mut tx = store.begin(CommunityId::from_u128(1)).await.unwrap();
    for (card_id, number, state) in [(5, 12, "open"), (6, 13, "merged"), (7, 14, "closed")] {
        tx.cards().insert(&card(card_id)).await.unwrap();
        tx.pull_requests()
            .put(Uuid::from_u128(card_id), &pull(number, state))
            .await
            .unwrap();
    }
    let mut read = pull(12, "open");
    read.draft = false;
    read.approved = true;
    read.checks = "passing".into();
    tx.pull_requests()
        .put(Uuid::from_u128(5), &read)
        .await
        .unwrap();
    assert_eq!(
        tx.pull_requests()
            .by_card(Uuid::from_u128(5))
            .await
            .unwrap(),
        Some(read.clone())
    );
    let open = tx.pull_requests().open().await.unwrap();
    assert_eq!(open, vec![(Uuid::from_u128(5), read)]);
}
