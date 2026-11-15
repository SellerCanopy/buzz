//! A card routed through several stages: one new thread per stage, repeat rounds, held
//! stages, conditions, loop limits, skips, moves and the hand-off to the next workflow.
//!
//! Ported from the Go engine's `scenario_routes`, `scenario_repeat` and condition scenarios.

mod common;

use std::collections::BTreeMap;

use buzz_hive_core::access::ActorKind;
use buzz_hive_core::domain::card::CardState;
use buzz_hive_core::domain::events::Event;
use buzz_hive_core::domain::run::{Run, RunState};
use buzz_hive_core::orchestrator::cards::{create_card, set_card_workflow, CardMove, NewCard};
use buzz_hive_core::orchestrator::run::{
    move_card, pause_card, resume_card, skip_stage, start_run, start_run_at,
};
use buzz_hive_core::orchestrator::settings::{set_setting, Scope};
use buzz_hive_core::orchestrator::OpError;
use buzz_hive_core::ports::store::{Posting, Store};
use buzz_hive_core::workflow::options::WorkflowOptions;
use common::{ctx_for, person, Fixture, BUILDER, CHANNEL, COMMUNITY};
use serde_json::json;
use uuid::Uuid;

fn builder() -> buzz_hive_core::orchestrator::Ctx {
    ctx_for(BUILDER, ActorKind::Agent)
}

fn kinds(events: &[Event]) -> Vec<&str> {
    events.iter().map(|e| e.kind.as_str()).collect()
}

/// A card on `workflow` whose run has started and whose first posts have reached the relay.
async fn started(workflow: &str) -> (Fixture, Uuid) {
    let f = Fixture::new();
    set_setting(
        &f.env(),
        &person(),
        &Scope::User,
        "update_channel",
        Some(json!(CHANNEL.to_string())),
    )
    .await
    .unwrap();
    let (card, _) = create_card(
        &f.store,
        &person(),
        NewCard {
            title: "Fix login".into(),
            description: "It breaks.".into(),
            workflow: workflow.into(),
        },
    )
    .await
    .unwrap();
    start_run(&f.env(), &person(), card.id).await.unwrap();
    f.drain().await.unwrap();
    (f, card.id)
}

/// The agent reports `result`, and the posts it caused reach the relay.
async fn report(f: &Fixture, card: Uuid, result: &str) -> (Run, Vec<Event>) {
    let done = f.report(&builder(), card, result, "").await.unwrap();
    f.drain().await.unwrap();
    done
}

async fn card_state(f: &Fixture, card: Uuid) -> CardState {
    let mut tx = f.store.begin(COMMUNITY).await.unwrap();
    tx.cards().get(card).await.unwrap().unwrap().state
}

async fn root_of(f: &Fixture, card: Uuid, stage: &str, visit: u32) -> Option<String> {
    let mut tx = f.store.begin(COMMUNITY).await.unwrap();
    tx.runs()
        .thread(card, stage, visit)
        .await
        .unwrap()
        .map(|t| t.root_event_id)
}

fn refused(error: OpError) -> String {
    match error {
        OpError::FailedPrecondition(message) => message,
        other => panic!("want a failed precondition, got {other:?}"),
    }
}

#[tokio::test]
async fn a_three_stage_card_opens_three_threads_with_three_roots() {
    let (f, card) = started("three").await;
    let (run, events) = report(&f, card, "passed").await;
    assert_eq!(run.current_stage.as_deref(), Some("b"));
    assert_eq!(kinds(&events), ["result.reported", "stage.routed"]);
    report(&f, card, "passed").await;
    let (run, events) = report(&f, card, "passed").await;
    assert_eq!(run.state, RunState::Ended);
    assert_eq!(
        kinds(&events),
        ["result.reported", "run.ended", "card.state"]
    );
    assert_eq!(card_state(&f, card).await, CardState::Done);

    let roots: Vec<String> = [
        root_of(&f, card, "a", 1).await,
        root_of(&f, card, "b", 1).await,
        root_of(&f, card, "c", 1).await,
    ]
    .into_iter()
    .map(|r| r.expect("each stage opened a thread"))
    .collect();
    assert_eq!(
        roots
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        3,
        "{roots:?}"
    );
    // Every stage task is a top-level mention of the agent. Only the start has a status line.
    let posts = f.relay.posts();
    let tasks: Vec<_> = posts.iter().filter(|p| p.mention.is_some()).collect();
    assert_eq!(tasks.len(), 3);
    assert!(tasks.iter().all(|p| p.root.is_none()));
    assert_eq!(posts.len(), 4);
    assert_eq!(
        run.steps
            .iter()
            .map(|s| s.stage.as_str())
            .collect::<Vec<_>>(),
        ["a", "b", "c"]
    );
    assert_eq!(run.steps[2].routed_to.as_deref(), Some("done"));
}

