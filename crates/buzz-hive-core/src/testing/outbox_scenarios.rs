//! Outbox scenarios that every store must pass, so the in-memory store and Postgres are held
//! to one behavior. A store's own test file calls these with its store.

use uuid::Uuid;

use super::{FakeDirectory, FakeRelay};
use crate::domain::card::{Card, CardState};
use crate::orchestrator::outbox::drain_outbox;
use crate::orchestrator::Env;
use crate::ports::store::{CommunityId, Posting, Store};
use crate::workflow::load::Catalog;

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

fn status(text: &str) -> Posting {
    Posting::Status {
        channel: Uuid::from_u128(77),
        text: text.into(),
    }
}

/// Two cards with two posts each. The relay fails the first post of card A.
///
/// A failed card must not hold up another card, and its own later post must not go ahead of
/// the one that failed. A pass that skips the card leaves it alone, and the next pass sends
/// its posts in order.
pub async fn one_cards_failure_does_not_block_another(store: &dyn Store, community: CommunityId) {
    let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
    let mut tx = store.begin(community).await.unwrap();
    tx.cards().insert(&card(1)).await.unwrap();
    tx.cards().insert(&card(2)).await.unwrap();
    for (key, owner) in [("a1", a), ("a2", a), ("b1", b), ("b2", b)] {
        assert!(tx.outbox().enqueue(key, owner, &status(key)).await.unwrap());
    }
    tx.commit().await.unwrap();

    let directory = FakeDirectory::new();
    let catalog = Catalog::default();
    let env = Env {
        store,
        directory: &directory,
        catalog: &catalog,
        hive: "ee",
    };
    let (_stop, running) = tokio::sync::watch::channel(false);

    // Pass one: a1 is the first call and fails. b1 and b2 still go out. a2 is never tried.
    let relay = FakeRelay::new().failing_first(1);
    let first = drain_outbox(&env, community, &relay, &[], &running)
        .await
        .unwrap();
    assert_eq!((first.posted, first.failed), (2, 1));
    assert_eq!(first.failed_cards, vec![a]);
    assert_eq!(relay.calls(), ["a1", "b1", "b2"]);
    assert_eq!(
        relay
            .posts()
            .iter()
            .map(|p| p.key.as_str())
            .collect::<Vec<_>>(),
        ["b1", "b2"]
    );

    // A pass told to skip card A leaves it alone.
    let skipped = drain_outbox(&env, community, &relay, &[a], &running)
        .await
        .unwrap();
    assert_eq!((skipped.posted, skipped.failed), (0, 0));
    assert_eq!(relay.calls(), ["a1", "b1", "b2"]);

    // Pass two with a healthy relay sends a1 then a2, in order.
    let second = drain_outbox(&env, community, &relay, &[], &running)
        .await
        .unwrap();
    assert_eq!((second.posted, second.failed), (2, 0));
    assert_eq!(relay.calls(), ["a1", "b1", "b2", "a1", "a2"]);
    assert_eq!(
        relay
            .posts()
            .iter()
            .map(|p| p.key.as_str())
            .collect::<Vec<_>>(),
        ["b1", "b2", "a1", "a2"]
    );

    // The failed attempt is on the row, and nothing is left to send.
    let mut tx = store.begin(community).await.unwrap();
    let rows = tx.outbox().all().await.unwrap();
    assert!(rows.iter().all(|row| row.done_at.is_some()));
    assert_eq!(rows[0].attempts, 1);
    assert!(rows[0].last_error.is_some());
    let last = drain_outbox(&env, community, &relay, &[], &running)
        .await
        .unwrap();
    assert_eq!((last.posted, last.failed), (0, 0));
}

/// A drainer that sees the stop signal sends nothing more.
pub async fn a_stopped_drain_sends_nothing(store: &dyn Store, community: CommunityId) {
    let mut tx = store.begin(community).await.unwrap();
    tx.cards().insert(&card(1)).await.unwrap();
    tx.outbox()
        .enqueue("k1", Uuid::from_u128(1), &status("k1"))
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let directory = FakeDirectory::new();
    let catalog = Catalog::default();
    let env = Env {
        store,
        directory: &directory,
        catalog: &catalog,
        hive: "ee",
    };
    let relay = FakeRelay::new();
    let (stop, stopped) = tokio::sync::watch::channel(false);
    stop.send(true).unwrap();
    let done = drain_outbox(&env, community, &relay, &[], &stopped)
        .await
        .unwrap();
    assert_eq!(done.posted, 0);
    assert!(relay.calls().is_empty());
}
