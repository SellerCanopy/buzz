//! Documents and files handed between stages: who may save, what the next task names, and what
//! a stage that leaves out a required output does.
//!
//! Ported from the Go engine's `core/orchestrator` artifact tests (save, handoff, skipped and
//! missing outputs) and its `artifacts` and `files` specs where they apply to a card.

mod common;

use buzz_hive_core::access::ActorKind;
use buzz_hive_core::domain::artifact::{ArtifactContent, ArtifactKind, MAX_ARTIFACT_BYTES};
use buzz_hive_core::domain::card::CardState;
use buzz_hive_core::domain::events::Event;
use buzz_hive_core::domain::run::RunState;
use buzz_hive_core::orchestrator::activity::{
    get_brief, log_work, read_history, record_decision, Rejected,
};
use buzz_hive_core::orchestrator::artifacts::{
    list_artifacts, list_files, list_versions, read_artifact, save_artifact, set_handoff,
    update_plan,
};
use buzz_hive_core::orchestrator::cards::{create_card, NewCard};
use buzz_hive_core::orchestrator::outbox::drain_outbox;
use buzz_hive_core::orchestrator::run::{skip_stage, stage_done, start_run};
use buzz_hive_core::orchestrator::settings::{set_setting, Scope};
use buzz_hive_core::orchestrator::OpError;
use buzz_hive_core::ports::store::Store;
use common::{ctx_for, person, Fixture, BUILDER, CHANNEL, COMMUNITY};
use serde_json::json;
use uuid::Uuid;

fn builder() -> buzz_hive_core::orchestrator::Ctx {
    ctx_for(BUILDER, ActorKind::Agent)
}

/// A card on `workflow` whose run has started and whose first task has reached the relay.
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
            title: "Write the docs".into(),
            description: "Plain words.".into(),
            workflow: workflow.into(),
        },
    )
    .await
    .unwrap();
    start_run(&f.env(), &person(), card.id).await.unwrap();
    drain_outbox(&f.env(), COMMUNITY, &f.relay).await.unwrap();
    (f, card.id)
}

fn text(body: &str) -> ArtifactContent {
    ArtifactContent::Text(body.into())
}

fn file(media_ref: &str) -> ArtifactContent {
    ArtifactContent::File {
        sha256: "ab".repeat(32),
        size: 2048,
        media_type: "image/png".into(),
        media_ref: media_ref.into(),
    }
}

async fn save(f: &Fixture, card: Uuid, name: &str, content: ArtifactContent) -> Vec<Event> {
    save_artifact(&f.env(), &builder(), card, name, content)
        .await
        .unwrap()
        .1
}

fn invalid(error: OpError) -> String {
    match error {
        OpError::InvalidArgument(message) => message,
        other => panic!("want an invalid argument, got {other:?}"),
    }
}

fn precondition(error: OpError) -> String {
    match error {
        OpError::FailedPrecondition(message) => message,
        other => panic!("want a failed precondition, got {other:?}"),
    }
}

/// The text of the newest stage task that reached the relay.
fn last_task(f: &Fixture) -> String {
    f.relay
        .posts()
        .into_iter()
        .rfind(|p| p.mention.is_some())
        .expect("a stage task was posted")
        .text
}

async fn run_state(f: &Fixture, card: Uuid) -> RunState {
    let mut tx = f.store.begin(COMMUNITY).await.unwrap();
    tx.runs()
        .active_for_card(card)
        .await
        .unwrap()
        .unwrap()
        .state
}

async fn card_state(f: &Fixture, card: Uuid) -> CardState {
    let mut tx = f.store.begin(COMMUNITY).await.unwrap();
    tx.cards().get(card).await.unwrap().unwrap().state
}

