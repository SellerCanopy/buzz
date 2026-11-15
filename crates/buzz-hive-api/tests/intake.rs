//! The intake procedures over `MemStore` and `FakeCodeHost`.

mod common;

use std::sync::Arc;

use axum::http::StatusCode;
use buzz_hive_api::App;
use buzz_hive_core::domain::intake::{Issue, IssueState, RepoRef};
use buzz_hive_core::ports::code_host::HostError;
use buzz_hive_core::testing::{FakeCodeHost, MemStore};
use chrono::Utc;
use common::{app, call, AGENT, MEMBER, OWNER, STRANGER};
use serde_json::{json, Value};

fn repo() -> RepoRef {
    RepoRef::parse("example-org/api").unwrap()
}

fn issue(number: u64, title: &str) -> Issue {
    Issue {
        repo: repo(),
        number,
        title: title.into(),
        body: format!("Body of {title}."),
        labels: vec!["bug".into()],
        assignees: Vec::new(),
        author: "octo".into(),
        state: IssueState::Open,
        url: format!("https://github.example.org/example-org/api/issues/{number}"),
        etag: None,
    }
}

async fn with_host() -> (App, FakeCodeHost) {
    let mut app = app(MemStore::new()).await;
    let host = FakeCodeHost::new();
    app.code_host = Some(Arc::new(host.clone()));
    (app, host)
}

fn rule_body() -> Value {
    json!({"rule": {
        "name": "Bugs",
        "enabled": true,
        "workflow": "rpi",
        "intake": {"host": "github.com", "repos": ["example-org/api"], "label": "bug", "intervalS": 120}
    }})
}

/// A name, a change to the valid rule, and the status and words the refusal must carry.
type Case = (
    &'static str,
    Box<dyn Fn(&mut Value)>,
    StatusCode,
    &'static str,
);

async fn save(app: &App) -> Value {
    let (status, rule) = call(app, "SaveIntakeRule", OWNER, rule_body()).await;
    assert_eq!(status, StatusCode::OK, "{rule}");
    rule
}

#[tokio::test]
async fn a_rule_is_saved_listed_and_deleted() {
    let (app, _) = with_host().await;
    let rule = save(&app).await;
    let id = rule["id"].as_str().unwrap();
    assert!(uuid::Uuid::parse_str(id).is_ok(), "{rule}");
    assert_eq!(rule["intake"]["repos"][0], "example-org/api");
    assert_eq!(rule["intake"]["assignees"], "anyone");
    assert_eq!(rule["enabled"], true);

    let (_, list) = call(&app, "ListIntakeRules", MEMBER, json!({})).await;
    assert_eq!(list["rules"][0]["id"], id);
    assert_eq!(list["rules"][0]["intake"]["label"], "bug");

    // Saving again with the id replaces the rule.
    let mut body = rule_body();
    body["rule"]["id"] = json!(id);
    body["rule"]["name"] = json!("Renamed");
    let (status, saved) = call(&app, "SaveIntakeRule", OWNER, body).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let (_, list) = call(&app, "ListIntakeRules", MEMBER, json!({})).await;
    assert_eq!(list["rules"].as_array().unwrap().len(), 1);
    assert_eq!(list["rules"][0]["name"], "Renamed");

    let (status, _) = call(&app, "DeleteIntakeRule", OWNER, json!({"id": id})).await;
    assert_eq!(status, StatusCode::OK);
    let (_, list) = call(&app, "ListIntakeRules", MEMBER, json!({})).await;
    assert!(list["rules"].as_array().is_none_or(Vec::is_empty), "{list}");
    let (status, error) = call(&app, "DeleteIntakeRule", OWNER, json!({"id": id})).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{error}");
}

#[tokio::test]
async fn saving_and_deleting_a_rule_wake_the_intake_supervisor() {
    let (app, _) = with_host().await;
    let wake = app.intake.clone();
    let rule = save(&app).await;
    tokio::time::timeout(std::time::Duration::from_secs(1), wake.notified())
        .await
        .expect("save woke it");
    call(&app, "DeleteIntakeRule", OWNER, json!({"id": rule["id"]})).await;
    tokio::time::timeout(std::time::Duration::from_secs(1), wake.notified())
        .await
        .expect("delete woke it");
}

