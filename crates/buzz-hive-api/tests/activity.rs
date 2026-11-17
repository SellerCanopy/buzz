//! `ListActivity`: the limit bounds and paging by `after_seq`. Ported from the Go engine's
//! `TestListActivityAcceptsTheLimitBounds` and `TestListActivityPagesAndFilters`
//! (`api/activity_test.go`).

mod common;

use axum::http::StatusCode;
use buzz_hive_api::App;
use buzz_hive_core::testing::MemStore;
use common::{app, call, MEMBER, OWNER};
use serde_json::{json, Value};

/// The most entries one `ListActivity` call returns (`MAX_ACTIVITY_LIMIT`).
const MAX_LIMIT: i64 = 1000;

/// A card with `notes` notes on it, oldest first.
async fn card_with_notes(notes: usize) -> (App, Value) {
    let app = app(MemStore::new()).await;
    let (status, card) = call(&app, "CreateCard", OWNER, json!({"title": "Notes"})).await;
    assert_eq!(status, StatusCode::OK, "{card}");
    let card = card["id"].clone();
    for n in 1..=notes {
        let (status, reply) = call(
            &app,
            "AddNote",
            OWNER,
            json!({"cardId": card, "summary": format!("note {n}")}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{reply}");
    }
    (app, card)
}

/// The summaries of the entries a `ListActivity` reply holds, in order.
fn summaries(reply: &Value) -> Vec<String> {
    reply["entries"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|entry| entry["summary"].as_str().unwrap_or_default().to_owned())
        .collect()
}

#[tokio::test]
async fn a_limit_over_the_maximum_or_below_zero_is_refused_and_the_bounds_are_accepted() {
    let (app, card) = card_with_notes(1).await;
    for limit in [-1, MAX_LIMIT + 1] {
        let (status, reply) = call(
            &app,
            "ListActivity",
            MEMBER,
            json!({"cardId": card, "limit": limit}),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "limit {limit}: {reply}");
        assert_eq!(reply["code"], "invalid_argument");
        assert!(
            reply["message"].as_str().unwrap().contains("1000"),
            "the refusal names the maximum: {reply}"
        );
    }
    // 0 is the default, 1 and the maximum are the bounds.
    for limit in [0, 1, MAX_LIMIT] {
        let (status, reply) = call(
            &app,
            "ListActivity",
            MEMBER,
            json!({"cardId": card, "limit": limit}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "limit {limit}: {reply}");
        assert!(!summaries(&reply).is_empty(), "limit {limit}: {reply}");
    }
}

#[tokio::test]
async fn after_seq_pages_through_the_entries_oldest_first_without_repeats() {
    let (app, card) = card_with_notes(5).await;

    let (status, all) = call(&app, "ListActivity", MEMBER, json!({"cardId": card})).await;
    assert_eq!(status, StatusCode::OK, "{all}");
    let notes: Vec<String> = summaries(&all)
        .into_iter()
        .filter(|s| s.starts_with("note "))
        .collect();
    assert_eq!(notes, ["note 1", "note 2", "note 3", "note 4", "note 5"]);

    // Read the same log two entries at a time, each page starting after the last entry seen.
    let mut seen = Vec::new();
    let mut after = json!(0);
    loop {
        let (status, page) = call(
            &app,
            "ListActivity",
            MEMBER,
            json!({"cardId": card, "limit": 2, "afterSeq": after}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{page}");
        // An empty page leaves `entries` out of the JSON.
        let entries = page["entries"].as_array().cloned().unwrap_or_default();
        assert!(entries.len() <= 2, "a page holds at most the limit: {page}");
        let Some(last) = entries.last() else { break };
        after = last["eventSeq"].clone();
        seen.extend(summaries(&page));
        assert!(seen.len() < 100, "paging did not end");
    }
    assert_eq!(seen, summaries(&all), "the pages add up to the whole log");
}

#[tokio::test]
async fn a_card_that_does_not_exist_has_no_activity_to_list() {
    let (app, _) = card_with_notes(0).await;
    let (status, reply) = call(
        &app,
        "ListActivity",
        MEMBER,
        json!({"cardId": "00000000-0000-0000-0000-0000000000aa"}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{reply}");
}
