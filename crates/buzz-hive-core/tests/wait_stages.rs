//! Stages that wait on outside state, the actions that act on a pull request, the pull request
//! sync that decides them, and the merge a person asks for. Everything runs on the fakes, so no
//! real GitHub is called.

use std::sync::Arc;
use std::time::Duration;

use buzz_hive_core::access::ActorKind;
use buzz_hive_core::background::pull_requests::{sync_card, watched, Synced};
use buzz_hive_core::domain::card::PullRequest;
use buzz_hive_core::domain::intake::{Issue, IssueState, RepoRef};
use buzz_hive_core::domain::run::{Run, RunState};
use buzz_hive_core::orchestrator::cards::{create_card, NewCard};
use buzz_hive_core::orchestrator::intake::IntakeError;
use buzz_hive_core::orchestrator::merge::{merge_card, set_merge_order};
use buzz_hive_core::orchestrator::outbox::{drain_outbox_with, Actions};
use buzz_hive_core::orchestrator::run::{
    pause_card, resume_card, skip_stage, stage_done, start_run,
};
use buzz_hive_core::orchestrator::settings::{set_setting, Scope};
use buzz_hive_core::orchestrator::{timers, Ctx, Env, OpError};
use buzz_hive_core::ports::code_host::{Check, CheckState, CodeHost, HostError, MergeMethod};
use buzz_hive_core::ports::store::{CommunityId, PullCall, Store};
use buzz_hive_core::testing::{FakeCodeHost, FakeDirectory, FakeRelay, ManualClock, MemStore};
use buzz_hive_core::workflow::load::{load, Catalog};
use buzz_hive_core::workflow::source::{MemSource, Source, Sources};
use chrono::Utc;
use serde_json::json;
use uuid::Uuid;

const COMMUNITY: CommunityId = CommunityId::from_u128(1);
const HIVE: &str = "ee";
const BUILDER: &str = "b1";
const CHANNEL: Uuid = Uuid::from_u128(77);