#[tokio::test]
async fn a_rule_the_engine_cannot_run_is_refused_with_the_reason() {
    let (app, _) = with_host().await;
    let cases: Vec<Case> = vec![
        (
            "under a minute",
            Box::new(|b| b["rule"]["intake"]["intervalS"] = json!(59)),
            StatusCode::BAD_REQUEST,
            "interval_s",
        ),
        (
            "a typed query",
            Box::new(|b| b["rule"]["intake"]["query"] = json!("repo:example-org/api is:open")),
            StatusCode::BAD_REQUEST,
            "query",
        ),
        (
            "assigned to me",
            Box::new(|b| b["rule"]["intake"]["assignedToMe"] = json!(true)),
            StatusCode::BAD_REQUEST,
            "assigned_to_me",
        ),
        (
            "two repos",
            Box::new(|b| {
                b["rule"]["intake"]["repos"] = json!(["example-org/api", "example-org/web"]);
            }),
            StatusCode::BAD_REQUEST,
            "exactly one repo",
        ),
        (
            "a repo that is not owner/name",
            Box::new(|b| b["rule"]["intake"]["repos"] = json!(["api"])),
            StatusCode::BAD_REQUEST,
            "owner/name",
        ),
        (
            "an assignee filter it does not know",
            Box::new(|b| b["rule"]["intake"]["assignees"] = json!("unassigned_or_me")),
            StatusCode::BAD_REQUEST,
            "unassigned_or_me",
        ),
        (
            "another host",
            Box::new(|b| b["rule"]["intake"]["host"] = json!("git.example.org")),
            StatusCode::BAD_REQUEST,
            "git.example.org",
        ),
        (
            "an unknown workflow",
            Box::new(|b| b["rule"]["workflow"] = json!("no-such-workflow")),
            StatusCode::NOT_FOUND,
            "no-such-workflow",
        ),
        (
            "an unknown rule id",
            Box::new(|b| b["rule"]["id"] = json!(uuid::Uuid::new_v4().to_string())),
            StatusCode::NOT_FOUND,
            "does not exist",
        ),
        (
            "an id that is not one",
            Box::new(|b| b["rule"]["id"] = json!("seven")),
            StatusCode::BAD_REQUEST,
            "id",
        ),
    ];
    for (name, edit, status, word) in cases {
        let mut body = rule_body();
        edit(&mut body);
        let (got, error) = call(&app, "SaveIntakeRule", OWNER, body).await;
        assert_eq!(got, status, "{name}: {error}");
        assert!(
            error["message"].as_str().unwrap().contains(word),
            "{name}: {error}"
        );
    }
    let (_, list) = call(&app, "ListIntakeRules", OWNER, json!({})).await;
    assert!(list["rules"].as_array().is_none_or(Vec::is_empty));
}

#[tokio::test]
async fn reading_needs_read_and_changing_a_rule_needs_run() {
    let (app, _) = with_host().await;
    let (status, _) = call(&app, "SaveIntakeRule", MEMBER, rule_body()).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "a Read member cannot save");
    let (status, _) = call(&app, "RefreshIntake", MEMBER, json!({})).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a Read member cannot refresh"
    );
    let (status, _) = call(&app, "DeleteIntakeRule", MEMBER, json!({"id": "x"})).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    for method in ["ListIntakeRules", "ListOwners", "ListRepos", "SearchIssues"] {
        let (status, _) = call(&app, method, STRANGER, json!({})).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} needs Read");
    }
    let (status, _) = call(&app, "SaveIntakeRule", AGENT, rule_body()).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "an agent may not change what becomes cards"
    );
    let rule = save(&app).await;
    let (status, _) = call(&app, "DeleteIntakeRule", AGENT, json!({"id": rule["id"]})).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn without_a_token_the_host_procedures_name_the_setting() {
    let app = app(MemStore::new()).await;
    for (method, body) in [
        ("ListRepos", json!({})),
        ("ListOwners", json!({})),
        (
            "SearchIssues",
            json!({"filter": {"repos": ["example-org/api"]}}),
        ),
        ("RefreshIntake", json!({})),
        (
            "CreateCard",
            json!({"issue": {"owner": "example-org", "repo": "api", "number": 1}}),
        ),
        (
            "PreviewIntake",
            json!({"rule": {"repos": ["example-org/api"]}}),
        ),
    ] {
        let (status, error) = call(&app, method, OWNER, body).await;
        assert_eq!(status, StatusCode::CONFLICT, "{method}: {error}");
        assert!(
            error["message"].as_str().unwrap().contains("GH_TOKEN"),
            "{error}"
        );
    }
    // Rules are only stored, so they can be written before a token is set.
    save(&app).await;
}

