//! The `open_pull_request` action stage: the call it queues, the pull request the drain opens
//! through the code host, and how the stage reports the answer.

use buzz_hive_core::access::ActorKind;
use buzz_hive_core::domain::card::{card_branch, CardState};
use buzz_hive_core::domain::intake::{Issue, IssueState, RepoRef};
use buzz_hive_core::domain::run::{Run, RunState};
use buzz_hive_core::orchestrator::cards::{create_card, NewCard};
use buzz_hive_core::orchestrator::outbox::{drain_outbox_with, Actions};
use buzz_hive_core::orchestrator::run::{stage_done, start_run, stop_run};
use buzz_hive_core::orchestrator::settings::{set_setting, Scope};
use buzz_hive_core::orchestrator::{Ctx, Env};
use buzz_hive_core::ports::code_host::HostError;
use buzz_hive_core::ports::store::{CommunityId, Posting, Store};
use buzz_hive_core::testing::{FakeCodeHost, FakeDirectory, FakeRelay, MemStore};
use buzz_hive_core::workflow::load::{load, Catalog};
use buzz_hive_core::workflow::source::{MemSource, Source, Sources};
use serde_json::json;
use uuid::Uuid;

const COMMUNITY: CommunityId = CommunityId::from_u128(1);
const HIVE: &str = "ee";
const BUILDER: &str = "b1";
const CHANNEL: Uuid = Uuid::from_u128(77);

fn files() -> MemSource {
    let workflow = |name: &str, stages: &str| {
        (
            (
                format!("{name}/kiln-package.yaml"),
                format!("name: {name}\nversion: 1.0.0\n"),
            ),
            (
                format!("{name}/workflow.yaml"),
                format!("version: 2\nstages:\n{stages}"),
            ),
        )
    };
    let code = "  - id: code\n    kind: prompt\n    agent: builder\n    prompt: Fix {{issue.title}} on {{branch}}\n";
    let flows = [
        workflow(
            "ship",
            &format!(
                "{code}  - id: pr\n    kind: action\n    action: open_pull_request\n    with: {{ draft: \"true\", title: \"Fix {{{{issue.title}}}}\" }}\n  - id: after\n    kind: prompt\n    agent: builder\n    prompt: Wrap up\n"
            ),
        ),
        // The pull request goes back to code once, then ends the workflow.
        workflow(
            "again",
            &format!(
                "{code}  - id: pr\n    kind: action\n    action: open_pull_request\n    on:\n      failed: code\n    loop_limit: 1\n"
            ),
        ),
        workflow(
            "custom",
            &format!(
                "{code}  - id: pr\n    kind: action\n    action: open_pull_request\n    with:\n      base: release\n      draft: \"false\"\n      body: See {{{{issue.url}}}}\n"
            ),
        ),
        workflow(
            "clean",
            &format!(
                "{code}  - id: tidy\n    kind: action\n    action: cleanup_worktree\n  - id: after\n    kind: prompt\n    agent: builder\n    prompt: Wrap up\n"
            ),
        ),
        workflow(
            "artifact",
            &format!(
                "{code}  - id: pr\n    kind: action\n    action: open_pull_request\n    with: {{ body_artifact: summary }}\n"
            ),
        ),
        workflow(
            "badbase",
            &format!(
                "{code}  - id: pr\n    kind: action\n    action: open_pull_request\n    with: {{ base: \"bad base\" }}\n"
            ),
        ),
    ];
    let mut files = MemSource::new();
    for ((kp, kc), (wp, wc)) in flows {
        files = files.with(&kp, kc).with(&wp, wc);
    }
    files
}

fn catalog() -> Catalog {
    load(&[Source::new("test", false, files())])
}

struct World {
    store: MemStore,
    directory: FakeDirectory,
    catalog: Catalog,
    sources: Sources,
    relay: FakeRelay,
    host: FakeCodeHost,
}

impl World {
    fn new() -> Self {
        Self {
            store: MemStore::new(),
            directory: FakeDirectory::new()
                .with_named_agent("builder", BUILDER, "aa")
                .with_channel(CHANNEL, &[HIVE, BUILDER]),
            catalog: catalog(),
            sources: Sources(vec![Source::new("test", false, files())]),
            relay: FakeRelay::new(),
            host: FakeCodeHost::new(),
        }
    }