#[tokio::test]
async fn a_fresh_session_repeat_adds_a_root_per_round() {
    let (f, card) = started("fresh").await;
    let (run, events) = report(&f, card, "needs_work").await;
    assert_eq!(run.state, RunState::Running);
    assert_eq!(run.visit, 2);
    assert_eq!(run.steps.len(), 1, "rounds are one step");
    assert_eq!(run.steps[0].round, 2);
    assert_eq!(
        kinds(&events),
        ["result.reported", "round.ended", "round.started"]
    );
    report(&f, card, "needs_work").await;
    let first = root_of(&f, card, "work", 1).await.unwrap();
    let second = root_of(&f, card, "work", 2).await.unwrap();
    let third = root_of(&f, card, "work", 3).await.unwrap();
    assert!(first != second && second != third && first != third);

    let tasks: Vec<_> = f
        .relay
        .posts()
        .into_iter()
        .filter(|p| p.mention.is_some())
        .collect();
    assert_eq!(tasks.len(), 3);
    assert!(
        tasks[1].text.contains("This is round 2."),
        "{}",
        tasks[1].text
    );
    assert!(tasks.iter().all(|p| p.root.is_none()));

    // The agent that reports must be the one the latest thread mentioned, and passed ends it.
    let (run, _) = report(&f, card, "passed").await;
    assert_eq!(run.state, RunState::Ended);
}

#[tokio::test]
async fn a_plain_repeat_replies_in_the_stage_root() {
    let (f, card) = started("plain").await;
    let root = root_of(&f, card, "work", 1).await.unwrap();
    let (run, _) = report(&f, card, "needs_work").await;
    assert_eq!(run.visit, 1);
    assert_eq!(run.steps[0].round, 2);
    let posts = f.relay.posts();
    let reply = posts.last().unwrap();
    assert_eq!(reply.root.as_deref(), Some(root.as_str()));
    assert_eq!(reply.mention.as_deref(), Some(BUILDER));
    assert!(reply.text.contains("This is round 2."), "{}", reply.text);
    assert!(root_of(&f, card, "work", 2).await.is_none());

    // The third round is the stage's last.
    report(&f, card, "needs_work").await;
    let (run, _) = report(&f, card, "needs_work").await;
    assert_eq!(run.state, RunState::Waiting);
    assert_eq!(card_state(&f, card).await, CardState::Waiting);
}

#[tokio::test]
async fn a_held_stage_waits_for_resume_card() {
    let (f, card) = started("held").await;
    let (run, events) = report(&f, card, "passed").await;
    assert_eq!(run.state, RunState::Paused);
    assert_eq!(run.current_stage.as_deref(), Some("b"));
    assert_eq!(
        kinds(&events),
        [
            "result.reported",
            "stage.routed",
            "run.paused",
            "card.state"
        ]
    );
    assert_eq!(card_state(&f, card).await, CardState::Waiting);
    // Nothing was posted for b, and its agent may not report.
    assert!(root_of(&f, card, "b", 1).await.is_none());
    assert_eq!(
        f.relay
            .posts()
            .iter()
            .filter(|p| p.mention.is_some())
            .count(),
        1
    );
    let error = f.report(&builder(), card, "passed", "").await.unwrap_err();
    assert!(refused(error).contains("run is paused"));

    let (run, events) = resume_card(&f.env(), &person(), card).await.unwrap();
    assert_eq!(run.state, RunState::Running);
    assert_eq!(kinds(&events), ["run.resumed", "card.state"]);
    f.drain().await.unwrap();
    assert!(root_of(&f, card, "b", 1).await.is_some());
    let (run, _) = report(&f, card, "passed").await;
    assert_eq!(run.state, RunState::Ended);
}

