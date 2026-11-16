//! The GitHub adapter against the replay server: what it asks for, what it makes of the
//! answers, and that a request nobody planned for fails the test and is named.

mod support;

use std::time::Duration;

use buzz_hive::github_adapter::GitHub;
use buzz_hive_core::domain::intake::{IssueFilter, IssueState, Page, RepoRef};
use buzz_hive_core::ports::code_host::{
    CheckState, CodeHost, HostError, MergeMethod, NewPullRequest,
};
use chrono::{TimeZone, Utc};
use support::github_replay::{Exchange, Replay};

const TOKEN: &str = "example-token";

fn repo() -> RepoRef {
    RepoRef::parse("example-org/api").unwrap()
}

fn open_issues() -> Exchange {
    Exchange::get("/repos/example-org/api/issues").query("state", "open")
}

async fn github(replay: &Replay) -> GitHub {
    GitHub::new(replay.url(), TOKEN).unwrap()
}

#[tokio::test]
async fn issues_are_listed_with_the_token_and_pull_requests_are_left_out() {
    let replay = Replay::start(vec![open_issues()
        .query("per_page", "100")
        .query("page", "1")
        .query("labels", "kiln")
        .query("assignee", "none")
        .header("etag", "\"v1\"")
        .fixture("issues_open.json")])
    .await;
    let filter = IssueFilter {
        state: IssueState::Open,
        label: Some("kiln".into()),
        unassigned: true,
    };
    let issues = github(&replay)
        .await
        .list_issues(&repo(), &filter, Page::first(100))
        .await
        .unwrap();
    let numbers: Vec<u64> = issues.iter().map(|i| i.number).collect();
    assert_eq!(
        numbers,
        [42, 43],
        "the pull request, number 44, is not an issue"
    );
    assert_eq!(issues[0].title, "Crash when saving a draft");
    assert_eq!(issues[0].labels, ["bug", "kiln"]);
    assert_eq!(issues[0].author, "octo");
    assert_eq!(issues[1].body, "", "a null body is an empty one");
    assert_eq!(issues[1].assignees, ["hubot"]);

    let sent = &replay.requests()[0];
    assert_eq!(sent.header("authorization"), Some("Bearer example-token"));
    assert_eq!(sent.header("accept"), Some("application/vnd.github+json"));
    assert!(sent.header("if-none-match").is_none());
    replay.finish();
}

#[tokio::test]
async fn a_second_read_asks_if_the_answer_changed_and_a_304_returns_the_first_answer() {
    let url = "/repos/example-org/api/issues";
    let replay = Replay::start(vec![
        Exchange::get(url)
            .header("etag", "\"v1\"")
            .fixture("issues_open.json"),
        Exchange::get(url).status(304),
    ])
    .await;
    let github = github(&replay).await;
    let first = github
        .list_issues(&repo(), &IssueFilter::default(), Page::first(100))
        .await
        .unwrap();
    let second = github
        .list_issues(&repo(), &IssueFilter::default(), Page::first(100))
        .await
        .unwrap();
    assert_eq!(first, second);
    let requests = replay.requests();
    assert_eq!(requests[1].header("if-none-match"), Some("\"v1\""));
    replay.finish();
}

#[tokio::test]
async fn an_answer_with_a_new_etag_replaces_the_one_kept() {
    let url = "/repos/example-org/api/issues";
    let replay = Replay::start(vec![
        Exchange::get(url).header("etag", "\"v1\"").body("[]"),
        Exchange::get(url)
            .header("etag", "\"v2\"")
            .fixture("issues_open.json"),
        Exchange::get(url).status(304),
    ])
    .await;
    let github = github(&replay).await;
    let filter = IssueFilter::default();
    assert!(github
        .list_issues(&repo(), &filter, Page::first(100))
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        github
            .list_issues(&repo(), &filter, Page::first(100))
            .await
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        github
            .list_issues(&repo(), &filter, Page::first(100))
            .await
            .unwrap()
            .len(),
        2
    );
    assert_eq!(replay.requests()[2].header("if-none-match"), Some("\"v2\""));
    replay.finish();
}