    fn env(&self) -> Env<'_> {
        Env {
            store: &self.store,
            directory: &self.directory,
            catalog: &self.catalog,
            hive: HIVE,
        }
    }

    async fn drain(&self) {
        self.drain_with(Some(&self.host)).await;
    }

    async fn drain_with(&self, host: Option<&FakeCodeHost>) {
        let (_stop, running) = tokio::sync::watch::channel(false);
        let actions = Actions {
            host: host.map(|h| h as &dyn buzz_hive_core::ports::code_host::CodeHost),
            catalog: &self.sources,
        };
        drain_outbox_with(
            &self.env(),
            COMMUNITY,
            &self.relay,
            Some(&actions),
            &[],
            &running,
        )
        .await
        .unwrap();
    }

    async fn run(&self, card: Uuid) -> Run {
        let mut tx = self.store.begin(COMMUNITY).await.unwrap();
        let runs = tx.runs().list(Some(card)).await.unwrap();
        runs.into_iter().last().unwrap()
    }

    /// The builder finishes the stage the card is at.
    async fn builder_done(&self, card: Uuid, result: &str) {
        let run = self.run(card).await;
        stage_done(
            &self.env(),
            &ctx(BUILDER, ActorKind::Agent),
            card,
            run.current_stage.as_deref().unwrap(),
            run.visit,
            result,
            "",
        )
        .await
        .unwrap();
        self.drain().await;
    }
}

fn ctx(pubkey: &str, kind: ActorKind) -> Ctx {
    Ctx {
        community: COMMUNITY,
        actor: buzz_hive_core::domain::events::Actor {
            pubkey: pubkey.into(),
            kind,
        },
        requester: None,
    }
}

fn person() -> Ctx {
    ctx("aa", ActorKind::Person)
}

fn issue() -> Issue {
    Issue {
        repo: RepoRef::parse("example-org/api").unwrap(),
        number: 7,
        title: "Button misaligned".into(),
        body: String::new(),
        labels: Vec::new(),
        assignees: Vec::new(),
        author: "octo".into(),
        state: IssueState::Open,
        url: "https://github.example.org/example-org/api/issues/7".into(),
        etag: None,
    }
}

/// A card on `workflow`, with the issue attached when `with_issue`, whose first stage has been
/// posted to the relay.
async fn started(workflow: &str, with_issue: bool) -> (World, Uuid) {
    let w = World::new();
    set_setting(
        &w.env(),
        &person(),
        &Scope::User,
        "update_channel",
        Some(json!(CHANNEL.to_string())),
    )
    .await
    .unwrap();
    let (card, _) = create_card(
        &w.store,
        &person(),
        NewCard {
            title: "Fix login".into(),
            description: "It breaks.".into(),
            workflow: workflow.into(),
        },
    )
    .await
    .unwrap();
    if with_issue {
        let issue = issue();
        let mut tx = w.store.begin(COMMUNITY).await.unwrap();
        tx.issues()
            .upsert(&issue, chrono::Utc::now())
            .await
            .unwrap();
        tx.issues()
            .attach(&issue.repo, issue.number, card.id, None)
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }
    start_run(&w.env(), &person(), card.id).await.unwrap();
    w.drain().await;
    (w, card.id)
}

async fn queued_pull_requests(w: &World, card: Uuid) -> Vec<(String, Posting)> {
    let mut tx = w.store.begin(COMMUNITY).await.unwrap();
    tx.outbox()
        .all()
        .await
        .unwrap()
        .into_iter()
        .filter(|row| row.card_id == card)
        .filter(|row| matches!(row.posting, Posting::PullRequest { .. }))
        .map(|row| (row.key, row.posting))
        .collect()
}