#[tokio::test]
async fn an_agent_may_not_skip_pause_or_resume_a_card() {
    let (f, card) = started("three").await;
    // The stage's own agent, and any other agent, are refused the same way.
    for who in [builder(), ctx_for("cc", ActorKind::Agent)] {
        for (call, result) in [
            ("skip", skip_stage(&f.env(), &who, card).await),
            ("pause", pause_card(&f.env(), &who, card).await),
            ("resume", resume_card(&f.env(), &who, card).await),
        ] {
            let error = result.unwrap_err();
            assert!(
                matches!(error, OpError::PermissionDenied(ref m) if m.contains("only a person")),
                "{call}: {error:?}"
            );
        }
    }
    // Nothing changed: a person can still pause the card, which was running.
    let (run, _) = pause_card(&f.env(), &person(), card).await.unwrap();
    assert_eq!(run.state, RunState::Paused);
    // And a refused agent did not get to resume or skip it.
    assert!(resume_card(&f.env(), &builder(), card).await.is_err());
    assert_eq!(card_state(&f, card).await, CardState::Waiting);
}

#[tokio::test]
async fn pausing_a_running_card_refuses_the_agent_and_resume_tells_it_to_carry_on() {
    let (f, card) = started("three").await;
    let (run, events) = pause_card(&f.env(), &person(), card).await.unwrap();
    assert_eq!(run.state, RunState::Paused);
    assert_eq!(kinds(&events), ["run.paused", "card.state"]);
    let error = pause_card(&f.env(), &person(), card).await.unwrap_err();
    assert!(refused(error).contains("only a running card"));
    let error = f.report(&builder(), card, "passed", "").await.unwrap_err();
    assert!(refused(error).contains("run is paused"));

    let (run, _) = resume_card(&f.env(), &person(), card).await.unwrap();
    assert_eq!(run.state, RunState::Running);
    f.drain().await.unwrap();
    let root = root_of(&f, card, "a", 1).await.unwrap();
    let reply = f.relay.posts().into_iter().last().unwrap();
    assert_eq!(reply.root.as_deref(), Some(root.as_str()));
    assert!(reply.text.contains("running again"), "{}", reply.text);
    // The agent reports where it left off.
    let (run, _) = report(&f, card, "passed").await;
    assert_eq!(run.current_stage.as_deref(), Some("b"));
    // Resuming a card that is not paused is refused.
    let error = resume_card(&f.env(), &person(), card).await.unwrap_err();
    assert!(refused(error).contains("only a paused card"));
}

#[tokio::test]
async fn a_stage_whose_labels_are_missing_follows_else() {
    let (f, card) = started("cond").await;
    let (run, events) = report(&f, card, "passed").await;
    assert_eq!(run.current_stage.as_deref(), Some("c"));
    assert_eq!(
        kinds(&events),
        [
            "result.reported",
            "stage.routed",
            "condition.unmet",
            "card.skipped",
            "stage.routed"
        ]
    );
    assert_eq!(events[2].payload["condition"], "labels frontend");
    // b never opened a thread, and the run records that it did not run.
    assert!(root_of(&f, card, "b", 1).await.is_none());
    assert!(root_of(&f, card, "c", 1).await.is_some());
    assert!(run.steps[1].skipped);
    assert_eq!(run.steps[1].routed_to.as_deref(), Some("c"));
}

#[tokio::test]
async fn a_loop_limit_stops_the_second_trip_back() {
    let (f, card) = started("loop").await;
    report(&f, card, "passed").await;
    // The first failure goes back to a, as b's `on` says.
    let (run, events) = report(&f, card, "failed").await;
    assert_eq!(run.current_stage.as_deref(), Some("a"));
    assert!(events
        .iter()
        .any(|e| e.kind == "stage.routed" && e.payload["backward"] == true));
    report(&f, card, "passed").await;
    // The second would be the loop's second trip, and the limit is one.
    let (run, events) = report(&f, card, "failed").await;
    assert_eq!(run.state, RunState::Waiting);
    assert_eq!(run.current_stage.as_deref(), Some("b"));
    let waiting = events.iter().find(|e| e.kind == "run.waiting").unwrap();
    let reason = waiting.payload["reason"].as_str().unwrap();
    assert!(
        reason.contains("2nd time") && reason.contains("loop limit is 1"),
        "{reason}"
    );
}

