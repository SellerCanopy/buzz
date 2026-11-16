//! Agents diagnose a failed run and file their own tickets; a person decides a code fault, and
//! a card's repos are added and removed by hand.

mod common;

use std::sync::Arc;

use axum::http::StatusCode;
use buzz_hive_api::App;
use buzz_hive_core::access::RelayRole;
use buzz_hive_core::domain::intake::{Issue, IssueState, RepoRef};
use buzz_hive_core::testing::{FakeCodeHost, FakeDirectory, MemStore};
use buzz_hive_core::workflow::source::{MemSource, Source, Sources};
use common::{app, call, call_for, call_unclaimed, community, AGENT, HIVE, MEMBER, OWNER};
use serde_json::{json, Value};

const BUILDER: &str = "b1";
const CHANNEL: &str = "00000000-0000-0000-0000-00000000004d";

fn demo() -> Arc<Sources> {
    let files = MemSource::new()
        .with("demo/kiln-package.yaml", "name: demo\nversion: 1.0.0\n")
        .with(
            "demo/workflow.yaml",
            "version: 2\nstages:\n  - id: research\n    kind: prompt\n    agent: builder\n    prompt: \"Research {{issue.title}}\"\n",
        );
    Arc::new(Sources(vec![Source::new("test", false, files)]))
}

fn repo(name: &str) -> RepoRef {
    RepoRef::parse(name).unwrap()
}

fn issue(name: &str, number: u64, title: &str) -> Issue {
    Issue {
        repo: repo(name),
        number,
        title: title.into(),
        body: String::new(),
        labels: Vec::new(),
        assignees: Vec::new(),
        author: "someone".into(),
        state: IssueState::Open,
        url: format!("https://github.com/{name}/issues/{number}"),
        etag: None,
    }
}

