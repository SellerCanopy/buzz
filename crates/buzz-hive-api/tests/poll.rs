//! `PollEvents`: wake on commit, wait out the limit, and clamp the wait.

mod common;

use std::time::Duration;

use buzz_hive_core::testing::MemStore;
use common::{app, call, MEMBER, OWNER};
use serde_json::json;
use tokio::time::Instant;

#[tokio::test(start_paused = true)]
async fn a_parked_poll_returns_as_soon_as_a_card_is_created() {
    let app = app(MemStore::new()).await;
    let started = Instant::now();
    let poll = {
        let app = app.clone();
        tokio::spawn(
            async move { call(&app, "PollEvents", MEMBER, json!({"waitSeconds": 20})).await },
        )
    };
    // Let the poll read once and park.
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert!(!poll.is_finished());

    call(
        &app,
        "CreateCard",
        OWNER,
        json!({"title": "wakes the poll"}),
    )
    .await;
    let (_, reply) = poll.await.unwrap();
    assert_eq!(reply["events"][0]["type"], "card.created");
    assert!(
        started.elapsed() < Duration::from_millis(100),
        "woken by the commit, not by the timeout: {:?}",
        started.elapsed()
    );
}

#[tokio::test(start_paused = true)]
async fn a_poll_with_nothing_new_returns_at_the_wait_limit_with_the_cursor_unchanged() {
    let app = app(MemStore::new()).await;
    call(&app, "CreateCard", OWNER, json!({"title": "a"})).await;
    let started = Instant::now();
    let (status, reply) = call(
        &app,
        "PollEvents",
        MEMBER,
        json!({"afterSeq": "1", "waitSeconds": 7}),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(started.elapsed(), Duration::from_secs(7));
    assert!(reply.get("events").is_none());
    assert_eq!(reply["nextSeq"], "1");
}

#[tokio::test(start_paused = true)]
async fn a_wait_of_sixty_is_clamped_to_twenty_five() {
    let app = app(MemStore::new()).await;
    let started = Instant::now();
    call(&app, "PollEvents", MEMBER, json!({"waitSeconds": 60})).await;
    assert_eq!(started.elapsed(), Duration::from_secs(25));
}

#[tokio::test(start_paused = true)]
async fn a_wait_of_zero_or_less_returns_at_once() {
    let app = app(MemStore::new()).await;
    let started = Instant::now();
    call(&app, "PollEvents", MEMBER, json!({})).await;
    call(&app, "PollEvents", MEMBER, json!({"waitSeconds": -5})).await;
    assert_eq!(started.elapsed(), Duration::ZERO);
}

#[tokio::test(start_paused = true)]
async fn events_come_back_after_the_cursor_in_order() {
    let app = app(MemStore::new()).await;
    for title in ["one", "two", "three"] {
        call(&app, "CreateCard", OWNER, json!({"title": title})).await;
    }
    let (_, reply) = call(&app, "PollEvents", MEMBER, json!({"afterSeq": "1"})).await;
    let seqs: Vec<_> = reply["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["seq"].clone())
        .collect();
    assert_eq!(seqs, [json!("2"), json!("3")]);
    assert_eq!(reply["nextSeq"], "3");
    let (status, bad) = call(&app, "PollEvents", MEMBER, json!({"afterSeq": "-1"})).await;
    assert_eq!(status, 400, "{bad}");
}

#[tokio::test(start_paused = true)]
async fn a_shutdown_releases_parked_polls() {
    let app = app(MemStore::new()).await;
    let poll = {
        let app = app.clone();
        tokio::spawn(
            async move { call(&app, "PollEvents", MEMBER, json!({"waitSeconds": 25})).await },
        )
    };
    tokio::time::sleep(Duration::from_millis(10)).await;
    let closed_at = Instant::now();
    app.bus.close();
    let (status, reply) = poll.await.unwrap();
    assert_eq!(status, 200);
    assert!(reply.get("events").is_none());
    assert!(closed_at.elapsed() < Duration::from_secs(1));
}

#[tokio::test]
async fn a_parked_poll_wakes_within_100_ms_on_the_real_clock() {
    let app = app(MemStore::new()).await;
    let poll = {
        let app = app.clone();
        tokio::spawn(
            async move { call(&app, "PollEvents", MEMBER, json!({"waitSeconds": 20})).await },
        )
    };
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!poll.is_finished(), "the poll is parked");
    let committed = std::time::Instant::now();
    call(
        &app,
        "CreateCard",
        OWNER,
        json!({"title": "wakes the poll"}),
    )
    .await;
    let (_, reply) = poll.await.unwrap();
    assert_eq!(reply["events"][0]["type"], "card.created");
    assert!(
        committed.elapsed() < Duration::from_millis(100),
        "{:?}",
        committed.elapsed()
    );
}