#[tokio::test]
async fn a_person_skips_a_waiting_stage_and_the_card_moves_on() {
    let (f, card) = started("three").await;
    // A running card cannot be skipped.
    let error = skip_stage(&f.env(), &person(), card).await.unwrap_err();
    assert!(refused(error).contains("only a card that waits"));

    let (run, _) = report(&f, card, "failed").await;
    assert_eq!(run.state, RunState::Waiting);
    let (run, events) = skip_stage(&f.env(), &person(), card).await.unwrap();
    assert_eq!(run.state, RunState::Running);
    assert_eq!(run.current_stage.as_deref(), Some("b"));
    assert_eq!(
        kinds(&events),
        [
            "inbox.resolved",
            "card.skipped",
            "stage.routed",
            "card.state"
        ]
    );
    assert!(run.steps[0].skipped);
    assert_eq!(card_state(&f, card).await, CardState::Running);
}

#[tokio::test]
async fn a_finished_card_is_handed_to_the_next_workflow() {
    let (f, card) = started("held").await;
    {
        let mut tx = f.store.begin(COMMUNITY).await.unwrap();
        tx.workflows()
            .set_options(
                "held",
                &WorkflowOptions {
                    next_workflow: "second".into(),
                    ..WorkflowOptions::default()
                },
            )
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }
    // The row has no pause list, so the package's own pause on b stands.
    report(&f, card, "passed").await;
    resume_card(&f.env(), &person(), card).await.unwrap();
    f.drain().await.unwrap();
    let (run, events) = report(&f, card, "passed").await;
    assert_eq!(run.state, RunState::Ended);
    assert!(
        kinds(&events).contains(&"card.moved"),
        "{:?}",
        kinds(&events)
    );

    let mut tx = f.store.begin(COMMUNITY).await.unwrap();
    let stored = tx.cards().get(card).await.unwrap().unwrap();
    assert_eq!(stored.workflow, "second");
    assert_eq!(stored.state, CardState::Running);
    let active = tx.runs().active_for_card(card).await.unwrap().unwrap();
    assert_eq!(active.workflow, "second");
    assert_eq!(active.current_stage.as_deref(), Some("s1"));
    drop(tx);
    f.drain().await.unwrap();
    assert!(root_of(&f, card, "s1", 1).await.is_some());
}

#[tokio::test]
async fn a_next_workflow_that_does_not_exist_leaves_the_card_done_with_a_note() {
    let (f, card) = started("second").await;
    {
        let mut tx = f.store.begin(COMMUNITY).await.unwrap();
        tx.workflows()
            .set_options(
                "second",
                &WorkflowOptions {
                    next_workflow: "ghost".into(),
                    ..WorkflowOptions::default()
                },
            )
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }
    let (run, events) = report(&f, card, "passed").await;
    assert_eq!(run.state, RunState::Ended);
    assert_eq!(card_state(&f, card).await, CardState::Done);
    let note = events.iter().find(|e| e.kind == "activity.added").unwrap();
    assert!(
        note.payload["summary"].as_str().unwrap().contains("ghost"),
        "{note:?}"
    );
}

#[tokio::test]
async fn an_agent_moves_the_card_only_when_the_workflow_allows_it() {
    let (f, card) = started("three").await;
    let error = f.move_to(&builder(), card, "c").await.unwrap_err();
    assert!(refused(error).contains("side_effects.agent_moves_cards"));

    set_setting(
        &f.env(),
        &person(),
        &Scope::Workflow("three".into()),
        "side_effects.agent_moves_cards",
        Some(json!(true)),
    )
    .await
    .unwrap();
    // A person is not the stage's agent.
    let error = f.move_to(&person(), card, "c").await.unwrap_err();
    assert!(matches!(error, OpError::PermissionDenied(_)), "{error:?}");
    let error = f.move_to(&builder(), card, "nope").await.unwrap_err();
    assert!(matches!(error, OpError::InvalidArgument(_)), "{error:?}");

    let (run, events) = f.move_to(&builder(), card, "c").await.unwrap();
    assert_eq!(run.current_stage.as_deref(), Some("c"));
    assert_eq!(kinds(&events), ["stage.routed"]);
    f.drain().await.unwrap();
    assert!(root_of(&f, card, "c", 1).await.is_some());
    assert!(root_of(&f, card, "b", 1).await.is_none());
}