/// An app whose host knows `example-org/api` and `example-org/web`.
async fn world() -> (App, FakeCodeHost) {
    let directory = FakeDirectory::new()
        .with_role(OWNER, RelayRole::Owner)
        .with_role(MEMBER, RelayRole::Member)
        .with_agent(AGENT, OWNER)
        .with_named_agent("builder", BUILDER, OWNER)
        .with_channel(CHANNEL.parse().unwrap(), &[HIVE, BUILDER]);
    let base = app(MemStore::new()).await;
    let host = FakeCodeHost::new();
    host.put_repo(&repo("example-org/api"));
    host.put_repo(&repo("example-org/web"));
    let app = App {
        directory: Arc::new(directory),
        catalog: demo(),
        code_host: Some(Arc::new(host.clone())),
        ..base
    };
    let (status, saved) = call(
        &app,
        "SetSetting",
        OWNER,
        json!({"scope": "user", "key": "update_channel", "valueJson": format!("\"{CHANNEL}\"")}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    (app, host)
}

async fn card(app: &App, title: &str) -> String {
    let (status, card) = call(
        app,
        "CreateCard",
        OWNER,
        json!({"title": title, "workflow": "demo"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{card}");
    card["id"].as_str().unwrap().to_owned()
}

/// Links `repos` to a card, as a person does on the card page, so an agent may name them.
async fn link(app: &App, repos: &[&str]) {
    let id = card(app, "Linked repos").await;
    for repo in repos {
        let (status, body) = call(
            app,
            "AddIssueRepo",
            OWNER,
            json!({"cardId": id, "repo": repo}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
}

async fn cards(app: &App) -> Vec<Value> {
    let (_, body) = call(app, "ListCards", OWNER, json!({})).await;
    body["cards"].as_array().cloned().unwrap_or_default()
}

async fn started_run(app: &App) -> (String, String) {
    let card = card(app, "Fix the build").await;
    let (status, run) = call(app, "StartStage", OWNER, json!({"cardId": card})).await;
    assert_eq!(status, StatusCode::OK, "{run}");
    (card, run["id"].as_str().unwrap().to_owned())
}

/// An int64 field, which the JSON form writes as a string.
fn int(value: &Value) -> i64 {
    value
        .as_str()
        .map_or_else(|| value.as_i64().unwrap(), |s| s.parse().unwrap())
}

fn evidence() -> Value {
    json!([{"source": "log", "quote": "error: no such file"}])
}

#[tokio::test]
async fn an_agent_reads_a_runs_events_and_the_health_of_the_hive() {
    let (app, _) = world().await;
    let (_, run) = started_run(&app).await;
    // A Read agent may read, and is not claimed, so it is capped at Read.
    let (status, body) = call_unclaimed(&app, "GetRunEvents", AGENT, json!({"runId": run})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let events = body["events"].as_array().unwrap();
    assert!(!events.is_empty(), "{body}");
    let last = int(&events.last().unwrap()["seq"]);
    assert_eq!(int(&body["nextSeq"]), last);
    let (_, after) = call_unclaimed(
        &app,
        "GetRunEvents",
        AGENT,
        json!({"runId": run, "afterSeq": last}),
    )
    .await;
    assert!(
        after["events"].as_array().is_none_or(Vec::is_empty),
        "{after}"
    );
    assert_eq!(int(&after["nextSeq"]), last);

    let (status, bad) = call(
        &app,
        "GetRunEvents",
        OWNER,
        json!({"runId": run, "limit": 9999}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
    let (status, _) = call(
        &app,
        "GetRunEvents",
        OWNER,
        json!({"runId": uuid::Uuid::nil()}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, health) = call_unclaimed(&app, "GetHiveHealth", AGENT, json!({})).await;
    assert_eq!(status, StatusCode::OK, "{health}");
    assert_eq!(health["allowlistApplied"], "unknown");
    assert_eq!(int(&health["stuckAfterSeconds"]), 3600);
    assert!(
        health["stuckRuns"].as_array().is_none_or(Vec::is_empty),
        "{health}"
    );
}

#[tokio::test]
async fn an_unclaimed_agent_reads_but_neither_reports_nor_files() {
    let (app, _) = world().await;
    let (card, run) = started_run(&app).await;
    let report = json!({"runId": run, "diagnosis": {"cause": "no file", "evidence": evidence()}});
    let file = json!({"kind": "engine", "title": "Run stuck", "cardId": card});

    let (status, body) = call_unclaimed(&app, "ReportDiagnosis", AGENT, report.clone()).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let (status, body) = call_unclaimed(&app, "FileTicket", AGENT, file.clone()).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    // The person it acts for is Read: still refused.
    let (status, body) = call_for(&app, "ReportDiagnosis", AGENT, MEMBER, report.clone()).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(cards(&app).await.len(), 1, "no ticket card was made");

    // The person it acts for is Run: the same calls go through.
    let (status, body) = call_for(&app, "ReportDiagnosis", AGENT, OWNER, report).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(int(&body["eventSeq"]) > 0, "{body}");
    let (status, body) = call_for(&app, "FileTicket", AGENT, OWNER, file).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["outcome"], "created");
}

#[tokio::test]
async fn a_diagnosis_needs_a_quote_and_lands_on_the_runs_card() {
    let (app, _) = world().await;
    let (card, run) = started_run(&app).await;
    for evidence in [json!([]), json!([{"source": "log", "quote": "  "}])] {
        let (status, body) = call(
            &app,
            "ReportDiagnosis",
            AGENT,
            json!({"runId": run, "diagnosis": {"cause": "no file", "evidence": evidence}}),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["code"], "invalid_argument");
    }
    let (status, body) = call(
        &app,
        "ReportDiagnosis",
        AGENT,
        json!({"runId": run, "diagnosis": {"cause": "no file", "stageId": "research", "evidence": evidence(), "gaps": ["no log of the push"]}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, activity) = call(&app, "ListActivity", OWNER, json!({"cardId": card})).await;
    assert!(activity.to_string().contains("no file"), "{activity}");
}

#[tokio::test]
async fn an_engine_fault_makes_one_backlog_card_with_its_repos_and_a_repeat_adds_a_note() {
    let (app, host) = world().await;
    link(&app, &["example-org/api", "example-org/web"]).await;
    let file = json!({
        "kind": "engine", "title": "Stage never starts", "body": "The thread is not recorded.",
        "repos": ["example-org/api", "example-org/web"],
    });
    let (status, made) = call(&app, "FileTicket", AGENT, file.clone()).await;
    assert_eq!(status, StatusCode::OK, "{made}");
    assert_eq!(made["outcome"], "created");
    let id = made["cardId"].as_str().unwrap();
    let (_, shown) = call(&app, "GetCard", OWNER, json!({"cardId": id})).await;
    assert_eq!(
        shown["state"], "idle",
        "a card with no run is in the backlog: {shown}"
    );
    let repos: Vec<_> = shown["repos"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            format!(
                "{}/{}",
                r["repo"]["owner"].as_str().unwrap(),
                r["repo"]["repo"].as_str().unwrap()
            )
        })
        .collect();
    assert_eq!(repos, ["example-org/api", "example-org/web"]);
    assert!(
        shown.get("issue").is_none(),
        "no GitHub issue exists: {shown}"
    );

    // The same fault again, in other words: a note, and no second card.
    let (status, again) = call(
        &app,
        "FileTicket",
        AGENT,
        json!({"kind": "engine", "title": "  stage NEVER starts ", "body": "Seen again."}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["outcome"], "noted");
    assert_eq!(again["cardId"], id);
    assert_eq!(
        cards(&app).await.len(),
        2,
        "the linked card and the fault's"
    );
    assert!(host.written().is_empty(), "nothing went to GitHub");
}

#[tokio::test]
async fn a_duplicate_fault_comments_on_the_open_issue_and_makes_no_card() {
    let (app, host) = world().await;
    link(&app, &["example-org/api"]).await;
    host.put_issue(issue(
        "example-org/api",
        7,
        "Login fails after the redirect",
    ));
    let before = cards(&app).await.len();
    let (status, body) = call(
        &app,
        "FileTicket",
        AGENT,
        json!({"kind": "ui", "title": "login fails after the redirect", "body": "Seen on run 3.", "repos": ["example-org/api"]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["outcome"], "commented");
    assert_eq!(body["issue"]["number"], 7);
    assert_eq!(cards(&app).await.len(), before, "no second card");
    let written = host.written();
    assert_eq!(written.len(), 1, "{written:?}");
    assert!(written[0].2.contains("Seen on run 3."));

    // Said twice with the same words, the comment is written once.
    let (_, _) = call(
        &app,
        "FileTicket",
        AGENT,
        json!({"kind": "ui", "title": "login fails after the redirect", "body": "Seen on run 3.", "repos": ["example-org/api"]}),
    )
    .await;
    assert_eq!(host.written().len(), 1);
}

#[tokio::test]
async fn a_fault_in_a_cards_code_waits_for_a_person_and_is_imported_once() {
    let (app, host) = world().await;
    let (_, made) = call(
        &app,
        "CreateCard",
        OWNER,
        json!({"title": "Add export", "workflow": "demo"}),
    )
    .await;
    let card = made["id"].as_str().unwrap().to_owned();
    let (status, added) = call(
        &app,
        "AddIssueRepo",
        OWNER,
        json!({"cardId": card, "repo": "example-org/api"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{added}");

    let file = json!({"kind": "code", "title": "Export drops the last row", "body": "Off by one.", "cardId": card});
    let (status, body) = call_for(&app, "FileTicket", AGENT, OWNER, file.clone()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["outcome"], "proposed");
    let proposal = body["proposalId"].clone();
    assert!(
        host.calls().iter().all(|c| !c.starts_with("create_issue")),
        "no GitHub write yet"
    );
    // Filing it again finds the proposal waiting.
    let (_, again) = call_for(&app, "FileTicket", AGENT, OWNER, file).await;
    assert_eq!(again["proposalId"], proposal);

    let (_, listed) = call(&app, "ListProposals", OWNER, json!({})).await;
    assert_eq!(
        listed["proposals"][0]["ticket"]["repo"], "example-org/api",
        "{listed}"
    );

    // An agent never accepts, and a person who did not request it cannot either.
    let accept = json!({"proposalId": proposal, "title": "Export drops the last row", "body": "Off by one."});
    let (status, body) = call(&app, "AcceptTicketProposal", AGENT, accept.clone()).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let (status, body) = call(&app, "AcceptTicketProposal", MEMBER, accept.clone()).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(host.calls().iter().all(|c| !c.starts_with("create_issue")));

    let (status, body) = call(&app, "AcceptTicketProposal", OWNER, accept.clone()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["proposal"]["state"], "accepted");
    assert_eq!(body["card"]["issue"]["number"], 1, "{body}");
    // A second accept finds nothing open and writes nothing again.
    let (status, again) = call(&app, "AcceptTicketProposal", OWNER, accept).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{again}");
    let created = host
        .calls()
        .iter()
        .filter(|c| c.starts_with("create_issue"))
        .count();
    assert_eq!(created, 1);
}

#[tokio::test]
async fn a_card_with_no_issue_takes_repos_in_order_and_the_first_is_the_main_one() {
    let (app, _) = world().await;
    let id = card(&app, "Add export").await;
    for repo in ["example-org/api", "example-org/web"] {
        let (status, body) = call(
            &app,
            "AddIssueRepo",
            OWNER,
            json!({"cardId": id, "repo": repo}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    let (_, shown) = call(&app, "GetCard", OWNER, json!({"cardId": id})).await;
    let repos = shown["repos"].as_array().unwrap();
    assert_eq!(repos.len(), 2, "{shown}");
    assert_eq!(repos[0]["main"], true);
    assert_eq!(repos[0]["repo"]["repo"], "api");
    assert_eq!(repos[1]["repo"]["repo"], "web");
    assert!(repos[0]["removeRefusal"]
        .as_str()
        .unwrap()
        .contains("remove the others first"));

    // The first repo stays while another remains; the second goes, then the first.
    let (status, _) = call(
        &app,
        "RemoveIssueRepo",
        OWNER,
        json!({"cardId": id, "repo": "example-org/api"}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    for repo in ["example-org/web", "example-org/api"] {
        let (status, body) = call(
            &app,
            "RemoveIssueRepo",
            OWNER,
            json!({"cardId": id, "repo": repo}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    let (_, shown) = call(&app, "GetCard", OWNER, json!({"cardId": id})).await;
    assert!(
        shown["repos"].as_array().is_none_or(Vec::is_empty),
        "{shown}"
    );
}

#[tokio::test]
async fn a_repo_is_refused_when_it_is_malformed_a_duplicate_unknown_or_the_card_is_running() {
    let (app, host) = world().await;
    let id = card(&app, "Add export").await;
    let add = |repo: &str, base: &str| json!({"cardId": id, "repo": repo, "base": base});
    for bad in [
        "web",
        "",
        "example-org/web/extra",
        "example-org/..",
        "../web",
        "example-org/web?x",
    ] {
        let (status, body) = call(&app, "AddIssueRepo", OWNER, add(bad, "")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {body}");
    }
    let (status, _) = call(
        &app,
        "AddIssueRepo",
        OWNER,
        add("example-org/web", "bad..base"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        host.calls().is_empty(),
        "a bad name or base costs no host call: {:?}",
        host.calls()
    );

    let (status, _) = call(&app, "AddIssueRepo", OWNER, add("example-org/gone", "")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, shown) = call(&app, "GetCard", OWNER, json!({"cardId": id})).await;
    assert!(
        shown["repos"].as_array().is_none_or(Vec::is_empty),
        "no row was written"
    );

    let (status, _) = call(&app, "AddIssueRepo", OWNER, add("example-org/web", "main")).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(&app, "AddIssueRepo", OWNER, add("example-org/web", "")).await;
    assert_eq!(status, StatusCode::CONFLICT, "a repo already on the card");
    let (status, _) = call(
        &app,
        "RemoveIssueRepo",
        OWNER,
        json!({"cardId": id, "repo": "example-org/api"}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "a repo the card lacks");

    let (status, run) = call(&app, "StartStage", OWNER, json!({"cardId": id})).await;
    assert_eq!(status, StatusCode::OK, "{run}");
    let (status, body) = call(&app, "AddIssueRepo", OWNER, add("example-org/api", "")).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let (status, _) = call(
        &app,
        "RemoveIssueRepo",
        OWNER,
        json!({"cardId": id, "repo": "example-org/web"}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "repos are frozen during a run"
    );
    // A person who only reads cannot change repos.
    let (status, _) = call(&app, "AddIssueRepo", MEMBER, add("example-org/api", "")).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn the_repo_of_the_cards_issue_and_a_repo_with_an_open_pull_request_stay() {
    let (app, host) = world().await;
    host.put_issue(issue("example-org/api", 7, "Login fails"));
    let (status, made) = call(
        &app,
        "CreateCard",
        OWNER,
        json!({"issue": {"host": "github.com", "owner": "example-org", "repo": "api", "number": 7}, "workflow": "demo"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{made}");
    let id = made["id"].as_str().unwrap().to_owned();
    let (status, body) = call(
        &app,
        "AddIssueRepo",
        OWNER,
        json!({"cardId": id, "repo": "example-org/web"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // The issue's repo is the one the card runs in, and cannot go.
    let (status, body) = call(
        &app,
        "RemoveIssueRepo",
        OWNER,
        json!({"cardId": id, "repo": "example-org/api"}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(
        body["message"].as_str().unwrap().contains("issue"),
        "{body}"
    );

    // A repo with an open pull request stays too.
    let pull = buzz_hive_core::domain::card::PullRequest {
        repo: "example-org/web".into(),
        number: 3,
        url: "https://github.example.org/example-org/web/pull/3".into(),
        head: "buzz/x".into(),
        base: "main".into(),
        state: "open".into(),
        draft: true,
        approved: false,
        checks: String::new(),
        head_sha: "abc".into(),
    };
    let mut tx = app.store.begin(community()).await.unwrap();
    tx.pull_requests()
        .put(uuid::Uuid::parse_str(&id).unwrap(), &pull)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let (status, body) = call(
        &app,
        "RemoveIssueRepo",
        OWNER,
        json!({"cardId": id, "repo": "example-org/web"}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(
        body["message"].as_str().unwrap().contains("pull request"),
        "{body}"
    );
}

#[tokio::test]
async fn an_agent_acting_for_a_person_neither_adds_nor_removes_a_cards_repos() {
    let (app, _) = world().await;
    let id = card(&app, "Add export").await;
    let (status, body) = call_for(
        &app,
        "AddIssueRepo",
        AGENT,
        OWNER,
        json!({"cardId": id, "repo": "example-org/api"}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    let (status, _) = call(
        &app,
        "AddIssueRepo",
        OWNER,
        json!({"cardId": id, "repo": "example-org/api"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = call_for(
        &app,
        "RemoveIssueRepo",
        AGENT,
        OWNER,
        json!({"cardId": id, "repo": "example-org/api"}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let (_, shown) = call(&app, "GetCard", OWNER, json!({"cardId": id})).await;
    assert_eq!(shown["repos"].as_array().map(Vec::len), Some(1), "{shown}");
}

#[tokio::test]
async fn an_agent_may_name_only_repos_a_person_linked_and_nothing_is_written_otherwise() {
    let (app, host) = world().await;
    host.put_issue(issue("example-org/api", 7, "Login fails"));
    let before = cards(&app).await.len();
    for kind in ["engine", "ui"] {
        let (status, body) = call(
            &app,
            "FileTicket",
            AGENT,
            json!({"kind": kind, "title": "Login fails", "body": "phish", "repos": ["example-org/api"]}),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{kind}: {body}");
    }
    assert_eq!(cards(&app).await.len(), before, "no card was made");
    assert!(
        host.calls().is_empty(),
        "GitHub was not asked: {:?}",
        host.calls()
    );
}

#[tokio::test]
async fn a_fault_in_the_engine_is_filed_when_github_cannot_be_asked_but_a_code_fault_is_not() {
    let (app, host) = world().await;
    link(&app, &["example-org/api"]).await;
    let file =
        |title: &str| json!({"kind": "engine", "title": title, "repos": ["example-org/api"]});

    host.fail_next(buzz_hive_core::ports::code_host::HostError::Other(
        "down".into(),
    ));
    let (status, body) = call(&app, "FileTicket", AGENT, file("Stage never starts")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["outcome"], "created");

    let without = App {
        code_host: None,
        ..app.clone()
    };
    let (status, body) = call(&without, "FileTicket", AGENT, file("Run never ends")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["outcome"], "created");

    // A fault in a card's code needs the host.
    let (_, made) = call(
        &app,
        "CreateCard",
        OWNER,
        json!({"title": "Add export", "workflow": "demo"}),
    )
    .await;
    let id = made["id"].as_str().unwrap();
    call(
        &app,
        "AddIssueRepo",
        OWNER,
        json!({"cardId": id, "repo": "example-org/api"}),
    )
    .await;
    let (status, body) = call(
        &without,
        "FileTicket",
        AGENT,
        json!({"kind": "code", "title": "Drops a row", "cardId": id}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
}

#[tokio::test]
async fn a_card_takes_ten_repos_and_the_eleventh_is_refused_with_nothing_written() {
    let (app, host) = world().await;
    let id = card(&app, "Wide change").await;
    let names: Vec<String> = (0..11).map(|n| format!("example-org/r{n}")).collect();
    for name in &names {
        host.put_repo(&repo(name));
    }
    let add = |name: &str| json!({"cardId": id, "repo": name});
    for name in &names[..10] {
        let (status, body) = call(&app, "AddIssueRepo", OWNER, add(name)).await;
        assert_eq!(status, StatusCode::OK, "{name}: {body}");
    }
    let held = |shown: &Value| shown["repos"].as_array().map_or(0, Vec::len);
    let (_, shown) = call(&app, "GetCard", OWNER, json!({"cardId": id})).await;
    assert_eq!(held(&shown), 10);

    let (status, body) = call(&app, "AddIssueRepo", OWNER, add(&names[10])).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body["message"].as_str().unwrap().contains("at most 10"));
    let (status, _) = call(&app, "AddIssueRepo", OWNER, add(&names[0])).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a duplicate is not a second row"
    );
    let (_, shown) = call(&app, "GetCard", OWNER, json!({"cardId": id})).await;
    assert_eq!(held(&shown), 10, "{shown}");

    // Taking one off makes room, and an absent repo is not found.
    let (status, _) = call(
        &app,
        "RemoveIssueRepo",
        OWNER,
        json!({"cardId": id, "repo": names[10]}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, body) = call(
        &app,
        "RemoveIssueRepo",
        OWNER,
        json!({"cardId": id, "repo": names[9]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = call(&app, "AddIssueRepo", OWNER, add(&names[10])).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}