#[tokio::test]
async fn a_refresh_makes_cards_and_the_board_shows_where_they_came_from() {
    let (app, host) = with_host().await;
    host.put_repo(&repo());
    host.put_issue(issue(7, "Crash on save"));
    let rule = save(&app).await;

    let (status, out) = call(&app, "RefreshIntake", OWNER, json!({"ruleId": rule["id"]})).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["matched"], 1);
    assert_eq!(out["addedCardIds"].as_array().unwrap().len(), 1);
    let id = out["addedCardIds"][0].as_str().unwrap();

    let (_, list) = call(&app, "ListCards", MEMBER, json!({})).await;
    let card = &list["cards"][0];
    assert_eq!(card["id"], id);
    assert_eq!(card["title"], "Crash on save");
    assert_eq!(card["workflow"], "rpi");
    assert_eq!(card["issue"]["owner"], "example-org");
    assert_eq!(card["issue"]["repo"], "api");
    assert_eq!(card["issue"]["number"], 7);
    assert_eq!(card["intakeRuleId"], rule["id"]);
    assert!(card["issueUrl"].as_str().unwrap().ends_with("/issues/7"));

    let (_, got) = call(&app, "GetCard", MEMBER, json!({"cardId": id})).await;
    assert_eq!(got["issueLabels"], json!(["bug"]));
    assert_eq!(got["body"], "Body of Crash on save.");

    // A second refresh adds nothing, and the issue closing marks the card.
    let (_, again) = call(&app, "RefreshIntake", OWNER, json!({})).await;
    assert!(
        again
            .get("addedCardIds")
            .is_none_or(|a| a.as_array().unwrap().is_empty()),
        "{again}"
    );
    host.remove_issue(&repo(), 7);
    let (_, gone) = call(&app, "RefreshIntake", OWNER, json!({})).await;
    assert_eq!(gone["marked"], 1, "{gone}");
    assert!(gone.get("removed").is_none_or(|r| r == 0));
    let (_, got) = call(&app, "GetCard", MEMBER, json!({"cardId": id})).await;
    assert!(!got["intakeStatus"].as_str().unwrap().is_empty(), "{got}");
}