#[tokio::test]
async fn a_save_appends_a_version_and_says_so_in_the_log() {
    let (f, card) = started("docs").await;
    let events = save(&f, card, "plan", text("- [ ] one")).await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, "artifact.saved");
    assert_eq!(events[0].payload["name"], "plan");
    assert_eq!(events[0].payload["version"], 1);
    assert_eq!(events[0].payload["kind"], "text");
    assert_eq!(events[0].payload["stage"], "draft");
    assert_eq!(events[0].payload["sha256"].as_str().unwrap().len(), 64);

    let (second, _) = save_artifact(&f.env(), &builder(), card, "plan", text("- [x] one"))
        .await
        .unwrap();
    assert_eq!(second.version, 2);
    let versions = list_versions(&f.env(), &person(), card, "plan")
        .await
        .unwrap();
    assert_eq!(
        versions.iter().map(|v| v.version).collect::<Vec<_>>(),
        [1, 2],
        "a save never rewrites a version"
    );
    let latest = read_artifact(&f.env(), &person(), card, "plan", 0)
        .await
        .unwrap();
    assert_eq!(latest.content, text("- [x] one"));
    let first = read_artifact(&f.env(), &person(), card, "plan", 1)
        .await
        .unwrap();
    assert_eq!(first.content, text("- [ ] one"));
    assert!(matches!(
        read_artifact(&f.env(), &person(), card, "plan", 3).await,
        Err(OpError::NotFound(_))
    ));
    assert!(matches!(
        read_artifact(&f.env(), &person(), card, "plan", -1).await,
        Err(OpError::InvalidArgument(_))
    ));
}