#[tokio::test]
async fn a_spent_limit_says_how_long_to_wait() {
    let reset = Utc::now().timestamp() + 120;
    let replay = Replay::start(vec![open_issues()
        .status(403)
        .header("x-ratelimit-remaining", "0")
        .header("x-ratelimit-reset", &reset.to_string())
        .body(r#"{"message":"API rate limit exceeded"}"#)])
    .await;
    let error = github(&replay)
        .await
        .list_issues(&repo(), &IssueFilter::default(), Page::first(100))
        .await
        .unwrap_err();
    let HostError::RateLimited { retry_after } = error else {
        panic!("expected a rate limit, got {error:?}");
    };
    assert!(
        (Duration::from_secs(100)..=Duration::from_secs(121)).contains(&retry_after),
        "{retry_after:?}"
    );
    replay.finish();
}

#[tokio::test]
async fn refusals_are_told_apart() {
    let replay = Replay::start(vec![
        open_issues()
            .status(401)
            .body(r#"{"message":"Bad credentials"}"#),
        open_issues().status(404).body(r#"{"message":"Not Found"}"#),
        open_issues().status(502).body("<html>bad gateway</html>"),
    ])
    .await;
    let github = github(&replay).await;
    let filter = IssueFilter::default();
    let first = github
        .list_issues(&repo(), &filter, Page::first(100))
        .await
        .unwrap_err();
    assert!(
        matches!(&first, HostError::Unauthorized(m) if m == "Bad credentials"),
        "{first:?}"
    );
    assert!(!format!("{first:?}").contains(TOKEN));
    let second = github
        .list_issues(&repo(), &filter, Page::first(100))
        .await
        .unwrap_err();
    assert!(matches!(second, HostError::NotFound(_)), "{second:?}");
    let third = github
        .list_issues(&repo(), &filter, Page::first(100))
        .await
        .unwrap_err();
    assert!(
        matches!(&third, HostError::Other(m) if m.contains("502")),
        "{third:?}"
    );
    replay.finish();
}

#[tokio::test]
async fn one_issue_is_read_with_its_etag_and_a_pull_request_is_not_found() {
    let replay = Replay::start(vec![
        Exchange::get("/repos/example-org/api/issues/42")
            .header("etag", "\"i42\"")
            .fixture("issue_42.json"),
        Exchange::get("/repos/example-org/api/issues/44")
            .body(r#"{"number":44,"state":"open","title":"x","pull_request":{}}"#),
    ])
    .await;
    let github = github(&replay).await;
    let issue = github.get_issue(&repo(), 42).await.unwrap();
    assert_eq!(issue.etag.as_deref(), Some("\"i42\""));
    assert_eq!(
        issue.url,
        "https://github.example.org/example-org/api/issues/42"
    );
    assert!(matches!(
        github.get_issue(&repo(), 44).await,
        Err(HostError::NotFound(_))
    ));
    replay.finish();
}

#[tokio::test]
async fn a_search_names_the_repo_of_each_hit() {
    let replay = Replay::start(vec![Exchange::get("/search/issues")
        .query(
            "q",
            "is:issue repo:example-org/web repo:example-org/api crash",
        )
        .query("per_page", "30")
        .query("page", "2")
        .fixture("search_issues.json")])
    .await;
    let hits = github(&replay)
        .await
        .search_issues(
            "is:issue repo:example-org/web repo:example-org/api crash",
            Page {
                number: 2,
                size: 30,
            },
        )
        .await
        .unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].repo.to_string(), "example-org/web");
    assert_eq!(hits[1].repo.to_string(), "example-org/api");
    assert_eq!(hits[1].state, IssueState::Closed);
    replay.finish();
}

#[tokio::test]
async fn comments_are_read_from_a_time_and_ones_beehive_wrote_are_known() {
    let since = Utc.with_ymd_and_hms(2026, 3, 1, 9, 0, 0).unwrap();
    let replay = Replay::start(vec![
        Exchange::get("/repos/example-org/api/issues/42/comments")
            .query("since", "2026-03-01T09:00:00Z")
            .fixture("comments.json"),
        // The marker is only believed from the token's own login.
        Exchange::get("/user").fixture("user.json").any_order(),
    ])
    .await;
    let comments = github(&replay)
        .await
        .list_comments(&repo(), 42, Some(since))
        .await
        .unwrap();
    assert_eq!(
        comments.iter().map(|c| c.id).collect::<Vec<_>>(),
        [9001, 9002, 9003]
    );
    assert_eq!(comments[1].author, "mallory");
    assert!(!comments[0].from_hive);
    assert!(comments[2].from_hive, "it carries Beehive's marker");
    replay.finish();
}

#[tokio::test]
async fn the_marker_in_someone_elses_comment_does_not_make_it_beehives() {
    let list = "/repos/example-org/api/issues/42/comments";
    let marker = "<!-- buzz-hive:card:example-org/api#42 -->";
    let forged = format!(
        r#"[{{"id":700,"body":"Looks fine.\n\n{marker}","created_at":"2026-03-01T10:00:00Z","user":{{"login":"mallory"}}}}]"#
    );
    let replay = Replay::start(vec![
        // Read as comments to hear: the forged one is a stranger's comment.
        Exchange::get(list).body(&forged),
        // Written to: Beehive's comment for the key is not there, so it posts its own.
        Exchange::get(list).body(&forged),
        Exchange::post(list).status(201).body(r#"{"id": 701}"#),
        Exchange::get("/user").fixture("user.json").any_order(),
    ])
    .await;
    let github = github(&replay).await;
    let comments = github.list_comments(&repo(), 42, None).await.unwrap();
    assert_eq!(comments.len(), 1);
    assert!(!comments[0].from_hive, "mallory is not the token's login");

    let id = github
        .comment(&repo(), 42, "Card made.", "card:example-org/api#42")
        .await
        .unwrap();
    assert_eq!(
        id, 701,
        "the copied marker did not stand in for Beehive's comment"
    );
    let posts = replay
        .requests()
        .into_iter()
        .filter(|r| r.method == "POST")
        .count();
    assert_eq!(posts, 1);
    // The login was read once for both calls.
    let logins = replay
        .requests()
        .into_iter()
        .filter(|r| r.path == "/user")
        .count();
    assert_eq!(logins, 1);
    replay.finish();
}

#[tokio::test]
async fn a_long_thread_is_read_page_by_page() {
    let page = |from: u64, count: u64| {
        let items: Vec<_> = (from..from + count)
            .map(|id| {
                serde_json::json!({
                    "id": id, "body": "x", "created_at": "2026-03-01T10:00:00Z",
                    "user": {"login": "octo"}
                })
            })
            .collect();
        serde_json::to_string(&items).unwrap()
    };
    let replay = Replay::start(vec![
        Exchange::get("/repos/example-org/api/issues/42/comments")
            .query("page", "1")
            .body(&page(1, 100)),
        Exchange::get("/repos/example-org/api/issues/42/comments")
            .query("page", "2")
            .body(&page(101, 3)),
    ])
    .await;
    let comments = github(&replay)
        .await
        .list_comments(&repo(), 42, None)
        .await
        .unwrap();
    assert_eq!(comments.len(), 103);
    replay.finish();
}

#[tokio::test]
async fn a_comment_is_written_once_for_a_key() {
    let list = "/repos/example-org/api/issues/42/comments";
    let replay = Replay::start(vec![
        // The first try: nothing carries the key yet, so the comment is written.
        Exchange::get(list).body("[]"),
        Exchange::post(list).status(201).body(r#"{"id": 555}"#),
        // A repeat after a failure: the comment is there, so nothing is written.
        Exchange::get(list).body(
            r#"[{"id":555,"body":"Card made.\n\n<!-- buzz-hive:card:example-org/api#42 -->","created_at":"2026-03-01T10:00:00Z","user":{"login":"hive-bot"}}]"#,
        ),
        Exchange::get("/user").fixture("user.json").any_order(),
    ])
    .await;
    let github = github(&replay).await;
    let id = github
        .comment(&repo(), 42, "Card made.", "card:example-org/api#42")
        .await
        .unwrap();
    assert_eq!(id, 555);
    let again = github
        .comment(&repo(), 42, "Card made.", "card:example-org/api#42")
        .await
        .unwrap();
    assert_eq!(again, 555);
    let posts: Vec<_> = replay
        .requests()
        .into_iter()
        .filter(|r| r.method == "POST")
        .collect();
    assert_eq!(posts.len(), 1);
    let sent: serde_json::Value = serde_json::from_str(&posts[0].body).unwrap();
    let body = sent["body"].as_str().unwrap();
    assert!(body.starts_with("Card made."));
    assert!(
        body.ends_with("<!-- buzz-hive:card:example-org/api#42 -->"),
        "{body}"
    );
    replay.finish();
}

const PULLS: &str = "/repos/example-org/api/pulls";

fn new_pull(title: &str, base: &str) -> NewPullRequest {
    NewPullRequest {
        repo: repo(),
        head: "buzz/8b93fa58-fix-login".into(),
        base: base.into(),
        title: title.into(),
        body: "Closes #7".into(),
        draft: true,
    }
}

fn pull_json(number: u64, draft: bool) -> String {
    format!(
        r#"{{"number":{number},"state":"open","draft":{draft},"html_url":"https://github.example.org/example-org/api/pull/{number}","head":{{"ref":"buzz/8b93fa58-fix-login"}},"base":{{"ref":"main"}}}}"#
    )
}

#[tokio::test]
async fn a_pull_request_is_opened_as_a_draft_from_the_card_branch() {
    let replay = Replay::start(vec![
        // Nothing is open for the branch yet.
        Exchange::get(PULLS)
            .query("state", "open")
            .query("head", "example-org:buzz/8b93fa58-fix-login")
            .body("[]"),
        Exchange::post(PULLS).status(201).body(&pull_json(12, true)),
    ])
    .await;
    let pull = github(&replay)
        .await
        .open_pull_request(&new_pull("Fix login", "main"), "kiln:c:pr:1")
        .await
        .unwrap();
    assert_eq!(pull.number, 12);
    assert!(pull.draft);
    assert_eq!(pull.state, "open");
    assert_eq!(pull.head, "buzz/8b93fa58-fix-login");
    assert_eq!(
        pull.url,
        "https://github.example.org/example-org/api/pull/12"
    );
    let posts: Vec<_> = replay
        .requests()
        .into_iter()
        .filter(|r| r.method == "POST")
        .collect();
    assert_eq!(posts.len(), 1);
    let sent: serde_json::Value = serde_json::from_str(&posts[0].body).unwrap();
    assert_eq!(
        sent,
        serde_json::json!({
            "title": "Fix login",
            "head": "buzz/8b93fa58-fix-login",
            "base": "main",
            "body": "Closes #7",
            "draft": true,
        })
    );
    replay.finish();
}

#[tokio::test]
async fn an_empty_base_is_the_repos_default_branch() {
    let replay = Replay::start(vec![
        Exchange::get(PULLS).query("state", "open").body("[]"),
        Exchange::get("/repos/example-org/api").body(r#"{"default_branch":"trunk"}"#),
        Exchange::post(PULLS).status(201).body(&pull_json(13, true)),
    ])
    .await;
    github(&replay)
        .await
        .open_pull_request(&new_pull("Fix login", ""), "kiln:c:pr:1")
        .await
        .unwrap();
    let post = replay
        .requests()
        .into_iter()
        .find(|r| r.method == "POST")
        .unwrap();
    let sent: serde_json::Value = serde_json::from_str(&post.body).unwrap();
    assert_eq!(sent["base"], "trunk");
    replay.finish();
}

#[tokio::test]
async fn a_pull_request_already_open_for_the_branch_is_returned_and_nothing_is_posted() {
    let replay = Replay::start(vec![Exchange::get(PULLS)
        .query("state", "open")
        .query("head", "example-org:buzz/8b93fa58-fix-login")
        .body(&format!("[{}]", pull_json(12, true)))])
    .await;
    let pull = github(&replay)
        .await
        .open_pull_request(&new_pull("Fix login", "main"), "kiln:c:pr:1")
        .await
        .unwrap();
    assert_eq!(pull.number, 12);
    assert!(replay.requests().iter().all(|r| r.method == "GET"));
    replay.finish();
}

#[tokio::test]
async fn two_calls_that_race_end_with_the_one_pull_request() {
    let replay = Replay::start(vec![
        Exchange::get(PULLS).query("state", "open").body("[]"),
        Exchange::post(PULLS).status(422).body(
            r#"{"message":"Validation Failed","errors":[{"message":"A pull request already exists for example-org:buzz/8b93fa58-fix-login."}]}"#,
        ),
        Exchange::get(PULLS)
            .query("state", "open")
            .body(&format!("[{}]", pull_json(12, true))),
    ])
    .await;
    let pull = github(&replay)
        .await
        .open_pull_request(&new_pull("Fix login", "main"), "kiln:c:pr:1")
        .await
        .unwrap();
    assert_eq!(pull.number, 12);
    replay.finish();
}

#[tokio::test]
async fn a_branch_that_was_never_pushed_is_rejected_with_githubs_reason() {
    let replay = Replay::start(vec![
        Exchange::get(PULLS).query("state", "open").body("[]"),
        Exchange::post(PULLS).status(422).body(
            r#"{"message":"Validation Failed","errors":[{"resource":"PullRequest","code":"invalid","field":"head","message":"head invalid"}]}"#,
        ),
    ])
    .await;
    let error = github(&replay)
        .await
        .open_pull_request(&new_pull("Fix login", "main"), "kiln:c:pr:1")
        .await
        .unwrap_err();
    match error {
        HostError::Rejected(message) => assert!(message.contains("head invalid"), "{message}"),
        other => panic!("{other:?}"),
    }
    replay.finish();
}

#[tokio::test]
async fn repos_are_listed_for_the_token_or_for_one_owner_and_owners_start_with_the_person() {
    let replay = Replay::start(vec![
        Exchange::get("/user/repos")
            .query("sort", "pushed")
            .fixture("repos.json")
            .any_order(),
        Exchange::get("/user").fixture("user.json").any_order(),
        Exchange::get("/user/orgs").fixture("orgs.json").any_order(),
    ])
    .await;
    let github = github(&replay).await;
    let all = github.list_repos("").await.unwrap();
    assert_eq!(all.len(), 4);
    let only: Vec<String> = github
        .list_repos("Example-Org")
        .await
        .unwrap()
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(only, ["example-org/api", "example-org/web"]);
    assert_eq!(
        github.list_owners().await.unwrap(),
        ["hive-bot", "example-org", "example-labs"]
    );
    replay.finish();
}

#[tokio::test]
async fn an_enterprise_address_with_a_path_keeps_it() {
    let replay = Replay::start(vec![Exchange::get(
        "/api/v3/repos/example-org/api/issues/42",
    )
    .fixture("issue_42.json")])
    .await;
    let github = GitHub::new(&format!("{}/api/v3/", replay.url()), TOKEN).unwrap();
    assert_eq!(github.get_issue(&repo(), 42).await.unwrap().number, 42);
    replay.finish();
}

const PULL_12: &str = "/repos/example-org/api/pulls/12";

fn merged_pull() -> &'static str {
    r#"{"number":12,"state":"closed","merged_at":"2026-03-01T10:00:00Z","head":{"ref":"b"},"base":{"ref":"main"}}"#
}

#[tokio::test]
async fn a_pull_request_is_read_with_its_labels_and_the_latest_verdict_of_each_reviewer() {
    let replay = Replay::start(vec![
        Exchange::get(PULL_12).fixture("pull_12.json"),
        Exchange::get("/repos/example-org/api/pulls/12/reviews").fixture("reviews_12.json"),
    ])
    .await;
    let status = github(&replay)
        .await
        .get_pull_request(&repo(), 12)
        .await
        .unwrap();
    assert_eq!(status.pull.state, "open");
    assert!(!status.pull.draft);
    assert_eq!(status.labels, ["ready", "kiln"]);
    // octo asked for changes and then approved; mona's approval was dismissed.
    assert!(status.pull.approved);
    replay.finish();
}

#[tokio::test]
async fn a_reviewer_who_still_asks_for_changes_blocks_approval_and_a_merged_pull_is_not_asked() {
    let replay = Replay::start(vec![
        Exchange::get(PULL_12).fixture("pull_12.json"),
        Exchange::get("/repos/example-org/api/pulls/12/reviews").body(
            r#"[{"user":{"login":"octo"},"state":"APPROVED"},{"user":{"login":"mona"},"state":"CHANGES_REQUESTED"}]"#,
        ),
        // A merged pull request's reviews are not read: nothing waits on them.
        Exchange::get(PULL_12).body(merged_pull()),
    ])
    .await;
    let github = github(&replay).await;
    assert!(
        !github
            .get_pull_request(&repo(), 12)
            .await
            .unwrap()
            .pull
            .approved
    );
    let merged = github.get_pull_request(&repo(), 12).await.unwrap();
    assert_eq!(merged.pull.state, "merged");
    assert!(!merged.pull.approved);
    replay.finish();
}

#[tokio::test]
async fn checks_come_from_check_runs_and_statuses_and_the_last_run_of_a_name_stands() {
    let replay = Replay::start(vec![
        Exchange::get(PULL_12).fixture("pull_12.json"),
        Exchange::get("/repos/example-org/api/commits/abc123/check-runs")
            .fixture("check_runs_abc123.json"),
        Exchange::get("/repos/example-org/api/commits/abc123/status").fixture("status_abc123.json"),
    ])
    .await;
    let read = github(&replay)
        .await
        .list_checks(&repo(), 12)
        .await
        .unwrap();
    assert_eq!(
        read.head_sha, "abc123",
        "the checks name the commit they are of"
    );
    let checks = read.checks;
    let seen: Vec<(&str, CheckState)> = checks.iter().map(|c| (c.name.as_str(), c.state)).collect();
    assert_eq!(
        seen,
        [
            ("lint", CheckState::Passed),
            ("docs", CheckState::Passed),
            ("e2e", CheckState::Pending),
            ("build", CheckState::Passed),
            ("ci/legacy", CheckState::Passed),
            ("ci/deploy", CheckState::Pending),
        ]
    );
    replay.finish();
}

#[tokio::test]
async fn a_draft_is_marked_ready_through_graphql_and_a_ready_one_is_left_alone() {
    let draft = r#"{"number":12,"state":"open","draft":true,"node_id":"PR_example12","head":{"ref":"b"},"base":{"ref":"main"}}"#;
    let replay = Replay::start(vec![
        Exchange::get(PULL_12).body(draft),
        Exchange::post("/graphql").body(
            r#"{"data":{"markPullRequestReadyForReview":{"pullRequest":{"isDraft":false}}}}"#,
        ),
        Exchange::get(PULL_12).fixture("pull_12.json"),
    ])
    .await;
    let github = github(&replay).await;
    github.mark_ready(&repo(), 12).await.unwrap();
    github.mark_ready(&repo(), 12).await.unwrap();
    let posts: Vec<_> = replay
        .requests()
        .into_iter()
        .filter(|r| r.method == "POST")
        .collect();
    assert_eq!(posts.len(), 1, "the ready pull request was not sent again");
    let sent: serde_json::Value = serde_json::from_str(&posts[0].body).unwrap();
    assert_eq!(sent["variables"]["id"], "PR_example12");
    assert!(sent["query"]
        .as_str()
        .unwrap()
        .contains("markPullRequestReadyForReview"));
    replay.finish();
}

#[tokio::test]
async fn graphql_refusing_a_ready_mark_is_a_rejection_and_an_enterprise_path_keeps_its_root() {
    let draft = r#"{"number":12,"state":"open","draft":true,"node_id":"PR_example12","head":{"ref":"b"},"base":{"ref":"main"}}"#;
    let replay = Replay::start(vec![
        Exchange::get("/api/v3/repos/example-org/api/pulls/12").body(draft),
        // GraphQL answers 200 even when it refuses.
        Exchange::post("/api/graphql").body(
            r#"{"errors":[{"message":"Pull request is in a state that cannot be marked ready"}]}"#,
        ),
    ])
    .await;
    let github = GitHub::new(&format!("{}/api/v3/", replay.url()), TOKEN).unwrap();
    match github.mark_ready(&repo(), 12).await.unwrap_err() {
        HostError::Rejected(message) => {
            assert!(message.contains("cannot be marked ready"), "{message}")
        }
        other => panic!("{other:?}"),
    }
    replay.finish();
}

#[tokio::test]
async fn a_merge_names_its_method_and_a_merged_pull_request_is_not_merged_again() {
    let merge = "/repos/example-org/api/pulls/12/merge";
    let replay = Replay::start(vec![
        Exchange::get(PULL_12).fixture("pull_12.json"),
        Exchange::put(merge).body(r#"{"merged":true}"#),
        Exchange::get(PULL_12).body(merged_pull()),
    ])
    .await;
    let github = github(&replay).await;
    github
        .merge(&repo(), 12, MergeMethod::Rebase, None)
        .await
        .unwrap();
    github
        .merge(&repo(), 12, MergeMethod::Rebase, None)
        .await
        .unwrap();
    let puts: Vec<_> = replay
        .requests()
        .into_iter()
        .filter(|r| r.method == "PUT")
        .collect();
    assert_eq!(puts.len(), 1);
    let sent: serde_json::Value = serde_json::from_str(&puts[0].body).unwrap();
    assert_eq!(sent, serde_json::json!({ "merge_method": "rebase" }));
    replay.finish();
}

#[tokio::test]
async fn a_merge_names_the_commit_it_was_checked_at_and_a_moved_head_is_refused() {
    let merge = "/repos/example-org/api/pulls/12/merge";
    let replay = Replay::start(vec![
        Exchange::get(PULL_12).fixture("pull_12.json"),
        Exchange::put(merge)
            .status(409)
            .body(r#"{"message":"Head branch was modified. Review and try the merge again."}"#),
    ])
    .await;
    match github(&replay)
        .await
        .merge(&repo(), 12, MergeMethod::Squash, Some("abc123"))
        .await
        .unwrap_err()
    {
        HostError::Rejected(message) => assert!(message.contains("Head branch"), "{message}"),
        other => panic!("{other:?}"),
    }
    let puts: Vec<_> = replay
        .requests()
        .into_iter()
        .filter(|r| r.method == "PUT")
        .collect();
    assert_eq!(puts.len(), 1);
    let sent: serde_json::Value = serde_json::from_str(&puts[0].body).unwrap();
    assert_eq!(
        sent,
        serde_json::json!({ "merge_method": "squash", "sha": "abc123" })
    );
    replay.finish();
}

#[tokio::test]
async fn a_merge_github_refuses_is_a_rejection_with_its_reason() {
    let merge = "/repos/example-org/api/pulls/12/merge";
    for status in [405, 409] {
        let replay = Replay::start(vec![
            Exchange::get(PULL_12).fixture("pull_12.json"),
            Exchange::put(merge)
                .status(status)
                .body(r#"{"message":"Pull Request is not mergeable"}"#),
        ])
        .await;
        match github(&replay)
            .await
            .merge(&repo(), 12, MergeMethod::Squash, None)
            .await
            .unwrap_err()
        {
            HostError::Rejected(message) => assert!(message.contains("not mergeable"), "{message}"),
            other => panic!("{other:?}"),
        }
        replay.finish();
    }
}

#[tokio::test]
#[should_panic(expected = "GET /repos/example-org/api/issues/99")]
async fn a_request_nobody_planned_fails_the_test_and_is_named() {
    let replay = Replay::start(vec![
        Exchange::get("/repos/example-org/api/issues/42").fixture("issue_42.json")
    ])
    .await;
    let github = github(&replay).await;
    github.get_issue(&repo(), 42).await.unwrap();
    // The adapter reports the 418 as an ordinary failure; the replay still remembers it.
    assert!(github.get_issue(&repo(), 99).await.is_err());
    replay.finish();
}

#[tokio::test]
#[should_panic(expected = "never asked for: GET /user/orgs")]
async fn an_exchange_nobody_asked_for_fails_the_test_too() {
    let replay = Replay::start(vec![Exchange::get("/user/orgs").fixture("orgs.json")]).await;
    replay.finish();
}

fn issue_json(number: u64, body: &str) -> String {
    serde_json::json!({
        "number": number,
        "title": "Export drops the last row",
        "body": body,
        "state": "open",
        "labels": [],
        "assignees": [],
        "user": {"login": "example-bot"},
        "html_url": format!("https://github.example.org/example-org/api/issues/{number}"),
    })
    .to_string()
}

#[tokio::test]
async fn an_issue_is_opened_with_a_marker_for_its_key() {
    let replay = Replay::start(vec![
        // Nothing carries the key yet.
        open_issues().body("[]"),
        Exchange::post("/repos/example-org/api/issues")
            .status(201)
            .body(&issue_json(9, "Off by one.")),
    ])
    .await;
    let made = github(&replay)
        .await
        .create_issue(
            &repo(),
            "Export drops the last row",
            "Off by one.",
            "ticket-proposal:4",
        )
        .await
        .unwrap();
    assert_eq!(made.number, 9);
    assert_eq!(made.repo, repo());
    let post = replay
        .requests()
        .into_iter()
        .find(|r| r.method == "POST")
        .unwrap();
    let sent: serde_json::Value = serde_json::from_str(&post.body).unwrap();
    assert_eq!(sent["title"], "Export drops the last row");
    let body = sent["body"].as_str().unwrap();
    assert!(body.starts_with("Off by one."), "{body}");
    assert!(
        body.contains("<!-- buzz-hive:ticket-proposal:4 -->"),
        "the key is in the body: {body}"
    );
    replay.finish();
}

#[tokio::test]
async fn a_repeated_key_finds_the_issue_it_opened_and_posts_nothing() {
    let marked = "Off by one.\n\n<!-- buzz-hive:ticket-proposal:4 -->";
    let replay = Replay::start(vec![
        open_issues().body(&format!("[{}]", issue_json(9, marked)))
    ])
    .await;
    let found = github(&replay)
        .await
        .create_issue(
            &repo(),
            "Export drops the last row",
            "Off by one.",
            "ticket-proposal:4",
        )
        .await
        .unwrap();
    assert_eq!(found.number, 9);
    assert!(replay.requests().iter().all(|r| r.method != "POST"));
    replay.finish();
}