#[tokio::test]
async fn a_refresh_with_no_rule_or_a_rate_limit_is_refused_at_once() {
    let (app, host) = with_host().await;
    let (status, error) = call(&app, "RefreshIntake", OWNER, json!({})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    let rule = save(&app).await;
    host.fail_next(HostError::RateLimited {
        retry_after: std::time::Duration::from_secs(90),
    });
    let (status, error) = call(&app, "RefreshIntake", OWNER, json!({"ruleId": rule["id"]})).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{error}");
    assert!(
        error["message"].as_str().unwrap().contains("90 seconds"),
        "{error}"
    );
    host.fail_next(HostError::Unauthorized("bad credentials".into()));
    let (status, error) = call(&app, "RefreshIntake", OWNER, json!({})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    let (status, error) = call(
        &app,
        "RefreshIntake",
        OWNER,
        json!({"ruleId": uuid::Uuid::new_v4().to_string()}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{error}");
}

#[tokio::test]
async fn a_preview_lists_what_the_rule_takes_and_stores_nothing() {
    let (app, host) = with_host().await;
    host.put_issue(issue(1, "One"));
    host.put_issue(issue(2, "Two"));
    let (status, out) = call(
        &app,
        "PreviewIntake",
        MEMBER,
        json!({"rule": {"repos": ["example-org/api"], "authors": ["octo"]}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["issues"].as_array().unwrap().len(), 2);
    assert_eq!(out["issues"][0]["ref"]["repo"], "api");
    let (_, list) = call(&app, "ListCards", MEMBER, json!({})).await;
    assert!(list["cards"].as_array().is_none_or(Vec::is_empty));
    let (_, rules) = call(&app, "ListIntakeRules", MEMBER, json!({})).await;
    assert!(rules["rules"].as_array().is_none_or(Vec::is_empty));
    let (status, error) = call(
        &app,
        "PreviewIntake",
        MEMBER,
        json!({"rule": {"repos": []}}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
}

#[tokio::test]
async fn the_pickers_list_repos_owners_and_search_issues() {
    let (app, host) = with_host().await;
    host.put_repo(&repo());
    host.set_owners(&["octo", "example-org"]);
    host.put_issue(issue(3, "Slow query"));

    let (_, repos) = call(&app, "ListRepos", MEMBER, json!({"host": "github.com"})).await;
    assert_eq!(repos["repos"], json!(["example-org/api"]));
    let (status, _) = call(
        &app,
        "ListRepos",
        MEMBER,
        json!({"host": "git.example.org"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = call(&app, "ListRepos", MEMBER, json!({"owner": "a/b"})).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "an owner cannot reach another path"
    );

    let (_, owners) = call(&app, "ListOwners", MEMBER, json!({})).await;
    assert_eq!(owners["owners"], json!(["octo", "example-org"]));

    let (status, hits) = call(
        &app,
        "SearchIssues",
        MEMBER,
        json!({"filter": {"repos": ["example-org/api"], "state": "open"}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{hits}");
    assert_eq!(hits["issues"][0]["ref"]["number"], 3);
    assert_eq!(hits["issues"][0]["labels"], json!(["bug"]));

    for filter in [
        json!({"repos": []}),
        json!({"repos": ["a/1", "a/2", "a/3", "a/4", "a/5", "a/6"]}),
        json!({"repos": ["example-org/api"], "state": "merged"}),
        json!({"repos": ["example-org/api"], "assignee": "@me"}),
    ] {
        let (status, error) = call(&app, "SearchIssues", MEMBER, json!({"filter": filter})).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{filter}: {error}");
    }
    let (status, error) = call(
        &app,
        "SearchIssues",
        MEMBER,
        json!({"filter": {"repos": ["example-org/api"]}, "cursor": "not-a-page"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
}

#[tokio::test]
async fn a_card_is_made_from_an_issue_once_and_its_comments_are_read_live() {
    let (app, host) = with_host().await;
    host.put_issue(issue(9, "Add a retry"));
    host.put_comment(&repo(), 9, "octo", "Seen on staging too.", Utc::now());
    let body = json!({"workflow": "rpi", "issue": {"host": "github.com", "owner": "example-org", "repo": "api", "number": 9}});

    let (status, card) = call(&app, "CreateCard", OWNER, body.clone()).await;
    assert_eq!(status, StatusCode::OK, "{card}");
    assert_eq!(card["title"], "Add a retry");
    assert_eq!(card["issue"]["number"], 9);
    assert!(
        card.get("intakeRuleId").is_none_or(|r| r == ""),
        "a person made it: {card}"
    );

    let (_, again) = call(&app, "CreateCard", OWNER, body).await;
    assert_eq!(
        again["id"], card["id"],
        "the issue's card is returned, not a second"
    );
    let (_, list) = call(&app, "ListCards", MEMBER, json!({})).await;
    assert_eq!(list["cards"].as_array().unwrap().len(), 1);

    let (status, comments) = call(
        &app,
        "ListIssueComments",
        MEMBER,
        json!({"cardId": card["id"]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{comments}");
    assert_eq!(comments["comments"][0]["author"], "octo");
    assert_eq!(comments["comments"][0]["body"], "Seen on staging too.");

    let (_, plain) = call(&app, "CreateCard", OWNER, json!({"title": "No issue"})).await;
    let (status, error) = call(
        &app,
        "ListIssueComments",
        MEMBER,
        json!({"cardId": plain["id"]}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{error}");
}

#[tokio::test]
async fn an_issue_that_cannot_be_a_card_is_refused() {
    let (app, _) = with_host().await;
    for (issue, status) in [
        (
            json!({"owner": "example-org", "repo": "api", "number": 0}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"owner": "example-org", "repo": "api", "number": -3}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"owner": "../x", "repo": "api", "number": 1}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"host": "git.example.org", "owner": "a", "repo": "b", "number": 1}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"owner": "example-org", "repo": "api", "number": 404}),
            StatusCode::NOT_FOUND,
        ),
    ] {
        let (got, error) = call(&app, "CreateCard", OWNER, json!({"issue": issue})).await;
        assert_eq!(got, status, "{issue}: {error}");
    }
}
