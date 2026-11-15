//! Timers in Postgres: they are the database's own time, one per run, stage and kind, and a
//! timer is taken by one caller only.

mod support;

use std::time::Duration;

use buzz_hive_core::domain::card::{Card, CardState};
use buzz_hive_core::domain::run::Run;
use buzz_hive_core::domain::timer::{Timer, TimerKind};
use buzz_hive_core::ports::store::{CommunityId, Store};
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
        workflow: "demo".into(),
        archived: false,
        created_by: "aa".into(),
    }
}

/// A card with a run, and the run's id.
async fn run_of(store: &PgStore, community: CommunityId, n: u128) -> Uuid {
    let mut tx = store.begin(community).await.unwrap();
    tx.cards().insert(&card(n)).await.unwrap();
    let run = Run::new(Uuid::from_u128(n), "demo", "h1");
    tx.runs().insert(&run).await.unwrap();
    tx.commit().await.unwrap();
    run.id
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn a_timer_is_due_when_its_time_comes_and_is_taken_once() {
    let (store, _guard, _relay) = store("hive_timers").await;
    let community = CommunityId::from_u128(1);
    let run = run_of(&store, community, 1).await;

    let mut tx = store.begin(community).await.unwrap();
    tx.timers()
        .arm(run, "build", TimerKind::Timeout, Duration::from_secs(3600))
        .await
        .unwrap();
    tx.timers()
        .arm(run, "build", TimerKind::Retry, Duration::ZERO)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let mut tx = store.begin(community).await.unwrap();
    // Only the retry is due; the timeout is an hour away.
    assert_eq!(
        tx.timers().due(10).await.unwrap(),
        vec![Timer {
            run_id: run,
            stage: "build".into(),
            kind: TimerKind::Retry
        }]
    );
    let next = tx.timers().next_in().await.unwrap().unwrap();
    assert_eq!(next, Duration::ZERO);
    // A timer that is not due is not taken, and one that is, is taken once.
    assert!(!tx
        .timers()
        .take_due(run, "build", TimerKind::Timeout)
        .await
        .unwrap());
    assert!(tx
        .timers()
        .take_due(run, "build", TimerKind::Retry)
        .await
        .unwrap());
    assert!(!tx
        .timers()
        .take_due(run, "build", TimerKind::Retry)
        .await
        .unwrap());
    let next = tx.timers().next_in().await.unwrap().unwrap();
    assert!(
        next > Duration::from_secs(3500) && next <= Duration::from_secs(3600),
        "{next:?}"
    );
    tx.commit().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn arming_again_replaces_the_time_and_clearing_a_run_drops_its_timers_only() {
    let (store, _guard, _relay) = store("hive_timers_set").await;
    let community = CommunityId::from_u128(1);
    let one = run_of(&store, community, 1).await;
    let two = run_of(&store, community, 2).await;

    let mut tx = store.begin(community).await.unwrap();
    tx.timers()
        .arm(one, "build", TimerKind::Timeout, Duration::ZERO)
        .await
        .unwrap();
    // The same run, stage and kind again: one row, now an hour away.
    tx.timers()
        .arm(one, "build", TimerKind::Timeout, Duration::from_secs(3600))
        .await
        .unwrap();
    tx.timers()
        .arm(two, "build", TimerKind::Timeout, Duration::ZERO)
        .await
        .unwrap();
    assert_eq!(tx.timers().due(10).await.unwrap().len(), 1);
    tx.timers().clear_run(two).await.unwrap();
    assert!(tx.timers().due(10).await.unwrap().is_empty());
    assert!(tx.timers().next_in().await.unwrap().is_some());
    tx.timers()
        .remove(one, "build", TimerKind::Timeout)
        .await
        .unwrap();
    assert_eq!(tx.timers().next_in().await.unwrap(), None);
    tx.commit().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn timers_belong_to_a_community_and_go_with_their_run() {
    let (store, _guard, _relay) = store("hive_timers_tenant").await;
    let (a, b) = (CommunityId::from_u128(1), CommunityId::from_u128(2));
    let run = run_of(&store, a, 1).await;

    let mut tx = store.begin(a).await.unwrap();
    tx.timers()
        .arm(run, "build", TimerKind::Timeout, Duration::ZERO)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let mut other = store.begin(b).await.unwrap();
    assert!(other.timers().due(10).await.unwrap().is_empty());
    assert_eq!(other.timers().next_in().await.unwrap(), None);
    drop(other);

    // Deleting the card takes its run and the run's timers with it.
    let mut tx = store.begin(a).await.unwrap();
    tx.cards().delete(Uuid::from_u128(1)).await.unwrap();
    tx.commit().await.unwrap();
    let mut tx = store.begin(a).await.unwrap();
    assert_eq!(tx.timers().next_in().await.unwrap(), None);
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn a_timer_cannot_name_a_run_that_does_not_exist() {
    let (store, _guard, _relay) = store("hive_timers_fk").await;
    let community = CommunityId::from_u128(1);
    let mut tx = store.begin(community).await.unwrap();
    let refused = tx
        .timers()
        .arm(
            Uuid::from_u128(9),
            "build",
            TimerKind::Timeout,
            Duration::ZERO,
        )
        .await;
    assert!(refused.is_err(), "{refused:?}");
}