#[tokio::test]
async fn a_move_names_the_stage_and_visit_it_is_made_from() {
    let (f, card) = started("three").await;
    set_setting(
        &f.env(),
        &person(),
        &Scope::Workflow("three".into()),
        "side_effects.agent_moves_cards",
        Some(json!(true)),
    )
    .await
    .unwrap();
    for (stage, visit) in [("", 1), ("a", 0)] {
        let error = move_card(&f.env(), &builder(), card, stage, visit, "c")
            .await
            .unwrap_err();
        assert!(
            matches!(error, OpError::InvalidArgument(_)),
            "{stage:?} {visit}: {error:?}"
        );
    }
    // The card is at `a`, visit 1. A move from `b`, or from a visit it has not reached, is
    // stale.
    for (stage, visit) in [("b", 1), ("a", 2)] {
        let error = move_card(&f.env(), &builder(), card, stage, visit, "c")
            .await
            .unwrap_err();
        assert!(
            refused(error).contains(&format!("stage {stage} visit {visit}")),
            "{stage} {visit}"
        );
    }
    assert_eq!(f.at(card).await, ("a".to_owned(), 1));
    let (run, _) = move_card(&f.env(), &builder(), card, "a", 1, "c")
        .await
        .unwrap();
    assert_eq!(run.current_stage.as_deref(), Some("c"));
}

