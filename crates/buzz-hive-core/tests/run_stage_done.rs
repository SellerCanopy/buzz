//! A stage's agent reports a result: the reducer, the identity check and what is refused.

mod common;

use buzz_hive_core::access::ActorKind;
use buzz_hive_core::domain::card::CardState;
use buzz_hive_core::domain::run::{Run, RunState};
use buzz_hive_core::orchestrator::cards::{create_card, NewCard};
use buzz_hive_core::orchestrator::run::{stage_done, start_run};
use buzz_hive_core::orchestrator::settings::{set_setting, Scope};
use buzz_hive_core::orchestrator::task_message::{card_link, DESCRIPTION_CUT, MAX_POST_BYTES};
use buzz_hive_core::orchestrator::OpError;
use buzz_hive_core::ports::store::Store;
use buzz_hive_core::rules::run::{step, Effect, Ended, Facts, Input};
use common::{catalog, ctx_for, person, Fixture, BUILDER, CHANNEL, COMMUNITY, HIVE};
use serde_json::json;
use uuid::Uuid;

fn builder() -> buzz_hive_core::orchestrator::Ctx {
    ctx_for(BUILDER, ActorKind::Agent)
}

/// A card on `workflow` whose run has started.
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
    (f, card.id)
}

fn kinds(events: &[buzz_hive_core::domain::events::Event]) -> Vec<&str> {
    events.iter().map(|e| e.kind.as_str()).collect()
}