#[tokio::test]
async fn entering_the_stage_queues_one_pull_request_for_the_card_branch() {
    let (w, card) = started("ship", true).await;
    // Entering the stage queues the call, and the drain opens it in the same pass the
    // builder's report starts.
    let run = w.run(card).await;
    w.builder_done(card, "passed").await;
    let _ = run;

    let opened = w.host.pull_requests();
    assert_eq!(opened.len(), 1, "{:?}", w.host.calls());
    let (request, pull) = &opened[0];
    assert_eq!(request.repo.to_string(), "example-org/api");
    assert_eq!(request.head, card_branch(card, "Fix login"));
    // `issue.title` is the card's title.
    assert_eq!(request.title, "Fix Fix login");
    assert!(request.draft);
    assert!(request.body.ends_with("Closes #7"), "{}", request.body);
    assert!(pull.draft);

    // The stage passed and the card went on to the stage after it.
    let run = w.run(card).await;
    assert_eq!(run.current_stage.as_deref(), Some("after"));
    assert_eq!(run.state, RunState::Running);
    let mut tx = w.store.begin(COMMUNITY).await.unwrap();
    let recorded = tx.pull_requests().by_card(card).await.unwrap().unwrap();
    assert_eq!(recorded.number, pull.number);
    assert_eq!(recorded.head, card_branch(card, "Fix login"));
}

#[tokio::test]
async fn a_second_drain_does_not_open_a_second_pull_request() {
    let (w, card) = started("ship", true).await;
    w.builder_done(card, "passed").await;
    w.drain().await;
    w.drain().await;
    assert_eq!(w.host.pull_requests().len(), 1);
    assert_eq!(queued_pull_requests(&w, card).await.len(), 1);
}

#[tokio::test]
async fn a_replayed_row_finds_the_pull_request_the_first_call_opened() {
    // The host answered and the process died before the stage was told. The call is made
    // again with the same key, and the host returns the pull request it opened.
    let (w, card) = started("ship", true).await;
    w.builder_done(card, "passed").await;
    let (request, first) = w.host.pull_requests().remove(0);
    let key = {
        let mut tx = w.store.begin(COMMUNITY).await.unwrap();
        tx.outbox()
            .all()
            .await
            .unwrap()
            .into_iter()
            .find(|row| row.card_id == card && matches!(row.posting, Posting::PullRequest { .. }))
            .unwrap()
            .key
    };
    use buzz_hive_core::ports::code_host::CodeHost;
    let again = w.host.open_pull_request(&request, &key).await.unwrap();
    assert_eq!(again.number, first.number);
    assert_eq!(w.host.pull_requests().len(), 1);
}

#[tokio::test]
async fn a_card_without_an_issue_waits_at_the_stage() {
    let (w, card) = started("ship", false).await;
    w.builder_done(card, "passed").await;
    assert!(w.host.pull_requests().is_empty());
    let run = w.run(card).await;
    assert_eq!(run.current_stage.as_deref(), Some("pr"));
    assert_eq!(run.state, RunState::Waiting, "{run:?}");
}

#[tokio::test]
async fn the_with_arguments_set_base_body_and_draft() {
    let (w, card) = started("custom", true).await;
    w.builder_done(card, "passed").await;
    let (request, pull) = w.host.pull_requests().remove(0);
    assert_eq!(request.base, "release");
    assert!(!request.draft);
    assert!(!pull.draft);
    assert_eq!(
        request.body,
        "See https://github.example.org/example-org/api/issues/7\n\nCloses #7"
    );
    assert_eq!(request.title, "Fix login");
}

#[tokio::test]
async fn the_body_can_come_from_an_artifact_and_a_missing_one_is_refused() {
    let (w, card) = started("artifact", true).await;
    // No artifact has been saved, so the stage cannot be entered.
    let run = w.run(card).await;
    let result = stage_done(
        &w.env(),
        &ctx(BUILDER, ActorKind::Agent),
        card,
        "code",
        run.visit,
        "passed",
        "",
    )
    .await;
    assert!(result.is_err(), "{result:?}");
    assert!(w.host.pull_requests().is_empty());
}

#[tokio::test]
async fn a_base_that_is_not_a_branch_name_is_refused() {
    let (w, card) = started("badbase", true).await;
    let run = w.run(card).await;
    let result = stage_done(
        &w.env(),
        &ctx(BUILDER, ActorKind::Agent),
        card,
        "code",
        run.visit,
        "passed",
        "",
    )
    .await;
    assert!(result.is_err(), "{result:?}");
}

#[tokio::test]
async fn a_refused_request_fails_the_stage_with_the_reason() {
    let (w, card) = started("again", true).await;
    w.host
        .fail_next(HostError::Rejected("A pull request is not allowed".into()));
    w.builder_done(card, "passed").await;
    // The failure routes back to code, once.
    let run = w.run(card).await;
    assert_eq!(run.current_stage.as_deref(), Some("code"), "{run:?}");
    assert_eq!(run.visit, 2);
    let step = run.steps.iter().find(|s| s.stage == "pr").unwrap();
    assert_eq!(step.result, "failed");
    assert!(step.summary.contains("not allowed"));
}