#[tokio::test]
async fn a_run_starts_at_a_named_stage_with_its_own_parameters() {
    let f = Fixture::new();
    set_setting(
        &f.env(),
        &person(),
        &Scope::User,
        "update_channel",
        Some(json!(CHANNEL.to_string())),
    )
    .await
    .unwrap();
    let (card, _) = create_card(
        &f.store,
        &person(),
        NewCard {
            title: "Fix login".into(),
            workflow: "demo".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let params = |name: &str, value: &str| BTreeMap::from([(name.to_owned(), value.to_owned())]);

    for bad in [params("depth", "shallow"), params("nope", "x")] {
        let error = start_run_at(&f.env(), &person(), card.id, Some("research"), bad)
            .await
            .unwrap_err();
        assert!(matches!(error, OpError::InvalidArgument(_)), "{error:?}");
    }
    let error = start_run_at(&f.env(), &person(), card.id, Some("nope"), BTreeMap::new())
        .await
        .unwrap_err();
    assert!(matches!(error, OpError::InvalidArgument(_)), "{error:?}");

    let (run, _) = start_run_at(
        &f.env(),
        &person(),
        card.id,
        Some("research"),
        params("depth", "deep"),
    )
    .await
    .unwrap();
    assert_eq!(run.parameters["depth"], "deep");
    let mut tx = f.store.begin(COMMUNITY).await.unwrap();
    let task = tx
        .outbox()
        .by_key(&format!("kiln:{}:research:1", card.id))
        .await
        .unwrap()
        .unwrap();
    match task.posting {
        Posting::StageTask { text, .. } => {
            assert!(text.contains("at depth deep"), "{text}")
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn a_run_started_at_a_later_stage_begins_there() {
    let f = Fixture::new();
    set_setting(
        &f.env(),
        &person(),
        &Scope::User,
        "update_channel",
        Some(json!(CHANNEL.to_string())),
    )
    .await
    .unwrap();
    let (card, _) = create_card(
        &f.store,
        &person(),
        NewCard {
            title: "Fix login".into(),
            workflow: "three".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let (run, _) = start_run_at(&f.env(), &person(), card.id, Some("b"), BTreeMap::new())
        .await
        .unwrap();
    assert_eq!(run.current_stage.as_deref(), Some("b"));
    assert_eq!(run.steps.len(), 1);
    f.drain().await.unwrap();
    assert!(root_of(&f, card.id, "b", 1).await.is_some());
    assert!(root_of(&f, card.id, "a", 1).await.is_none());
}

#[tokio::test]
async fn a_report_that_cannot_reach_the_next_agent_is_refused_and_changes_nothing() {
    let (f, card) = started("three").await;
    // The next stage's agent has left the update channel.
    let directory = buzz_hive_core::testing::FakeDirectory::new()
        .with_named_agent("builder", BUILDER, "aa")
        .with_channel(CHANNEL, &[common::HIVE]);
    let f = Fixture { directory, ..f };
    let error = f.report(&builder(), card, "passed", "").await.unwrap_err();
    assert!(refused(error).contains("add builder to the update channel"));
    let mut tx = f.store.begin(COMMUNITY).await.unwrap();
    let run = tx.runs().active_for_card(card).await.unwrap().unwrap();
    assert_eq!(run.current_stage.as_deref(), Some("a"));
    assert_eq!(run.state, RunState::Running);
    assert_eq!(run.steps.len(), 1);
}

async fn idle_card(f: &Fixture, workflow: &str) -> Uuid {
    let (card, _) = create_card(
        &f.store,
        &person(),
        NewCard {
            title: "sorted".into(),
            workflow: workflow.into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    card.id
}

fn move_to(card: Uuid, workflow: &str) -> CardMove {
    CardMove {
        card,
        workflow: workflow.into(),
    }
}

#[tokio::test]
async fn sorting_cards_is_all_or_nothing() {
    let f = Fixture::new();
    let (one, two) = (idle_card(&f, "").await, idle_card(&f, "").await);
    let (cards, events) = set_card_workflow(
        &f.env(),
        &person(),
        &[move_to(two, "three"), move_to(one, "held")],
    )
    .await
    .unwrap();
    assert_eq!(
        cards
            .iter()
            .map(|c| (c.id, c.workflow.as_str()))
            .collect::<Vec<_>>(),
        [(two, "three"), (one, "held")],
        "the cards come back in the order asked"
    );
    assert_eq!(kinds(&events), ["card.moved", "card.moved"]);
    assert_eq!(events[0].payload["from"], "");

    // A card already there appends nothing, and an empty workflow is Backlog.
    let (_, events) = set_card_workflow(&f.env(), &person(), &[move_to(two, "three")])
        .await
        .unwrap();
    assert!(events.is_empty());
    let (cards, _) = set_card_workflow(&f.env(), &person(), &[move_to(two, "")])
        .await
        .unwrap();
    assert_eq!(cards[0].workflow, "");

    // One refused card refuses the batch.
    let error = set_card_workflow(
        &f.env(),
        &person(),
        &[move_to(two, "three"), move_to(one, "ghost")],
    )
    .await
    .unwrap_err();
    assert!(matches!(error, OpError::NotFound(_)), "{error:?}");
    let mut tx = f.store.begin(COMMUNITY).await.unwrap();
    assert_eq!(tx.cards().get(two).await.unwrap().unwrap().workflow, "");
    drop(tx);

    let error = set_card_workflow(
        &f.env(),
        &person(),
        &[move_to(one, "three"), move_to(one, "held")],
    )
    .await
    .unwrap_err();
    assert!(matches!(error, OpError::InvalidArgument(_)), "{error:?}");
}

#[tokio::test]
async fn a_card_that_has_run_or_is_busy_cannot_be_sorted() {
    let (f, running) = started("three").await;
    let error = set_card_workflow(&f.env(), &person(), &[move_to(running, "held")])
        .await
        .unwrap_err();
    assert!(refused(error).contains("only an idle card"));

    // Done, then back to idle by hand: it has still run.
    let (f, done) = started("second").await;
    report(&f, done, "passed").await;
    buzz_hive_core::orchestrator::cards::update_card(
        &f.store,
        &person(),
        done,
        buzz_hive_core::orchestrator::cards::CardPatch {
            state: Some(CardState::Idle),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let error = set_card_workflow(&f.env(), &person(), &[move_to(done, "held")])
        .await
        .unwrap_err();
    assert!(refused(error).contains("has run"));
}
