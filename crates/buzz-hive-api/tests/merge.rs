//! `MergeCard` and `SetMergeOrder` over `MemStore` and `FakeCodeHost`.

mod common;

use std::sync::Arc;

use axum::http::StatusCode;
use buzz_hive_api::App;
use buzz_hive_core::ports::code_host::{Check, CheckState, CodeHost, HostError, NewPullRequest};
use buzz_hive_core::testing::{FakeCodeHost, MemStore};
use common::{app, call, community, AGENT, MEMBER, OWNER};
use serde_json::{json, Value};
use uuid::Uuid;

/// A card with a ready pull request whose checks have passed, on the host and in the store.
async fn with_ready_pull_request() -> (App, FakeCodeHost, Value, u64) {
    let mut app = app(MemStore::new()).await;
    let host = FakeCodeHost::new();
    app.code_host = Some(Arc::new(host.clone()));
    let (status, card) = call(&app, "CreateCard", OWNER, json!({"title": "Fix login"})).await;
    assert_eq!(status, StatusCode::OK, "{card}");
    let repo = buzz_hive_core::domain::intake::RepoRef::parse("example-org/api").unwrap();
    let pull = host
        .open_pull_request(
            &NewPullRequest {
                repo: repo.clone(),
                head: "buzz/fix".into(),
                base: "main".into(),
                title: "Fix login".into(),
                body: String::new(),
                draft: false,
            },
            "key",
        )
        .await
        .unwrap();
    host.set_checks(
        &repo,
        pull.number,
        vec![Check {
            name: "build".into(),
            state: CheckState::Passed,
        }],
    );
    let id = Uuid::parse_str(card["id"].as_str().unwrap()).unwrap();
    let mut tx = app.store.begin(community()).await.unwrap();
    tx.pull_requests().put(id, &pull).await.unwrap();
    tx.commit().await.unwrap();
    (app, host, card, pull.number)
}

#[tokio::test]
async fn a_person_with_run_merges_the_pull_request_and_the_card_shows_it() {
    let (app, host, card, number) = with_ready_pull_request().await;
    let (status, merged) = call(&app, "MergeCard", OWNER, json!({"cardId": card["id"]})).await;
    assert_eq!(status, StatusCode::OK, "{merged}");
    assert_eq!(merged["pullRequest"]["state"], "merged");
    assert_eq!(merged["pullRequest"]["checks"], "passing");
    assert!(host
        .calls()
        .contains(&format!("merge example-org/api#{number} squash")));

    // A second call finds it merged and merges nothing again.
    let (status, again) = call(&app, "MergeCard", OWNER, json!({"cardId": card["id"]})).await;
    assert_eq!(status, StatusCode::OK, "{again}");
    let merges = host
        .calls()
        .iter()
        .filter(|c| c.starts_with("merge "))
        .count();
    assert_eq!(merges, 1);
}

#[tokio::test]
async fn reading_needs_no_more_than_read_but_merging_needs_run_and_a_person() {
    let (app, host, card, _) = with_ready_pull_request().await;
    let body = json!({"cardId": card["id"]});
    let (status, error) = call(&app, "MergeCard", MEMBER, body.clone()).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{error}");
    let (status, error) = call(
        &app,
        "SetMergeOrder",
        MEMBER,
        json!({"cardId": card["id"], "repos": []}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{error}");

    // The agent has Run, and the engine still refuses it: only a person merges.
    let (status, error) = call(&app, "MergeCard", AGENT, body).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{error}");
    assert!(
        error.to_string().contains("only a person may merge a card"),
        "{error}"
    );
    assert!(host.calls().iter().all(|c| !c.starts_with("merge ")));
}

#[tokio::test]
async fn nothing_is_merged_when_the_checks_have_not_passed() {
    let (app, host, card, number) = with_ready_pull_request().await;
    let repo = buzz_hive_core::domain::intake::RepoRef::parse("example-org/api").unwrap();
    host.set_checks(
        &repo,
        number,
        vec![Check {
            name: "build".into(),
            state: CheckState::Failed,
        }],
    );
    let (status, error) = call(&app, "MergeCard", OWNER, json!({"cardId": card["id"]})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert!(error.to_string().contains("failing checks"), "{error}");
    assert!(host.calls().iter().all(|c| !c.starts_with("merge ")));
}

#[tokio::test]
async fn a_push_after_the_checks_were_read_is_refused_and_the_next_try_merges() {
    let (app, host, card, number) = with_ready_pull_request().await;
    let repo = buzz_hive_core::domain::intake::RepoRef::parse("example-org/api").unwrap();
    let checked = host.head_of(&repo, number);
    // The branch moves between the read of the checks and the merge.
    host.push_after_checks(&repo, number);
    let (status, error) = call(&app, "MergeCard", OWNER, json!({"cardId": card["id"]})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert!(
        error.to_string().contains("head branch was modified"),
        "{error}"
    );
    assert_ne!(host.head_of(&repo, number), checked);
    let (status, shown) = call(&app, "GetCard", OWNER, json!({"cardId": card["id"]})).await;
    assert_eq!(status, StatusCode::OK, "{shown}");
    assert_ne!(
        shown["pullRequest"]["state"], "merged",
        "the unchecked commit was not merged"
    );

    // The next try reads the checks of the new head and merges that commit.
    let (status, merged) = call(&app, "MergeCard", OWNER, json!({"cardId": card["id"]})).await;
    assert_eq!(status, StatusCode::OK, "{merged}");
    assert_eq!(merged["pullRequest"]["state"], "merged");
}

#[tokio::test]
async fn a_host_that_refuses_the_token_is_told_and_a_service_with_no_token_says_how_to_set_one() {
    let (app, host, card, _) = with_ready_pull_request().await;
    host.fail_next(HostError::Unauthorized("Bad credentials".into()));
    let (status, error) = call(&app, "MergeCard", OWNER, json!({"cardId": card["id"]})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert!(
        error.to_string().contains("refused the team token"),
        "{error}"
    );

    let mut bare = app.clone();
    bare.code_host = None;
    let (status, error) = call(&bare, "MergeCard", OWNER, json!({"cardId": card["id"]})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert!(error.to_string().contains("GH_TOKEN"), "{error}");
}

#[tokio::test]
async fn a_card_that_does_not_exist_or_has_no_pull_request_is_refused() {
    let (app, _, _, _) = with_ready_pull_request().await;
    let (status, error) = call(
        &app,
        "MergeCard",
        OWNER,
        json!({"cardId": Uuid::new_v4().to_string()}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{error}");
    let (_, plain) = call(
        &app,
        "CreateCard",
        OWNER,
        json!({"title": "No pull request"}),
    )
    .await;
    let (status, error) = call(&app, "MergeCard", OWNER, json!({"cardId": plain["id"]})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    let (status, error) = call(&app, "MergeCard", OWNER, json!({"cardId": "7"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
}

#[tokio::test]
async fn the_merge_order_of_a_card_with_no_repo_is_refused() {
    let (app, _, card, _) = with_ready_pull_request().await;
    let (status, error) = call(
        &app,
        "SetMergeOrder",
        OWNER,
        json!({"cardId": card["id"], "repos": ["example-org/api"]}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
    assert!(error.to_string().contains("no repo"), "{error}");
}
