//! Every event kind introduced so far is appended by at least one scenario.
//!
//! Ported from `zz_event_coverage_test.go`. A phase that adds a kind to `EVENT_KINDS`
//! adds a scenario here and bumps `CURRENT_PHASE`.

mod common;

use buzz_hive_core::access::Level;
use buzz_hive_core::access::{ActorKind, RelayRole};
use buzz_hive_core::domain::artifact::ArtifactContent;
use buzz_hive_core::domain::card::CardState;
use buzz_hive_core::domain::card::PullRequest;
use buzz_hive_core::domain::events::EVENT_KINDS;
use buzz_hive_core::domain::intake::RepoRef;
use buzz_hive_core::orchestrator::access::set_level;
use buzz_hive_core::orchestrator::activity::log_work;
use buzz_hive_core::orchestrator::agent_tools;
use buzz_hive_core::orchestrator::artifacts::{save_artifact, set_handoff};
use buzz_hive_core::orchestrator::assistant::{
    assistant_result, expire_jobs, start_job, JobKind, Reported, Start,
};
use buzz_hive_core::orchestrator::cards::{
    add_note, archive_cards, create_card, delete_card, restore_card, update_card, CardPatch,
    NewCard,
};
use buzz_hive_core::orchestrator::inbox::{approve, send_back};
use buzz_hive_core::orchestrator::repos::{add_repo, remove_repo};
use buzz_hive_core::orchestrator::run::{resume_card, skip_stage, start_run};
use buzz_hive_core::orchestrator::secrets;
use buzz_hive_core::orchestrator::settings::{set_setting, Scope};
use buzz_hive_core::orchestrator::timers;
use buzz_hive_core::orchestrator::wait::{record, Seen};
use buzz_hive_core::orchestrator::Env;
use buzz_hive_core::ports::code_host::PullStatus;
use buzz_hive_core::ports::secrets::SecretString;
use buzz_hive_core::ports::store::Store;
use buzz_hive_core::testing::{FakeCodeHost, FakeSecretSink, FakeSupervisor, ManualClock};
use buzz_hive_core::testing::{FakeDirectory, MemStore};
use common::{ctx_for, person, Fixture, BUILDER, CHANNEL, COMMUNITY};
use std::time::Duration;

const CURRENT_PHASE: u8 = 23;