#[tokio::test]
async fn only_the_running_stages_agent_may_save_what_the_stage_declares() {
    let (f, card) = started("docs").await;
    // Not the stage's agent.
    let other = ctx_for("cc", ActorKind::Agent);
    let error = save_artifact(&f.env(), &other, card, "plan", text("x"))
        .await
        .unwrap_err();
    assert!(matches!(error, OpError::PermissionDenied(_)), "{error:?}");
    // A name the stage does not declare.
    let message = invalid(
        save_artifact(&f.env(), &builder(), card, "other", text("x"))
            .await
            .unwrap_err(),
    );
    assert!(
        message.contains("does not declare the artifact other")
            && message.contains("plan, logo, notes"),
        "{message}"
    );
    // The next stage's input is not this stage's output.
    assert!(matches!(
        save_artifact(&f.env(), &builder(), card, "review", text("x")).await,
        Err(OpError::InvalidArgument(_))
    ));
    // A name the store would refuse.
    for bad in ["Plan", "", "a b", "../x"] {
        assert!(
            matches!(
                save_artifact(&f.env(), &builder(), card, bad, text("x")).await,
                Err(OpError::InvalidArgument(_))
            ),
            "{bad:?}"
        );
    }
    // Nothing was saved by any of them.
    assert!(list_artifacts(&f.env(), &person(), card)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn a_card_with_no_run_in_progress_takes_no_save() {
    let f = Fixture::new();
    let (card, _) = create_card(
        &f.store,
        &person(),
        NewCard {
            title: "idle".into(),
            workflow: "docs".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let message = precondition(
        save_artifact(&f.env(), &builder(), card.id, "plan", text("x"))
            .await
            .unwrap_err(),
    );
    assert!(message.contains("no run in progress"), "{message}");
}

#[tokio::test]
async fn a_body_over_one_mebibyte_is_refused_and_one_mebibyte_is_kept() {
    let (f, card) = started("docs").await;
    let over = "x".repeat(MAX_ARTIFACT_BYTES + 1);
    let message = invalid(
        save_artifact(&f.env(), &builder(), card, "plan", text(&over))
            .await
            .unwrap_err(),
    );
    assert!(message.contains("artifact limit"), "{message}");
    let exact = "x".repeat(MAX_ARTIFACT_BYTES);
    let (v, _) = save_artifact(&f.env(), &builder(), card, "plan", text(&exact))
        .await
        .unwrap();
    assert_eq!(v.version, 1);
}

#[tokio::test]
async fn a_version_keeps_its_kind_and_a_file_needs_its_pointer() {
    let (f, card) = started("docs").await;
    save(&f, card, "logo", file("https://media.example.org/logo.png")).await;
    // A text version of a file artifact.
    let message = invalid(
        save_artifact(&f.env(), &builder(), card, "logo", text("x"))
            .await
            .unwrap_err(),
    );
    assert!(message.contains("is a file artifact"), "{message}");
    // A hash that is not 64 lowercase hex characters, a negative size, no type, no reference.
    let with = |sha: &str, size: i64, ty: &str, media_ref: &str| ArtifactContent::File {
        sha256: sha.into(),
        size,
        media_type: ty.into(),
        media_ref: media_ref.into(),
    };
    let good = "ab".repeat(32);
    for bad in [
        with("AB", 1, "image/png", "r"),
        with(&good, -1, "image/png", "r"),
        with(&good, 1, "", "r"),
        with(&good, 1, "image/png", ""),
    ] {
        assert!(
            matches!(
                save_artifact(&f.env(), &builder(), card, "logo", bad.clone()).await,
                Err(OpError::InvalidArgument(_))
            ),
            "{bad:?}"
        );
    }
    let files = list_files(&f.env(), &person(), card).await.unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].name, "logo");
    assert_eq!(files[0].content, file("https://media.example.org/logo.png"));
}

#[tokio::test]
async fn the_next_task_names_what_stage_one_saved() {
    let (f, card) = started("docs").await;
    save(&f, card, "plan", text("- [ ] write\n- [ ] ship")).await;
    save(&f, card, "logo", file("https://media.example.org/logo.png")).await;
    save(&f, card, "notes", text("scratch")).await;
    // The draft task told the agent what to save.
    let draft = last_task(&f);
    assert!(
        draft.contains("Outputs to save before you report a result:"),
        "{draft}"
    );
    assert!(
        draft.contains("- `plan`\n- `logo` (optional)\n- `notes` (optional)"),
        "{draft}"
    );
    assert!(draft.contains("buzz upload file --file <path>"), "{draft}");
    assert!(draft.contains(&format!("--channel {CHANNEL}")), "{draft}");
    assert!(draft.contains("kind `file`"), "{draft}");

    stage_done(&f.env(), &builder(), card, "passed", "drafted")
        .await
        .unwrap();
    drain_outbox(&f.env(), COMMUNITY, &f.relay).await.unwrap();
    let review = last_task(&f);
    assert!(review.contains("Input artifacts:"), "{review}");
    assert!(
        review.contains("- `plan` (text, version 1, 22 bytes):\n```\n- [ ] write\n- [ ] ship\n```"),
        "{review}"
    );
    // A stage with declared inputs reads those, not every artifact.
    assert!(
        !review.contains("`logo`") && !review.contains("`notes`"),
        "{review}"
    );
    assert!(!review.contains("Outputs to save"), "{review}");
    assert!(review.contains("stage_done"), "{review}");

    // Review declares no outputs. The stage after it declares no inputs, so it gets what is
    // handed on: the plan and the logo, not the notes, whose handoff is off.
    stage_done(&f.env(), &builder(), card, "passed", "")
        .await
        .unwrap();
    drain_outbox(&f.env(), COMMUNITY, &f.relay).await.unwrap();
    let ship = last_task(&f);
    assert!(ship.contains("- `plan` (text, version 1"), "{ship}");
    assert!(
        ship.contains("- `logo` (file, version 1, image/png, 2048 bytes): download it with `buzz media get https://media.example.org/logo.png -o <path>`."),
        "{ship}"
    );
    assert!(!ship.contains("`notes`"), "{ship}");
}

#[tokio::test]
async fn a_long_input_is_named_not_written_and_a_missing_one_is_said() {
    let (f, card) = started("docs").await;
    let long = "y".repeat(8 * 1024);
    save(&f, card, "plan", text(&long)).await;
    stage_done(&f.env(), &builder(), card, "passed", "")
        .await
        .unwrap();
    drain_outbox(&f.env(), COMMUNITY, &f.relay).await.unwrap();
    let review = last_task(&f);
    assert!(
        review.contains("- `plan` (text, version 1, 8 KiB): too long to write here. Read it with the Beehive tool `read_artifact`."),
        "{review}"
    );
    assert!(
        !review.contains(&long),
        "the body was written into the task"
    );
}

#[tokio::test]
async fn a_required_output_left_out_makes_the_run_wait_and_an_optional_one_does_not() {
    let (f, card) = started("docs").await;
    // plan is required, logo and notes are not.
    let (run, events) = stage_done(&f.env(), &builder(), card, "passed", "all done")
        .await
        .unwrap();
    assert_eq!(run.state, RunState::Waiting);
    assert_eq!(run.current_stage.as_deref(), Some("draft"));
    let kinds: Vec<&str> = events.iter().map(|e| e.kind.as_str()).collect();
    assert_eq!(kinds, ["result.reported", "run.waiting", "card.state"]);
    let waiting = events.iter().find(|e| e.kind == "run.waiting").unwrap();
    assert_eq!(waiting.payload["condition"], "missing_output");
    assert_eq!(waiting.payload["missing"], json!(["plan"]));
    assert!(
        waiting.payload["reason"]
            .as_str()
            .unwrap()
            .contains("reported passed without saving plan"),
        "{}",
        waiting.payload
    );
    assert_eq!(card_state(&f, card).await, CardState::Waiting);
    // No second stage task was queued.
    drain_outbox(&f.env(), COMMUNITY, &f.relay).await.unwrap();
    assert_eq!(
        f.relay
            .posts()
            .iter()
            .filter(|p| p.mention.is_some())
            .count(),
        1
    );

    // A person skips the stage, and the card goes on to review.
    let (run, _) = skip_stage(&f.env(), &person(), card).await.unwrap();
    assert_eq!(run.current_stage.as_deref(), Some("review"));
    assert_eq!(run.state, RunState::Running);
}

#[tokio::test]
async fn a_result_other_than_passed_does_not_need_the_outputs() {
    let (f, card) = started("docs").await;
    let (run, events) = stage_done(&f.env(), &builder(), card, "failed", "could not")
        .await
        .unwrap();
    assert!(events
        .iter()
        .all(|e| e.kind != "run.waiting" || e.payload.get("condition").is_none()));
    assert_ne!(run.current_stage.as_deref(), Some("review"));
}

#[tokio::test]
async fn a_saved_output_lets_the_stage_pass() {
    let (f, card) = started("docs").await;
    save(&f, card, "plan", text("x")).await;
    let (run, _) = stage_done(&f.env(), &builder(), card, "passed", "")
        .await
        .unwrap();
    assert_eq!(run.current_stage.as_deref(), Some("review"));
    assert_eq!(run_state(&f, card).await, RunState::Running);
}

#[tokio::test]
async fn an_output_saved_by_an_earlier_visit_does_not_count_for_this_one() {
    let (f, card) = started("docs").await;
    save(&f, card, "plan", text("v1")).await;
    // Another visit of the same stage, read straight from the store.
    let mut tx = f.store.begin(COMMUNITY).await.unwrap();
    assert_eq!(
        tx.artifacts().saved_in(card, "draft", 1).await.unwrap(),
        ["plan"]
    );
    assert!(tx
        .artifacts()
        .saved_in(card, "draft", 2)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn a_person_sets_handoff_and_the_log_says_so() {
    let (f, card) = started("docs").await;
    save(&f, card, "notes", text("scratch")).await;
    let infos = list_artifacts(&f.env(), &person(), card).await.unwrap();
    assert_eq!(infos.len(), 1);
    assert_eq!(
        (infos[0].kind, infos[0].latest, infos[0].handoff),
        (ArtifactKind::Text, 1, false),
        "the output declares handoff: false"
    );
    let (info, events) = set_handoff(&f.env(), &person(), card, "notes", true)
        .await
        .unwrap();
    assert!(info.handoff);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, "artifact.handoff");
    assert_eq!(
        events[0].payload,
        json!({ "name": "notes", "handoff": true })
    );
    assert!(matches!(
        set_handoff(&f.env(), &person(), card, "nope", true).await,
        Err(OpError::NotFound(_))
    ));

    // A save keeps what the person set.
    save(&f, card, "notes", text("more")).await;
    assert!(list_artifacts(&f.env(), &person(), card).await.unwrap()[0].handoff);
}

#[tokio::test]
async fn the_plan_is_counted_from_its_latest_version_and_update_plan_ticks_items() {
    let (f, card) = started("ticked").await;
    // Not saved yet.
    let message = precondition(
        update_plan(&f.env(), &builder(), card, "one", true)
            .await
            .unwrap_err(),
    );
    assert!(message.contains("has no saved version yet"), "{message}");

    save(&f, card, "todo", text("- [ ] one\n- [ ] two\n")).await;
    let (update, events) = update_plan(&f.env(), &builder(), card, "one", true)
        .await
        .unwrap();
    assert_eq!(
        (update.plan.as_str(), update.checked, update.total),
        ("todo", 1, 2)
    );
    assert_eq!(events[0].kind, "artifact.saved");
    assert_eq!(events[0].payload["checked"], 1);
    assert_eq!(events[0].payload["item"], "one");
    let latest = read_artifact(&f.env(), &person(), card, "todo", 0)
        .await
        .unwrap();
    assert_eq!(latest.version, 2);
    assert_eq!(latest.content, text("- [x] one\n- [ ] two\n"));

    // An item already in that state saves nothing.
    let (_, events) = update_plan(&f.env(), &builder(), card, "one", true)
        .await
        .unwrap();
    assert!(events.is_empty());
    assert_eq!(
        list_versions(&f.env(), &person(), card, "todo")
            .await
            .unwrap()
            .len(),
        2
    );

    // An unknown item lists the items there are.
    let message = invalid(
        update_plan(&f.env(), &builder(), card, "three", true)
            .await
            .unwrap_err(),
    );
    assert!(message.contains(r#""one", "two""#), "{message}");
    // An empty item and one over 1 KiB.
    assert!(matches!(
        update_plan(&f.env(), &builder(), card, "  ", true).await,
        Err(OpError::InvalidArgument(_))
    ));
    assert!(matches!(
        update_plan(&f.env(), &builder(), card, &"x".repeat(1025), true).await,
        Err(OpError::InvalidArgument(_))
    ));

    // The round that just ended left one item open, so the stage runs another round. When every
    // item is checked, the next passed ends the stage.
    let (run, _) = stage_done(&f.env(), &builder(), card, "passed", "round 1")
        .await
        .unwrap();
    assert_eq!(run.state, RunState::Running);
    assert_eq!(run.steps.last().unwrap().round, 2, "{run:?}");
    update_plan(&f.env(), &builder(), card, "two", true)
        .await
        .unwrap();
    let (run, _) = stage_done(&f.env(), &builder(), card, "passed", "round 2")
        .await
        .unwrap();
    assert_eq!(run.state, RunState::Ended, "{run:?}");
}

#[tokio::test]
async fn a_stage_with_no_plan_has_nothing_to_update() {
    let (f, card) = started("docs").await;
    let message = precondition(
        update_plan(&f.env(), &builder(), card, "one", true)
            .await
            .unwrap_err(),
    );
    assert!(message.contains("has no plan to update"), "{message}");
}

#[tokio::test]
async fn work_and_decisions_become_the_brief_and_the_history() {
    let (f, card) = started("docs").await;
    let work = log_work(
        &f.env(),
        &builder(),
        card,
        "Wrote the outline.",
        "Three parts.",
        "",
    )
    .await
    .unwrap();
    let decision = record_decision(
        &f.env(),
        &builder(),
        card,
        "Kept it short.",
        "People skim.",
        &[Rejected {
            option: "Long form".into(),
            reason: "nobody reads it".into(),
        }],
        "",
        0,
    )
    .await
    .unwrap();
    assert!(decision.seq > work.seq);
    assert_eq!(work.kind, "activity.added");
    assert_eq!(work.payload["stage"], "draft");

    let brief = get_brief(&f.env(), &person(), card, "").await.unwrap();
    assert!(
        brief.starts_with("Earlier sessions worked on this stage."),
        "{brief}"
    );
    assert!(
        brief
            .contains("Kept it short. Why: People skim. Turned down: Long form (nobody reads it)."),
        "{brief}"
    );
    assert!(
        brief.contains("- Wrote the outline.\n  Three parts."),
        "{brief}"
    );
    // Another stage has none of it.
    assert_eq!(
        get_brief(&f.env(), &person(), card, "review")
            .await
            .unwrap(),
        ""
    );

    // The history pages by seq.
    let all = read_history(&f.env(), &person(), card, 0, 0).await.unwrap();
    assert_eq!(all.len(), 2);
    let rest = read_history(&f.env(), &person(), card, work.seq, 1)
        .await
        .unwrap();
    assert_eq!(rest.len(), 1);
    assert_eq!(rest[0].seq, decision.seq);
    assert!(matches!(
        read_history(&f.env(), &person(), card, 0, 501).await,
        Err(OpError::InvalidArgument(_))
    ));

    // Entries are checked.
    for summary in ["", "two\nlines", &"x".repeat(121)] {
        assert!(
            matches!(
                log_work(&f.env(), &builder(), card, summary, "", "").await,
                Err(OpError::InvalidArgument(_))
            ),
            "{summary:?}"
        );
    }
    assert!(matches!(
        record_decision(&f.env(), &builder(), card, "ok", "", &[], "", 0).await,
        Err(OpError::InvalidArgument(_))
    ));
    // Only the stage's agent logs.
    let other = ctx_for("cc", ActorKind::Agent);
    assert!(matches!(
        log_work(&f.env(), &other, card, "x", "", "").await,
        Err(OpError::PermissionDenied(_))
    ));
}