#[tokio::test]
async fn a_second_visit_opens_its_own_call() {
    let (w, card) = started("again", true).await;
    w.host
        .fail_next(HostError::Unauthorized("bad token".into()));
    w.builder_done(card, "passed").await;
    // Back at code, then the pull request again.
    w.builder_done(card, "passed").await;
    assert_eq!(queued_pull_requests(&w, card).await.len(), 2);
    assert_eq!(w.host.pull_requests().len(), 1);
    let run = w.run(card).await;
    assert_eq!(run.state, RunState::Ended, "{run:?}");
}

#[tokio::test]
async fn a_rate_limited_host_leaves_the_call_queued_for_a_retry() {
    let (w, card) = started("ship", true).await;
    w.host.fail_next(HostError::RateLimited {
        retry_after: std::time::Duration::from_secs(30),
    });
    w.builder_done(card, "passed").await;
    assert!(w.host.pull_requests().is_empty());
    let run = w.run(card).await;
    assert_eq!(run.current_stage.as_deref(), Some("pr"));
    assert_eq!(run.state, RunState::Running);
    let mut tx = w.store.begin(COMMUNITY).await.unwrap();
    let row = tx
        .outbox()
        .all()
        .await
        .unwrap()
        .into_iter()
        .find(|row| row.card_id == card && matches!(row.posting, Posting::PullRequest { .. }))
        .unwrap();
    assert!(row.last_error.is_some());
}

#[tokio::test]
async fn no_code_host_fails_the_stage_and_says_why() {
    let (w, card) = started("again", true).await;
    let run = w.run(card).await;
    stage_done(
        &w.env(),
        &ctx(BUILDER, ActorKind::Agent),
        card,
        "code",
        run.visit,
        "passed",
        "",
    )
    .await
    .unwrap();
    w.drain_with(None).await;
    let run = w.run(card).await;
    let step = run.steps.iter().find(|s| s.stage == "pr").unwrap();
    assert_eq!(step.result, "failed");
    assert!(step.summary.contains("GH_TOKEN"));
}

#[tokio::test]
async fn a_stopped_run_does_not_open_the_pull_request() {
    let (w, card) = started("ship", true).await;
    let run = w.run(card).await;
    // The stage is entered and its call is queued, but nothing has drained it yet.
    stage_done(
        &w.env(),
        &ctx(BUILDER, ActorKind::Agent),
        card,
        "code",
        run.visit,
        "passed",
        "",
    )
    .await
    .unwrap();
    stop_run(&w.env(), &person(), run.id).await.unwrap();
    w.drain().await;
    assert!(w.host.pull_requests().is_empty(), "{:?}", w.host.calls());
}

#[tokio::test]
async fn cleanup_worktree_passes_straight_through() {
    let (w, card) = started("clean", true).await;
    w.builder_done(card, "passed").await;
    let run = w.run(card).await;
    assert_eq!(run.current_stage.as_deref(), Some("after"));
    assert!(w.host.pull_requests().is_empty());
    assert_eq!(card_state(&w, card).await, CardState::Running);
}

async fn card_state(w: &World, card: Uuid) -> CardState {
    let mut tx = w.store.begin(COMMUNITY).await.unwrap();
    tx.cards().get(card).await.unwrap().unwrap().state
}

#[tokio::test]
async fn the_task_message_names_the_branch_and_asks_for_a_push() {
    let (w, _card) = started("ship", true).await;
    let posts = w.relay.posts();
    let task = posts
        .iter()
        .find(|p| p.text.contains("Fix Fix login"))
        .unwrap_or_else(|| panic!("{posts:?}"));
    let branch = card_branch(_card, "Fix login");
    assert!(task.text.contains(&format!("on {branch}")), "{}", task.text);
    assert!(
        task.text.contains(&format!("push the branch `{branch}`")),
        "{}",
        task.text
    );
}

#[tokio::test]
async fn a_card_without_an_issue_is_not_asked_to_push() {
    let (w, _card) = started("ship", false).await;
    let posts = w.relay.posts();
    assert!(posts.iter().all(|p| !p.text.contains("push the branch")));
}