async fn card_scenarios(store: &MemStore) {
    let ctx = person();
    let (card, _) = create_card(
        store,
        &ctx,
        NewCard {
            title: "scenario".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    update_card(
        store,
        &ctx,
        card.id,
        CardPatch {
            workflow: Some("rpi".into()),
            state: Some(CardState::Running),
        },
    )
    .await
    .unwrap();
    add_note(store, &ctx, card.id, "a note", "").await.unwrap();
    update_card(
        store,
        &ctx,
        card.id,
        CardPatch {
            state: Some(CardState::Stopped),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    archive_cards(store, &ctx, &[card.id]).await.unwrap();
    restore_card(store, &ctx, card.id).await.unwrap();
    delete_card(store, &ctx, card.id).await.unwrap();
}

/// Phase 6: the update channel, a run that starts, and the stage task that reaches the relay.
async fn run_scenarios(f: &Fixture) {
    let ctx = person();
    set_setting(
        &f.env(),
        &ctx,
        &Scope::User,
        "update_channel",
        Some(serde_json::json!(CHANNEL.to_string())),
    )
    .await
    .unwrap();
    let (card, _) = create_card(
        &f.store,
        &ctx,
        NewCard {
            title: "run".into(),
            workflow: "demo".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    start_run(&f.env(), &ctx, card.id).await.unwrap();
    f.drain().await.unwrap();
}

/// Phase 7: the agent reports a result, and a passed last stage ends the run.
async fn report_scenarios(f: &Fixture) {
    let ctx = person();
    set_setting(
        &f.env(),
        &ctx,
        &Scope::User,
        "update_channel",
        Some(serde_json::json!(CHANNEL.to_string())),
    )
    .await
    .unwrap();
    let (card, _) = create_card(
        &f.store,
        &ctx,
        NewCard {
            title: "report".into(),
            workflow: "solo".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    start_run(&f.env(), &ctx, card.id).await.unwrap();
    let agent = ctx_for(BUILDER, ActorKind::Agent);
    f.report(&agent, card.id, "passed", "done").await.unwrap();
}

/// A card on `workflow` whose run has started and whose posts have reached the relay.
async fn begun(f: &Fixture, workflow: &str) -> uuid::Uuid {
    let ctx = person();
    let (card, _) = create_card(
        &f.store,
        &ctx,
        NewCard {
            title: workflow.into(),
            workflow: workflow.into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    start_run(&f.env(), &ctx, card.id).await.unwrap();
    f.drain().await.unwrap();
    card.id
}

/// Phase 8: results route a card, a result with no route waits, a person skips and resumes,
/// a repeat stage runs rounds, and a stage whose conditions fail is skipped.
async fn routing_scenarios(f: &Fixture) {
    let agent = ctx_for(BUILDER, ActorKind::Agent);
    let report = |card: uuid::Uuid, result: &'static str| {
        let agent = agent.clone();
        async move {
            f.report(&agent, card, result, "").await.unwrap();
            f.drain().await.unwrap();
        }
    };

    // A result with no route waits, and a person skips the stage.
    let card = begun(f, "three").await;
    report(card, "passed").await;
    report(card, "failed").await;
    skip_stage(&f.env(), &person(), card).await.unwrap();

    // A held stage waits for a person.
    let card = begun(f, "held").await;
    report(card, "passed").await;
    resume_card(&f.env(), &person(), card).await.unwrap();

    // Rounds of a repeat stage.
    let card = begun(f, "plain").await;
    report(card, "needs_work").await;

    // Conditions the card does not meet.
    let card = begun(f, "cond").await;
    report(card, "passed").await;
}

/// Phase 9: an agent saves an artifact and logs work, and a person sets the handoff.
async fn artifact_scenarios(f: &Fixture) {
    let agent = ctx_for(BUILDER, ActorKind::Agent);
    let card = begun(f, "docs").await;
    save_artifact(
        &f.env(),
        &agent,
        card,
        "plan",
        ArtifactContent::Text("- [ ] one".into()),
    )
    .await
    .unwrap();
    log_work(&f.env(), &agent, card, "Wrote the plan.", "", "")
        .await
        .unwrap();
    set_handoff(&f.env(), &person(), card, "plan", false)
        .await
        .unwrap();
}

/// Phase 11: a stage times out and is scheduled to run again.
async fn retry_scenarios(f: &Fixture, clock: &ManualClock) {
    let ctx = person();
    set_setting(
        &f.env(),
        &ctx,
        &Scope::User,
        "execution.retries.max",
        Some(serde_json::json!(1)),
    )
    .await
    .unwrap();
    begun(f, "solo").await;
    clock.advance(Duration::from_secs(31 * 60));
    for timer in timers::due(&f.store, COMMUNITY, 10).await.unwrap() {
        timers::fire(&f.env(), COMMUNITY, &timer).await.unwrap();
    }
}

/// Phase 12: a plan reaches an approval stage, is sent back once and approved the second time.
async fn approval_scenarios(f: &Fixture) {
    let agent = ctx_for(BUILDER, ActorKind::Agent);
    let card = begun(f, "gated").await;
    for round in 0..2 {
        save_artifact(
            &f.env(),
            &agent,
            card,
            "plan",
            ArtifactContent::Text(format!("version {round}")),
        )
        .await
        .unwrap();
        f.report(&agent, card, "passed", "ready").await.unwrap();
        if round == 0 {
            send_back(&f.env(), &person(), card, "Add a rollback step.")
                .await
                .unwrap();
        } else {
            approve(&f.env(), &person(), card, &Default::default())
                .await
                .unwrap();
        }
    }
}

/// Phase 14: an admin sets a level.
async fn access_scenarios(f: &Fixture) {
    let directory = FakeDirectory::new()
        .with_role("aa", RelayRole::Owner)
        .with_role(BUILDER, RelayRole::Member);
    let env = Env {
        directory: &directory,
        ..f.env()
    };
    set_level(&env, &person(), BUILDER, Level::Run)
        .await
        .unwrap();
}

/// Phase 16: a rule makes a card, a failed pass is recorded, a closed issue leaves the search,
/// and a comment on a running card's issue is read and handed to its agent.
async fn intake_scenarios(f: &Fixture) {
    use buzz_hive_core::background::comments::poll_comments;
    use buzz_hive_core::background::intake::{record_failure, refresh_rule};
    use buzz_hive_core::domain::intake::{IntakeRule, Issue, IssueState, RepoRef, RuleFilter};
    use buzz_hive_core::orchestrator::intake::save_rule;
    use buzz_hive_core::testing::FakeCodeHost;

    set_setting(
        &f.env(),
        &person(),
        &Scope::User,
        "update_channel",
        Some(serde_json::json!(CHANNEL.to_string())),
    )
    .await
    .unwrap();
    let repo = RepoRef::parse("example-org/api").unwrap();
    let issue = |number: u64, state: IssueState| Issue {
        repo: repo.clone(),
        number,
        title: format!("Issue {number}"),
        body: String::new(),
        labels: Vec::new(),
        assignees: Vec::new(),
        author: "octo".into(),
        state,
        url: String::new(),
        etag: None,
    };
    let host = FakeCodeHost::new();
    host.put_issue(issue(1, IssueState::Open));
    host.put_issue(issue(2, IssueState::Open));
    let rule = save_rule(
        &f.env(),
        COMMUNITY,
        IntakeRule {
            id: uuid::Uuid::nil(),
            name: "all".into(),
            repo: repo.clone(),
            filter: RuleFilter {
                authors: vec!["octo".into()],
                ..RuleFilter::default()
            },
            workflow: "demo".into(),
            enabled: true,
            interval_s: 60,
        },
    )
    .await
    .unwrap();
    let now = chrono::Utc::now();
    refresh_rule(&f.env(), &host, COMMUNITY, rule.id, now)
        .await
        .unwrap();
    record_failure(&f.env(), COMMUNITY, rule.id, "example-org/api", "down", 1)
        .await
        .unwrap();
    host.put_issue(issue(2, IssueState::Closed));
    refresh_rule(&f.env(), &host, COMMUNITY, rule.id, now)
        .await
        .unwrap();

    let mut tx = f.store.begin(COMMUNITY).await.unwrap();
    let card = tx
        .issues()
        .get(&repo, 1)
        .await
        .unwrap()
        .unwrap()
        .card_id
        .unwrap();
    drop(tx);
    start_run(&f.env(), &person(), card).await.unwrap();
    host.put_comment(
        &repo,
        1,
        "octo",
        "Please look at this.",
        now + chrono::Duration::seconds(5),
    );
    poll_comments(
        &f.env(),
        &host,
        COMMUNITY,
        now + chrono::Duration::minutes(1),
    )
    .await
    .unwrap();
}

/// Phase 18: the host reports a pull request that is different from the one the card has.
async fn pull_request_scenarios(f: &Fixture) {
    let (card, _) = create_card(
        &f.store,
        &person(),
        NewCard {
            title: "pull request".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let pull = PullRequest {
        repo: "example-org/api".into(),
        number: 3,
        state: "open".into(),
        ..Default::default()
    };
    let mut tx = f.store.begin(COMMUNITY).await.unwrap();
    tx.pull_requests().put(card.id, &pull).await.unwrap();
    tx.commit().await.unwrap();
    let seen = Seen {
        status: Some(PullStatus {
            pull: PullRequest {
                approved: true,
                ..pull
            },
            labels: Vec::new(),
        }),
        checks: None,
        head_sha: None,
        gone: false,
        now: chrono::Utc::now(),
    };
    record(&f.env(), COMMUNITY, card.id, &seen).await.unwrap();
}

/// Phase 20: the assistant is asked twice. One agent reports, and the other never does and
/// runs out of time.
async fn assistant_scenarios(f: &Fixture, clock: &ManualClock) {
    set_setting(
        &f.env(),
        &person(),
        &Scope::User,
        "assistant_agent",
        Some(serde_json::json!("builder")),
    )
    .await
    .unwrap();
    let ask = |text: &str| Start {
        kind: JobKind::Chat,
        text: text.into(),
        draft: None,
        skills: Vec::new(),
        root: None,
    };
    let (answered, _) = start_job(&f.env(), &f.relay, &person(), ask("one"))
        .await
        .unwrap();
    assistant_result(
        &f.env(),
        &ctx_for(BUILDER, ActorKind::Agent),
        Reported {
            job_id: answered,
            message: "hello".into(),
            draft: None,
            base_hash: None,
        },
    )
    .await
    .unwrap();
    start_job(&f.env(), &f.relay, &person(), ask("two"))
        .await
        .unwrap();
    clock.advance(Duration::from_secs(31 * 60));
    expire_jobs(&f.env(), COMMUNITY, 10).await.unwrap();
}

/// Phase 21: a repo is added to a card and taken off it.
async fn repo_scenarios(f: &Fixture) {
    let (card, _) = create_card(
        &f.store,
        &person(),
        NewCard {
            title: "repos".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let host = FakeCodeHost::new();
    host.put_repo(&RepoRef::parse("example-org/api").unwrap());
    add_repo(&f.store, &host, &person(), card.id, "example-org/api", "")
        .await
        .unwrap();
    remove_repo(&f.store, &person(), card.id, "example-org/api")
        .await
        .unwrap();
}

/// Phase 22: a person sets an agent's tool server.
async fn agent_tools_scenarios(f: &Fixture) {
    agent_tools::seed(&f.store, COMMUNITY, "coder", None)
        .await
        .unwrap();
    let supervisor = FakeSupervisor::new();
    supervisor.deploy("coder", "thread", true);
    let server = buzz_hive_core::domain::tool_server::ToolServer::new(
        "docs",
        buzz_hive_core::domain::tool_server::Transport::Stdio,
        &serde_json::json!({"command": "docs-mcp"}),
    )
    .unwrap();
    agent_tools::set(&f.store, Some(&supervisor), &person(), "coder", server)
        .await
        .unwrap();
}

/// Phase 23: a person sets a team secret and grants it to an agent.
async fn secret_scenarios(f: &Fixture) {
    let supervisor = FakeSupervisor::new();
    supervisor.deploy("coder", "thread", true);
    let sink = FakeSecretSink::new();
    secrets::set_team(
        &f.store,
        Some(&sink),
        Some(&supervisor),
        &person(),
        "GH_TOKEN",
        SecretString::new("example-value"),
    )
    .await
    .unwrap();
    secrets::grant(&f.store, Some(&supervisor), &person(), "coder", "GH_TOKEN")
        .await
        .unwrap();
}

#[tokio::test]
async fn every_kind_introduced_so_far_is_appended_by_some_scenario() {
    let (f, clock) = Fixture::timed();
    let store = &f.store;
    card_scenarios(store).await;
    run_scenarios(&f).await;
    report_scenarios(&f).await;
    routing_scenarios(&f).await;
    artifact_scenarios(&f).await;
    retry_scenarios(&f, &clock).await;
    approval_scenarios(&f).await;
    access_scenarios(&f).await;
    intake_scenarios(&f).await;
    pull_request_scenarios(&f).await;
    assistant_scenarios(&f, &clock).await;
    repo_scenarios(&f).await;
    agent_tools_scenarios(&f).await;
    secret_scenarios(&f).await;

    let committed = store.committed_kinds();
    let missing: Vec<_> = EVENT_KINDS
        .iter()
        .filter(|(kind, phase)| *phase <= CURRENT_PHASE && !committed.contains(*kind))
        .map(|(kind, _)| *kind)
        .collect();
    assert!(missing.is_empty(), "no scenario appends {missing:?}");
    for kind in &committed {
        assert!(
            EVENT_KINDS.iter().any(|(k, _)| k == kind),
            "{kind} is not in the registry"
        );
    }
}