fn files() -> MemSource {
    let code =
        "  - id: code\n    kind: prompt\n    agent: builder\n    prompt: Fix {{issue.title}}\n";
    let pr = "  - id: pr\n    kind: action\n    action: open_pull_request\n";
    let after = "  - id: after\n    kind: prompt\n    agent: builder\n    prompt: Wrap up\n";
    let flows = [
        // CI decides: green goes on to ready and then waits for the merge, red goes back to code.
        (
            "ci",
            format!(
                "{code}{pr}  - id: validate\n    kind: action\n    action: wait_for_ci\n    with: {{ required: \"build, lint\" }}\n    on: {{ passed: ready, failed: code }}\n    loop_limit: 2\n  - id: ready\n    kind: action\n    action: mark_ready\n  - id: merged\n    kind: wait\n    wait: {{ pull_request: merged, poll: 30s, timeout: 2h }}\n{after}"
            ),
        ),
        // Every check counts, and the grace and timeout are the defaults.
        (
            "ciall",
            format!(
                "{code}{pr}  - id: validate\n    kind: action\n    action: wait_for_ci\n    on: {{ passed: after, failed: code }}\n    loop_limit: 2\n{after}"
            ),
        ),
        (
            "citimeout",
            format!(
                "{code}{pr}  - id: validate\n    kind: action\n    action: wait_for_ci\n    with: {{ timeout: 10m, poll: 30s }}\n    on: {{ passed: after }}\n{after}"
            ),
        ),
        // A label wait on a card that has an issue and no pull request.
        (
            "labels",
            format!(
                "{code}  - id: gate\n    kind: wait\n    wait: {{ labels: [approved], poll: 30s }}\n{after}"
            ),
        ),
        // The tail of the built-in workflow: ready, then a held cleanup the merge starts.
        (
            "land",
            format!(
                "{code}{pr}  - id: ready\n    kind: action\n    action: mark_ready\n  - id: cleanup\n    pause: true\n    kind: action\n    action: cleanup_worktree\n{after}"
            ),
        ),
        // The workflow merges for itself.
        (
            "selfmerge",
            format!(
                "{code}{pr}  - id: ready\n    kind: action\n    action: mark_ready\n  - id: land\n    kind: action\n    action: merge\n    with: {{ method: rebase }}\n{after}"
            ),
        ),
        // Green checks go on to ready and then to the merge the workflow makes.
        (
            "cimerge",
            format!(
                "{code}{pr}  - id: validate\n    kind: action\n    action: wait_for_ci\n    on: {{ passed: ready, failed: code }}\n    loop_limit: 2\n  - id: ready\n    kind: action\n    action: mark_ready\n  - id: land\n    kind: action\n    action: merge\n{after}"
            ),
        ),
        // A wait on the pull request with no route for a failure.
        (
            "approval",
            format!(
                "{code}{pr}  - id: review\n    kind: wait\n    wait: {{ pull_request: approved, poll: 30s }}\n{after}"
            ),
        ),
        // No pull request is ever opened, so the stages that act on one have nothing to act on.
        (
            "nopr",
            format!("{code}  - id: ready\n    kind: action\n    action: mark_ready\n{after}"),
        ),
    ];
    let mut files = MemSource::new();
    for (name, stages) in flows {
        files = files
            .with(
                &format!("{name}/kiln-package.yaml"),
                format!("name: {name}\nversion: 1.0.0\n"),
            )
            .with(
                &format!("{name}/workflow.yaml"),
                format!("version: 2\nstages:\n{stages}"),
            );
    }
    files
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
        Self::on(MemStore::new())
    }

    fn timed() -> (Self, ManualClock) {
        let clock = ManualClock::new();
        (
            Self::on(MemStore::with_clock(Arc::new(clock.clone()))),
            clock,
        )
    }

    fn on(store: MemStore) -> Self {
        Self {
            store,
            directory: FakeDirectory::new()
                .with_named_agent("builder", BUILDER, "aa")
                .with_channel(CHANNEL, &[HIVE, BUILDER]),
            catalog: load(&[Source::new("test", false, files())]),
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

    /// Drains the outbox until a pass finds nothing more: a settled call may queue the next.
    async fn drain(&self) {
        for _ in 0..4 {
            let (_stop, running) = tokio::sync::watch::channel(false);
            let actions = Actions {
                host: Some(&self.host as &dyn CodeHost),
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
    }

    async fn run(&self, card: Uuid) -> Run {
        let mut tx = self.store.begin(COMMUNITY).await.unwrap();
        tx.runs()
            .list(Some(card))
            .await
            .unwrap()
            .into_iter()
            .last()
            .unwrap()
    }

    async fn stage(&self, card: Uuid) -> String {
        self.run(card).await.current_stage.unwrap_or_default()
    }

    async fn stored_pull(&self, card: Uuid) -> PullRequest {
        let mut tx = self.store.begin(COMMUNITY).await.unwrap();
        tx.pull_requests().by_card(card).await.unwrap().unwrap()
    }

    async fn builder_done(&self, card: Uuid) {
        let run = self.run(card).await;
        stage_done(
            &self.env(),
            &ctx(BUILDER, ActorKind::Agent),
            card,
            run.current_stage.as_deref().unwrap(),
            run.visit,
            "passed",
            "",
        )
        .await
        .unwrap();
        self.drain().await;
    }

    /// Reads the pull request as the sync loop does, `later` after now, and drains what it queued.
    async fn sync(&self, card: Uuid, later: Duration) -> Synced {
        let now = Utc::now() + chrono::Duration::from_std(later).unwrap();
        let synced = sync_card(&self.env(), &self.host, COMMUNITY, card, now)
            .await
            .unwrap();
        self.drain().await;
        synced
    }

    fn number(&self) -> u64 {
        self.host.pull_requests()[0].1.number
    }

    fn checks(&self, checks: &[(&str, CheckState)]) {
        self.host.set_checks(
            &repo(),
            self.number(),
            checks
                .iter()
                .map(|(name, state)| Check {
                    name: (*name).into(),
                    state: *state,
                })
                .collect(),
        );
    }
}

fn repo() -> RepoRef {
    RepoRef::parse("example-org/api").unwrap()
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

fn issue(labels: &[&str]) -> Issue {
    Issue {
        repo: repo(),
        number: 7,
        title: "Button misaligned".into(),
        body: String::new(),
        labels: labels.iter().map(|l| (*l).to_owned()).collect(),
        assignees: Vec::new(),
        author: "octo".into(),
        state: IssueState::Open,
        url: "https://github.example.org/example-org/api/issues/7".into(),
        etag: None,
    }
}

async fn started_on(w: &World, workflow: &str) -> Uuid {
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
    let issue = issue(&[]);
    let mut tx = w.store.begin(COMMUNITY).await.unwrap();
    tx.issues().upsert(&issue, Utc::now()).await.unwrap();
    tx.issues()
        .attach(&issue.repo, issue.number, card.id, None)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    start_run(&w.env(), &person(), card.id).await.unwrap();
    w.drain().await;
    card.id
}

/// A card on `workflow` whose pull request is open: the builder has reported and the pull request
/// stage has run, so the card is at the stage after it.
async fn with_pull_request(workflow: &str) -> (World, Uuid) {
    let w = World::new();
    let card = started_on(&w, workflow).await;
    w.builder_done(card).await;
    assert_eq!(w.host.pull_requests().len(), 1, "{:?}", w.host.calls());
    (w, card)
}

const MINUTE: Duration = Duration::from_secs(60);

fn passed(name: &str) -> (&str, CheckState) {
    (name, CheckState::Passed)
}

#[tokio::test]
async fn ci_waits_for_unfinished_checks_then_passes_and_the_next_stage_marks_ready() {
    let (w, card) = with_pull_request("ci").await;
    assert_eq!(w.stage(card).await, "validate");
    assert_eq!(w.run(card).await.state, RunState::Running);
    // The step says what the stage waits for, for the card page.
    let step = w.run(card).await.steps.pop().unwrap();
    assert_eq!(step.summary, "waiting for the checks build, lint to pass");

    w.checks(&[passed("build"), ("lint", CheckState::Pending)]);
    let synced = w.sync(card, MINUTE).await;
    assert!(!synced.moved);
    assert_eq!(w.stage(card).await, "validate", "a check is still running");
    assert_eq!(w.stored_pull(card).await.checks, "pending");

    w.checks(&[
        passed("build"),
        passed("lint"),
        ("docs", CheckState::Failed),
    ]);
    let synced = w.sync(card, MINUTE).await;
    assert!(synced.moved);
    // Only the required checks decide: docs is red and does not count. The pull request was a
    // draft, so mark_ready took it out of draft and the card now waits for the merge.
    assert_eq!(w.stage(card).await, "merged");
    assert!(w
        .host
        .calls()
        .contains(&format!("mark_ready example-org/api#{}", w.number())));
    assert!(!w.host.pull_requests()[0].1.draft);
}

#[tokio::test]
async fn a_failed_required_check_sends_the_card_back_with_the_check_named() {
    let (w, card) = with_pull_request("ci").await;
    w.checks(&[("build", CheckState::Failed), passed("lint")]);
    w.sync(card, MINUTE).await;
    let run = w.run(card).await;
    assert_eq!(run.current_stage.as_deref(), Some("code"), "{run:?}");
    assert_eq!(run.visit, 2);
    let step = run.steps.iter().find(|s| s.stage == "validate").unwrap();
    assert_eq!(step.result, "failed");
    assert_eq!(step.summary, "the check build failed");
}

#[tokio::test]
async fn a_required_check_that_never_reports_fails_after_the_grace() {
    let (w, card) = with_pull_request("ci").await;
    w.checks(&[passed("build")]);
    w.sync(card, MINUTE).await;
    assert_eq!(w.stage(card).await, "validate", "lint may still appear");
    w.sync(card, 3 * MINUTE).await;
    let run = w.run(card).await;
    let step = run.steps.iter().find(|s| s.stage == "validate").unwrap();
    assert_eq!(step.summary, "the required checks lint never reported");
}

#[tokio::test]
async fn a_repo_with_no_checks_passes_once_the_grace_is_over() {
    let (w, card) = with_pull_request("ciall").await;
    w.sync(card, MINUTE).await;
    assert_eq!(w.stage(card).await, "validate", "checks may still appear");
    w.sync(card, 3 * MINUTE).await;
    assert_eq!(w.stage(card).await, "after");
}

#[tokio::test]
async fn ci_fails_at_its_timeout_and_not_before() {
    let (w, clock) = World::timed();
    let card = started_on(&w, "citimeout").await;
    w.builder_done(card).await;
    // Checks keep running, so only the clock can end the stage.
    w.checks(&[("build", CheckState::Pending)]);
    w.sync(card, MINUTE).await;
    assert_eq!(
        timers::next_in(&w.store, COMMUNITY).await.unwrap(),
        Some(10 * MINUTE)
    );

    clock.advance(10 * MINUTE - Duration::from_secs(1));
    assert!(timers::due(&w.store, COMMUNITY, 10)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(w.stage(card).await, "validate");

    clock.advance(Duration::from_secs(1));
    for timer in timers::due(&w.store, COMMUNITY, 10).await.unwrap() {
        timers::fire(&w.env(), COMMUNITY, &timer).await.unwrap();
    }
    let run = w.run(card).await;
    let step = run.steps.iter().find(|s| s.stage == "validate").unwrap();
    assert_eq!(step.result, "failed");
    assert_eq!(step.summary, "validate timed out after 10m");
    // The stage had no route for a failure, so the card waits for a person instead of going on.
    assert_ne!(run.current_stage.as_deref(), Some("after"));
}

#[tokio::test]
async fn a_stage_reports_how_often_the_loop_should_read_the_card() {
    let (w, card) = with_pull_request("ci").await;
    w.checks(&[("build", CheckState::Pending)]);
    // `ci` names no poll, so the default of a minute applies; `merged` names 30 seconds.
    assert_eq!(w.sync(card, Duration::ZERO).await.poll, MINUTE);
    w.checks(&[passed("build"), passed("lint")]);
    w.sync(card, MINUTE).await;
    assert_eq!(w.stage(card).await, "merged");
    assert_eq!(
        w.sync(card, Duration::ZERO).await.poll,
        Duration::from_secs(30)
    );
}

#[tokio::test]
async fn a_closed_pull_request_fails_a_wait_stage() {
    let (w, card) = with_pull_request("approval").await;
    assert_eq!(w.stage(card).await, "review");
    w.host
        .edit_pull(&repo(), w.number(), |pull| pull.state = "closed".into());
    w.sync(card, MINUTE).await;
    let run = w.run(card).await;
    let step = run.steps.iter().find(|s| s.stage == "review").unwrap();
    assert_eq!(step.result, "failed");
    assert_eq!(step.summary, "the pull request was closed");
    assert_ne!(run.current_stage.as_deref(), Some("after"));
}

#[tokio::test]
async fn an_approval_passes_the_stage_and_a_vanished_pull_request_fails_it() {
    let (w, card) = with_pull_request("approval").await;
    w.sync(card, MINUTE).await;
    assert_eq!(w.stage(card).await, "review", "nobody has approved yet");
    w.host
        .edit_pull(&repo(), w.number(), |pull| pull.approved = true);
    w.sync(card, MINUTE).await;
    assert_eq!(w.stage(card).await, "after");

    let (w, card) = with_pull_request("approval").await;
    w.host
        .fail_next(HostError::NotFound("no such pull request".into()));
    w.sync(card, MINUTE).await;
    let run = w.run(card).await;
    let step = run.steps.iter().find(|s| s.stage == "review").unwrap();
    assert_eq!(step.summary, "the pull request is gone from the host");
}

#[tokio::test]
async fn a_pull_request_merged_by_hand_passes_the_wait_for_the_merge() {
    let (w, card) = with_pull_request("ci").await;
    w.checks(&[passed("build"), passed("lint")]);
    w.sync(card, MINUTE).await;
    assert_eq!(w.stage(card).await, "merged");
    w.host
        .edit_pull(&repo(), w.number(), |pull| pull.state = "merged".into());
    w.sync(card, MINUTE).await;
    assert_eq!(w.stage(card).await, "after");
    assert_eq!(w.stored_pull(card).await.state, "merged");
    // Nothing waits on the card now, and its pull request is not open: it is read no more.
    assert!(watched(&w.env(), COMMUNITY).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_label_wait_works_with_no_pull_request_and_reads_the_issue_labels() {
    let w = World::new();
    let card = started_on(&w, "labels").await;
    w.builder_done(card).await;
    assert_eq!(w.stage(card).await, "gate");
    assert_eq!(watched(&w.env(), COMMUNITY).await.unwrap(), [card]);

    w.sync(card, MINUTE).await;
    assert_eq!(w.stage(card).await, "gate");

    // Intake brings the label in on its next read of the issue.
    let labelled = issue(&["approved"]);
    let mut tx = w.store.begin(COMMUNITY).await.unwrap();
    tx.issues().upsert(&labelled, Utc::now()).await.unwrap();
    tx.commit().await.unwrap();
    w.sync(card, MINUTE).await;
    assert_eq!(w.stage(card).await, "after");
}

#[tokio::test]
async fn the_sync_logs_a_change_once_and_watches_only_cards_that_need_it() {
    let (w, card) = with_pull_request("ci").await;
    assert_eq!(watched(&w.env(), COMMUNITY).await.unwrap(), [card]);

    w.checks(&[("build", CheckState::Pending)]);
    let first = w.sync(card, Duration::ZERO).await;
    let changed: Vec<_> = first
        .events
        .iter()
        .filter(|e| e.kind == "pull_request.changed")
        .collect();
    assert_eq!(changed.len(), 1);
    assert_eq!(changed[0].payload["checks"], "pending");
    // The same answer again changes nothing and logs nothing.
    let second = w.sync(card, Duration::ZERO).await;
    assert!(second.events.is_empty(), "{:?}", second.events);
}

#[tokio::test]
async fn a_stage_that_acts_on_a_pull_request_the_card_lacks_waits_for_a_person() {
    let w = World::new();
    let card = started_on(&w, "nopr").await;
    w.builder_done(card).await;
    let run = w.run(card).await;
    assert_eq!(run.current_stage.as_deref(), Some("ready"));
    assert_eq!(run.state, RunState::Waiting);
    assert!(w.host.calls().iter().all(|c| !c.starts_with("mark_ready")));
}

#[tokio::test]
async fn a_workflows_merge_stage_merges_with_its_method_and_the_stage_passes() {
    let (w, card) = with_pull_request("selfmerge").await;
    assert_eq!(w.stage(card).await, "after", "{:?}", w.host.calls());
    let calls = w.host.calls();
    assert!(
        calls.contains(&format!("merge example-org/api#{} rebase", w.number())),
        "{calls:?}"
    );
    assert_eq!(w.stored_pull(card).await.state, "merged");
}

#[tokio::test]
async fn a_workflows_merge_is_refused_when_the_branch_moved_after_its_checks_were_read() {
    let (w, card) = with_pull_request("cimerge").await;
    w.checks(&[passed("build")]);
    // The sync reads green checks of the first head and queues the call that takes the pull
    // request out of draft, which has not run.
    sync_card(&w.env(), &w.host, COMMUNITY, card, Utc::now() + MINUTE)
        .await
        .unwrap();
    assert_eq!(w.stage(card).await, "ready");
    assert_eq!(
        w.stored_pull(card).await.head_sha,
        w.host.head_of(&repo(), w.number())
    );
    // A push lands before the merge is queued and made.
    w.host.push_to_pull(&repo(), w.number());
    w.drain().await;

    // The host refused the merge, so nothing merged and the stage says why.
    assert_eq!(w.host.pull_requests()[0].1.state, "open");
    let run = w.run(card).await;
    let failed: Vec<_> = run.steps.iter().filter(|s| s.result == "failed").collect();
    assert_eq!(failed.len(), 1, "{run:?}");
    assert!(failed[0].summary.contains("modified"), "{failed:?}");
    assert_eq!(w.stored_pull(card).await.state, "open");
}

#[tokio::test]
async fn a_workflows_merge_of_the_commit_whose_checks_were_read_goes_through() {
    let (w, card) = with_pull_request("cimerge").await;
    w.checks(&[passed("build")]);
    w.sync(card, MINUTE).await;
    assert_eq!(w.host.pull_requests()[0].1.state, "merged");
    assert_eq!(w.stage(card).await, "after");
}

#[test]
fn a_merge_row_queued_before_the_commit_was_stored_reads_back_unpinned() {
    // The shape of a row written before the field existed.
    let old: PullCall =
        serde_json::from_value(json!({ "call": "merge", "method": "squash" })).unwrap();
    assert_eq!(
        old,
        PullCall::Merge {
            method: MergeMethod::Squash,
            head_sha: None
        }
    );
    // Without a sha the row stays as it was, so a queued row reads back the same.
    assert_eq!(
        serde_json::to_value(&old).unwrap(),
        json!({ "call": "merge", "method": "squash" })
    );
}

#[tokio::test]
async fn a_merge_the_host_refuses_fails_the_stage_with_the_reason() {
    let w = World::new();
    let card = started_on(&w, "selfmerge").await;
    // The pull request stage and mark_ready go through; the merge is refused.
    w.host
        .fail_next(HostError::Rejected("not mergeable".into()));
    w.builder_done(card).await;
    // The first queued failure is consumed by whichever host call comes first, so the stage
    // that failed is the one that named it.
    let run = w.run(card).await;
    let failed: Vec<_> = run.steps.iter().filter(|s| s.result == "failed").collect();
    assert_eq!(failed.len(), 1, "{run:?}");
    assert!(failed[0].summary.contains("not mergeable"), "{failed:?}");
}

// The merge a person asks for, ported from the Go engine's merge specs.

/// A card at the held `cleanup` stage of `land`, whose pull request is ready and green.
async fn held_at_cleanup() -> (World, Uuid) {
    let (w, card) = with_pull_request("land").await;
    assert_eq!(w.stage(card).await, "cleanup", "{:?}", w.host.calls());
    assert_eq!(w.run(card).await.state, RunState::Paused);
    w.checks(&[passed("build")]);
    (w, card)
}

async fn merge(w: &World, ctx: &Ctx, card: Uuid) -> Result<(), IntakeError> {
    merge_card(&w.env(), ctx, &w.host, card, None)
        .await
        .map(|_| ())
}

fn refused(result: Result<(), IntakeError>) -> String {
    match result.unwrap_err() {
        IntakeError::Op(OpError::FailedPrecondition(message)) => message,
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn merging_squashes_the_pull_request_and_starts_the_stage_the_workflow_held() {
    let (w, card) = held_at_cleanup().await;
    merge(&w, &person(), card).await.unwrap();
    w.drain().await;
    let calls = w.host.calls();
    assert!(
        calls.contains(&format!("merge example-org/api#{} squash", w.number())),
        "{calls:?}"
    );
    assert_eq!(w.stored_pull(card).await.state, "merged");
    // Cleanup passed straight through and the card went on.
    assert_eq!(w.stage(card).await, "after");
    assert_eq!(w.run(card).await.state, RunState::Running);
}

#[tokio::test]
async fn merging_again_after_the_merge_merges_nothing_twice() {
    let (w, card) = held_at_cleanup().await;
    merge(&w, &person(), card).await.unwrap();
    merge(&w, &person(), card).await.unwrap();
    let merges = w
        .host
        .calls()
        .iter()
        .filter(|c| c.starts_with("merge "))
        .count();
    assert_eq!(merges, 1);
}

#[tokio::test]
async fn only_a_person_may_merge_and_set_the_merge_order() {
    let (w, card) = held_at_cleanup().await;
    let agent = ctx(BUILDER, ActorKind::Agent);
    match merge(&w, &agent, card).await.unwrap_err() {
        IntakeError::Op(OpError::PermissionDenied(message)) => {
            assert!(
                message.contains("only a person may merge a card"),
                "{message}"
            )
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        set_merge_order(&w.env(), &agent, card, &["example-org/api".into()]).await,
        Err(OpError::PermissionDenied(_))
    ));
    assert!(w.host.calls().iter().all(|c| !c.starts_with("merge ")));
}

#[tokio::test]
async fn nothing_is_merged_while_checks_fail_or_have_not_finished() {
    let (w, card) = held_at_cleanup().await;
    w.checks(&[("build", CheckState::Failed)]);
    assert_eq!(
        refused(merge(&w, &person(), card).await),
        "the pull request has failing checks"
    );
    w.checks(&[passed("build"), ("lint", CheckState::Pending)]);
    assert_eq!(
        refused(merge(&w, &person(), card).await),
        "the pull request's checks have not finished"
    );
    assert!(w.host.calls().iter().all(|c| !c.starts_with("merge ")));
    assert_eq!(w.stage(card).await, "cleanup");
}

#[tokio::test]
async fn a_draft_a_closed_pull_request_and_one_the_card_lacks_are_refused() {
    let (w, card) = held_at_cleanup().await;
    w.host
        .edit_pull(&repo(), w.number(), |pull| pull.draft = true);
    assert!(refused(merge(&w, &person(), card).await).contains("draft"));
    w.host.edit_pull(&repo(), w.number(), |pull| {
        pull.draft = false;
        pull.state = "closed".into();
    });
    assert_eq!(
        refused(merge(&w, &person(), card).await),
        "the pull request is closed"
    );

    let w = World::new();
    let card = started_on(&w, "nopr").await;
    assert_eq!(
        refused(merge(&w, &person(), card).await),
        "the card has no pull request"
    );
}

#[tokio::test]
async fn a_card_set_to_not_merge_is_refused() {
    let (w, card) = held_at_cleanup().await;
    set_setting(
        &w.env(),
        &person(),
        &Scope::Card(card),
        "card.dont_merge",
        Some(json!(true)),
    )
    .await
    .unwrap();
    assert!(refused(merge(&w, &person(), card).await).contains("card.dont_merge"));
    assert!(w.host.calls().iter().all(|c| !c.starts_with("merge ")));
}

#[tokio::test]
async fn a_stage_that_is_working_blocks_the_merge_and_a_waiting_one_does_not() {
    let (w, card) = with_pull_request("ci").await;
    w.checks(&[passed("build"), passed("lint")]);
    // The card waits for the merge at `merged`: a person may merge it now.
    w.sync(card, MINUTE).await;
    assert_eq!(w.stage(card).await, "merged");

    // Red checks send it back to code, where the builder works the branch.
    let (w, card) = with_pull_request("ci").await;
    w.checks(&[("build", CheckState::Failed), passed("lint")]);
    w.sync(card, MINUTE).await;
    assert_eq!(w.stage(card).await, "code");
    w.checks(&[passed("build"), passed("lint")]);
    let message = refused(merge(&w, &person(), card).await);
    assert!(message.contains("a stage is running"), "{message}");
    assert!(w.host.calls().iter().all(|c| !c.starts_with("merge ")));
}

#[tokio::test]
async fn a_merge_the_host_refuses_leaves_the_card_where_it_was() {
    let (w, card) = held_at_cleanup().await;
    w.host
        .fail_next(HostError::Unauthorized("bad token".into()));
    let error = merge(&w, &person(), card).await.unwrap_err();
    assert!(
        matches!(error, IntakeError::Host(HostError::Unauthorized(_))),
        "{error:?}"
    );
    assert_eq!(w.stage(card).await, "cleanup");
    assert_eq!(w.stored_pull(card).await.state, "open");
}

#[tokio::test]
async fn the_merge_order_accepts_the_cards_one_repo_and_nothing_else() {
    let (w, card) = held_at_cleanup().await;
    let order = |repos: &[&str]| {
        let repos: Vec<String> = repos.iter().map(|r| (*r).to_owned()).collect();
        let (w, card) = (&w, card);
        async move { set_merge_order(&w.env(), &person(), card, &repos).await }
    };
    assert_eq!(order(&["example-org/api"]).await.unwrap().id, card);
    assert!(matches!(order(&[]).await, Err(OpError::InvalidArgument(_))));
    assert!(matches!(
        order(&["example-org/api", "example-org/web"]).await,
        Err(OpError::InvalidArgument(_))
    ));
    assert!(matches!(
        order(&["example-org/web"]).await,
        Err(OpError::InvalidArgument(_))
    ));
}

#[tokio::test]
async fn a_person_can_skip_a_waiting_stage_and_pausing_restarts_its_wait() {
    let (w, card) = with_pull_request("ci").await;
    pause_card(&w.env(), &person(), card).await.unwrap();
    assert_eq!(w.run(card).await.state, RunState::Paused);
    // A paused stage is not read, so nothing passes it behind the person's back.
    w.checks(&[passed("build"), passed("lint")]);
    w.sync(card, MINUTE).await;
    assert_eq!(w.stage(card).await, "validate");
    resume_card(&w.env(), &person(), card).await.unwrap();
    assert_eq!(w.run(card).await.state, RunState::Running);
    w.sync(card, MINUTE).await;
    assert_eq!(w.stage(card).await, "merged");

    skip_stage(&w.env(), &person(), card).await.unwrap();
    assert_eq!(w.stage(card).await, "after");
}

// The built-in `rpi` workflow, which these stages exist for.

#[test]
fn every_stage_of_the_built_in_rpi_workflow_resolves_to_something_that_runs() {
    use buzz_hive_core::workflow::actions::{action_names, is_waiting_action};
    use buzz_hive_core::workflow::builtin::builtin_source;
    let catalog = load(&[builtin_source()]);
    assert_eq!(catalog.findings, vec![]);
    let rpi = catalog.get("rpi").unwrap();
    let runs_now = [
        "open_pull_request",
        "cleanup_worktree",
        "mark_ready",
        "merge",
        "wait",
        "wait_for_ci",
    ];
    for stage in &rpi.workflow.stages {
        match stage.kind.as_str() {
            "prompt" => assert!(stage.agent.is_some(), "{} names an agent", stage.id),
            "approval" => assert!(stage.approval.is_some(), "{}", stage.id),
            "action" => {
                assert!(
                    action_names().contains(&stage.action.as_str()),
                    "{}",
                    stage.action
                );
                assert!(
                    runs_now.contains(&stage.action.as_str()),
                    "{} does not run",
                    stage.action
                );
                if stage.action == "wait_for_ci" {
                    assert!(is_waiting_action(stage));
                    assert!(buzz_hive_core::rules::wait::waiting_of(stage).is_some());
                }
            }
            other => panic!("{} is a {other} stage, which nothing runs", stage.id),
        }
    }
    // Every action the reducer names is a built-in one, so a workflow cannot spell one wrong.
    for name in runs_now {
        assert!(action_names().contains(&name), "{name}");
    }
}