#[test]
fn the_reducer_routes_a_result_and_ends_only_on_a_passed_last_stage() {
    let catalog = catalog();
    let wf = catalog.get("demo").unwrap();
    let facts = Facts::default();
    let mut run = Run::new(Uuid::from_u128(5), "demo", &wf.hash());
    run = step(
        &run,
        wf,
        &facts,
        Input::Start {
            requester: None,
            stage: None,
            held: false,
        },
    )
    .run;
    assert_eq!(run.current_stage.as_deref(), Some("research"));

    let done = |stage: &str, result: &str| Input::StageDone {
        stage: stage.into(),
        result: result.into(),
        summary: "s".into(),
    };

    // A result from a stage that is not the current one changes nothing.
    let stale = step(&run, wf, &facts, done("build", "passed"));
    assert_eq!(stale.run, run);
    assert!(stale.effects.is_empty());

    // Passed from a stage that is not the last enters the next one, in a new thread.
    let first = step(&run, wf, &facts, done("research", "passed"));
    assert_eq!(first.run.current_stage.as_deref(), Some("build"));
    assert_eq!(first.run.state, RunState::Running);
    let emitted: Vec<&str> = first
        .effects
        .iter()
        .map(|e| match e {
            Effect::Emit { kind, .. } => *kind,
            Effect::PostStageTask { stage, .. } => {
                assert_eq!(stage, "build");
                "task"
            }
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(emitted, ["result.reported", "stage.routed", "task"]);

    // The last stage ends the run only when it passed. Anything else waits for a person.
    let last = first.run;
    let failed = step(&last, wf, &facts, done("build", "failed"));
    assert_eq!(failed.run.state, RunState::Waiting);
    assert!(failed.reason.unwrap().contains("reported failed"));
    let passed = step(&last, wf, &facts, done("build", "passed"));
    assert_eq!(passed.run.state, RunState::Ended);
    assert_eq!(passed.ended, Some(Ended::Done));
    let emitted: Vec<(&str, serde_json::Value)> = passed
        .effects
        .iter()
        .map(|e| match e {
            Effect::Emit { kind, payload } => (*kind, payload.clone()),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(emitted[0].0, "result.reported");
    assert_eq!(emitted[1].0, "run.ended");
    assert_eq!(emitted[1].1["outcome"], "passed");

    // An ended run takes no further result.
    let again = step(&passed.run, wf, &facts, done("build", "passed"));
    assert!(again.effects.is_empty());
}

#[tokio::test]
async fn a_passed_last_stage_ends_the_run_and_moves_the_card_to_done() {
    let (f, card_id) = started("solo").await;
    let (run, events) = f
        .report(&builder(), card_id, "passed", "all green")
        .await
        .unwrap();
    assert_eq!(run.state, RunState::Ended);
    assert!(run.ended_at.is_some());
    assert_eq!(
        kinds(&events),
        ["result.reported", "run.ended", "card.state"]
    );
    assert!(events.iter().all(|e| e.run_id == Some(run.id)));
    assert!(events.iter().all(|e| e.actor.pubkey == BUILDER));
    let reported = &events[0].payload;
    assert_eq!(reported["result"], "passed");
    assert_eq!(reported["summary"], "all green");
    assert_eq!(events[2].payload["state"], "done");

    let mut tx = f.store.begin(COMMUNITY).await.unwrap();
    let card = tx.cards().get(card_id).await.unwrap().unwrap();
    assert_eq!(card.state, CardState::Done);
    assert!(tx.runs().active_for_card(card_id).await.unwrap().is_none());
}

#[tokio::test]
async fn the_report_works_after_the_task_has_reached_the_relay() {
    let (f, card_id) = started("solo").await;
    f.drain().await.unwrap();
    let (run, _) = f.report(&builder(), card_id, "passed", "").await.unwrap();
    assert_eq!(run.state, RunState::Ended);
}

#[tokio::test]
async fn a_result_with_no_route_waits_for_a_person_and_the_card_waits() {
    let (f, card_id) = started("solo").await;
    let (run, events) = f
        .report(&builder(), card_id, "failed", "no luck")
        .await
        .unwrap();
    assert_eq!(run.state, RunState::Waiting);
    assert_eq!(
        kinds(&events),
        ["result.reported", "run.waiting", "card.state"]
    );
    assert_eq!(events[2].payload["state"], "waiting");
    assert!(events[2].payload["reason"]
        .as_str()
        .unwrap()
        .contains("reported failed"));
    let mut tx = f.store.begin(COMMUNITY).await.unwrap();
    assert_eq!(
        tx.cards().get(card_id).await.unwrap().unwrap().state,
        CardState::Waiting
    );
    drop(tx);
    // The agent has reported, so a second report is refused until a person moves the card.
    let error = f
        .report(&builder(), card_id, "passed", "")
        .await
        .unwrap_err();
    assert!(
        matches!(&error, OpError::FailedPrecondition(m) if m.contains("run is waiting")),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_passed_stage_that_is_not_the_last_enters_the_next_in_a_new_thread() {
    let (f, card_id) = started("demo").await;
    let (run, events) = f.report(&builder(), card_id, "passed", "").await.unwrap();
    assert_eq!(run.state, RunState::Running);
    assert_eq!(run.current_stage.as_deref(), Some("build"));
    assert_eq!(kinds(&events), ["result.reported", "stage.routed"]);
    let mut tx = f.store.begin(COMMUNITY).await.unwrap();
    let keys: Vec<String> = tx
        .outbox()
        .all()
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.key)
        .collect();
    assert_eq!(
        keys,
        [
            format!("kiln:{card_id}:research:1:status"),
            format!("kiln:{card_id}:research:1"),
            format!("kiln:{card_id}:build:1"),
        ]
    );
}

#[tokio::test]
async fn only_the_agent_the_task_mentioned_may_report() {
    let (f, card_id) = started("solo").await;
    for who in [
        person(),
        ctx_for("b2", ActorKind::Agent),
        ctx_for(HIVE, ActorKind::Agent),
    ] {
        let error = f.report(&who, card_id, "passed", "").await.unwrap_err();
        match error {
            OpError::PermissionDenied(message) => {
                assert!(message.contains("only the agent builder"), "{message}")
            }
            other => panic!("want permission denied, got {other:?}"),
        }
    }
    // Nothing was applied.
    let mut tx = f.store.begin(COMMUNITY).await.unwrap();
    assert_eq!(
        tx.cards().get(card_id).await.unwrap().unwrap().state,
        CardState::Running
    );
}

#[tokio::test]
async fn a_result_the_stage_does_not_allow_lists_the_ones_it_does() {
    let (f, card_id) = started("solo").await;
    for result in ["bogus", "needs_work", "Passed"] {
        let error = f.report(&builder(), card_id, result, "").await.unwrap_err();
        match error {
            OpError::InvalidArgument(message) => assert!(
                message.contains("passed, failed, needs_input"),
                "{result}: {message}"
            ),
            other => panic!("want invalid argument, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn an_over_long_summary_is_refused() {
    let (f, card_id) = started("solo").await;
    let error = f
        .report(&builder(), card_id, "passed", &"x".repeat(10_001))
        .await
        .unwrap_err();
    assert!(matches!(error, OpError::InvalidArgument(_)), "{error:?}");
    let (run, _) = f
        .report(&builder(), card_id, "passed", &"x".repeat(10_000))
        .await
        .unwrap();
    assert_eq!(run.state, RunState::Ended);
}

#[tokio::test]
async fn a_stale_or_early_report_is_refused_with_a_sentence() {
    let (f, card_id) = started("solo").await;
    f.report(&builder(), card_id, "passed", "").await.unwrap();
    // The run has ended, so a second report finds nothing waiting.
    let error = f
        .report(&builder(), card_id, "passed", "")
        .await
        .unwrap_err();
    match error {
        OpError::FailedPrecondition(message) => {
            assert!(message.contains("no run in progress"), "{message}")
        }
        other => panic!("{other:?}"),
    }

    // A card that never started has no run.
    let (card, _) = create_card(
        &f.store,
        &person(),
        NewCard {
            title: "idle".into(),
            workflow: "solo".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let error = f
        .report(&builder(), card.id, "passed", "")
        .await
        .unwrap_err();
    assert!(matches!(error, OpError::FailedPrecondition(_)), "{error:?}");

    // A card that does not exist.
    let error = f
        .report(&builder(), Uuid::from_u128(9), "passed", "")
        .await
        .unwrap_err();
    assert!(matches!(error, OpError::NotFound(_)), "{error:?}");
}

#[tokio::test]
async fn a_report_must_name_its_stage_and_visit() {
    let (f, card_id) = started("solo").await;
    for (stage, visit) in [("", 1), ("only", 0)] {
        let error = stage_done(&f.env(), &builder(), card_id, stage, visit, "passed", "")
            .await
            .unwrap_err();
        assert!(
            matches!(error, OpError::InvalidArgument(_)),
            "{stage:?} {visit}: {error:?}"
        );
    }
    // Nothing was applied.
    assert_eq!(f.at(card_id).await, ("only".to_owned(), 1));
}

#[tokio::test]
async fn a_report_for_an_earlier_stage_or_visit_is_refused_and_changes_nothing() {
    // `b` sends a failure back to `a`, so `a` is visited twice.
    let (f, card_id) = started("loop").await;
    f.report(&builder(), card_id, "passed", "").await.unwrap();
    assert_eq!(f.at(card_id).await, ("b".to_owned(), 1));
    f.report(&builder(), card_id, "failed", "").await.unwrap();
    assert_eq!(f.at(card_id).await, ("a".to_owned(), 2));

    // The late answer to the first visit of `a`, and a report for the stage after it.
    for (stage, visit) in [("a", 1), ("b", 1), ("b", 2), ("a", 3)] {
        let error = stage_done(&f.env(), &builder(), card_id, stage, visit, "passed", "")
            .await
            .unwrap_err();
        match error {
            OpError::FailedPrecondition(message) => assert!(
                message.contains(&format!("stage {stage} visit {visit}"))
                    && message.contains("stage a visit 2"),
                "{message}"
            ),
            other => panic!("{stage} {visit}: want failed precondition, got {other:?}"),
        }
    }
    assert_eq!(f.at(card_id).await, ("a".to_owned(), 2));

    let (run, _) = stage_done(&f.env(), &builder(), card_id, "a", 2, "passed", "")
        .await
        .unwrap();
    assert_eq!(run.current_stage.as_deref(), Some("b"));
}

#[tokio::test]
async fn a_stranger_learns_nothing_about_where_the_run_is() {
    // The caller is checked before the position, so a wrong stage from a caller who is not
    // the stage's agent is a permission error, not a hint at the run's stage.
    let (f, card_id) = started("solo").await;
    let error = stage_done(&f.env(), &person(), card_id, "elsewhere", 9, "passed", "")
        .await
        .unwrap_err();
    assert!(matches!(error, OpError::PermissionDenied(_)), "{error:?}");
}

#[tokio::test]
async fn a_card_with_the_longest_description_still_gets_a_task_that_ends_in_the_instruction() {
    for unit in ["x", "\u{e9}", "\u{1f41d}"] {
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
                description: unit.repeat(65_536),
                workflow: "solo".into(),
            },
        )
        .await
        .unwrap();
        start_run(&f.env(), &person(), card.id).await.unwrap();
        let mut tx = f.store.begin(COMMUNITY).await.unwrap();
        let rows = tx.outbox().all().await.unwrap();
        let text = match &rows[1].posting {
            buzz_hive_core::ports::store::Posting::StageTask { text, .. } => text.clone(),
            other => panic!("{other:?}"),
        };
        // The relay takes 64 KiB, so nothing is cut after this: the whole task arrives.
        assert!(text.len() <= MAX_POST_BYTES, "{unit}: {} bytes", text.len());
        assert!(text.contains(DESCRIPTION_CUT), "{unit}");
        let instruction = format!(
            "`stage_done` with card_id `{}`, stage `only`, visit 1 and result one of: passed, failed, needs_input.",
            card.id
        );
        assert!(text.contains(&instruction), "{unit}");
        assert!(
            text.ends_with(&card_link(card.id)),
            "{unit}: the card link is last"
        );
    }
}

#[tokio::test]
async fn the_stage_task_tells_the_agent_which_card_and_which_results() {
    let (f, card_id) = started("solo").await;
    let mut tx = f.store.begin(COMMUNITY).await.unwrap();
    let rows = tx.outbox().all().await.unwrap();
    let text = match &rows[1].posting {
        buzz_hive_core::ports::store::Posting::StageTask { text, .. } => text.clone(),
        other => panic!("{other:?}"),
    };
    assert!(
        text.contains(&format!("card_id `{card_id}`")) && text.contains("`stage_done`"),
        "{text}"
    );
    assert!(
        text.contains("stage `only`, visit 1 and result one of: passed"),
        "{text}"
    );
    assert!(text.contains("passed, failed, needs_input"), "{text}");
}
