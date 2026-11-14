//! Starting a run: the reducer, the outbox and what a person sees when it is refused.

mod common;

use buzz_hive_core::domain::card::CardState;
use buzz_hive_core::domain::run::RunState;
use buzz_hive_core::orchestrator::cards::{create_card, NewCard};
use buzz_hive_core::orchestrator::outbox::drain_outbox;
use buzz_hive_core::orchestrator::run::start_run;
use buzz_hive_core::orchestrator::settings::{set_setting, Scope};
use buzz_hive_core::orchestrator::OpError;
use buzz_hive_core::ports::store::{Posting, Store};
use buzz_hive_core::rules::run::{entry_stage, step, task_key, Effect, Input};
use buzz_hive_core::testing::{FakeDirectory, FakeRelay};
use common::{catalog, person, Fixture, BUILDER, CHANNEL, COMMUNITY, HIVE};
use serde_json::json;

async fn card_on(f: &Fixture, workflow: &str) -> uuid::Uuid {
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
    card.id
}

async fn set_channel(f: &Fixture) {
    set_setting(
        &f.env(),
        &person(),
        &Scope::User,
        "update_channel",
        Some(json!(CHANNEL.to_string())),
    )
    .await
    .unwrap();
}

async fn ready() -> (Fixture, uuid::Uuid) {
    let f = Fixture::new();
    set_channel(&f).await;
    let card = card_on(&f, "demo").await;
    (f, card)
}

fn refused(error: OpError) -> String {
    match error {
        OpError::FailedPrecondition(message) => message,
        other => panic!("want a failed precondition, got {other:?}"),
    }
}

#[test]
fn the_reducer_starts_a_created_run_and_ignores_every_other_state() {
    let catalog = catalog();
    let wf = catalog.get("demo").unwrap();
    let card = uuid::Uuid::from_u128(5);
    let run = buzz_hive_core::domain::run::Run::new(card, "demo", &wf.hash());

    let started = step(
        &run,
        wf,
        Input::Start {
            requester: Some("aa".into()),
        },
    );
    assert_eq!(started.run.state, RunState::Running);
    assert_eq!(started.run.current_stage.as_deref(), Some("research"));
    assert_eq!(started.run.visit, 1);
    assert_eq!(started.run.requester.as_deref(), Some("aa"));
    let key = task_key(card, "research", 1);
    assert_eq!(key, format!("kiln:{card}:research:1"));
    let kinds: Vec<&str> = started
        .effects
        .iter()
        .map(|e| match e {
            Effect::Emit { kind, .. } => *kind,
            Effect::PostStatus { .. } => "status",
            Effect::PostStageTask { .. } => "task",
        })
        .collect();
    assert_eq!(kinds, ["run.started", "status", "task"]);
    match &started.effects[2] {
        Effect::PostStageTask {
            stage,
            visit,
            agent,
            key: k,
            ..
        } => assert_eq!(
            (stage.as_str(), *visit, agent.as_str(), k),
            ("research", 1, "builder", &key)
        ),
        other => panic!("{other:?}"),
    }

    // A run that has started is unchanged, so a repeated Start posts nothing twice.
    let again = step(&started.run, wf, Input::Start { requester: None });
    assert_eq!(again.run, started.run);
    assert!(again.effects.is_empty());

    // A visit counts the stage's earlier visits.
    let mut revisit = run.clone();
    revisit.visit = 2;
    let third = step(&revisit, wf, Input::Start { requester: None });
    assert_eq!(third.run.visit, 3);
}

#[test]
fn the_entry_stage_needs_a_prompt_stage_with_an_agent() {
    let catalog = catalog();
    assert_eq!(
        entry_stage(catalog.get("demo").unwrap()).unwrap().1,
        "builder"
    );
    let message = entry_stage(catalog.get("noagent").unwrap()).unwrap_err();
    assert!(message.contains("names no agent"), "{message}");
}

#[tokio::test]
async fn start_run_queues_the_status_and_the_task_and_moves_the_card_to_running() {
    let (f, card_id) = ready().await;
    let (run, events) = start_run(&f.env(), &person(), card_id).await.unwrap();
    assert_eq!(run.state, RunState::Running);
    let kinds: Vec<&str> = events.iter().map(|e| e.kind.as_str()).collect();
    assert_eq!(kinds, ["card.state", "run.started"]);
    assert!(events.iter().all(|e| e.run_id == Some(run.id)));

    let mut tx = f.store.begin(COMMUNITY).await.unwrap();
    let card = tx.cards().get(card_id).await.unwrap().unwrap();
    assert_eq!(card.state, CardState::Running);
    let rows = tx.outbox().all().await.unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].key, format!("kiln:{card_id}:research:1:status"));
    assert_eq!(rows[1].key, format!("kiln:{card_id}:research:1"));
    match &rows[0].posting {
        Posting::Status { channel, text } => {
            assert_eq!(*channel, CHANNEL);
            assert_eq!(text, "Fix login: run started on demo, at stage Research.");
        }
        other => panic!("{other:?}"),
    }
    match &rows[1].posting {
        Posting::StageTask {
            agent,
            agent_name,
            text,
            ..
        } => {
            assert_eq!((agent.as_str(), agent_name.as_str()), (BUILDER, "builder"));
            assert!(
                text.contains("Research Fix login at depth normal"),
                "the task renders {{issue.title}} and the package default of params.depth:\n{text}"
            );
            assert!(text.starts_with("Fix login\n\nIt breaks.\n\n"), "{text}");
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn draining_posts_the_status_then_the_task_and_records_the_thread() {
    let (f, card_id) = ready().await;
    start_run(&f.env(), &person(), card_id).await.unwrap();
    let drained = drain_outbox(&f.env(), COMMUNITY, &f.relay).await.unwrap();
    assert_eq!((drained.posted, drained.failed), (2, 0));
    let posts = f.relay.posts();
    assert_eq!(posts.len(), 2);
    assert_eq!(posts[0].mention, None);
    assert_eq!(posts[1].mention.as_deref(), Some(BUILDER));
    assert_eq!(posts[1].channel, CHANNEL);
    let kinds: Vec<&str> = drained.events.iter().map(|e| e.kind.as_str()).collect();
    assert_eq!(kinds, ["stage.posted"]);

    let mut tx = f.store.begin(COMMUNITY).await.unwrap();
    let thread = tx
        .runs()
        .thread(card_id, "research", 1)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(thread.root_event_id, posts[1].event_id);
    assert_eq!(thread.agent_pubkey, BUILDER);
    assert!(tx
        .outbox()
        .all()
        .await
        .unwrap()
        .iter()
        .all(|r| r.done_at.is_some()));

    // Nothing left to send.
    let again = drain_outbox(&f.env(), COMMUNITY, &f.relay).await.unwrap();
    assert_eq!(again.posted, 0);
    assert_eq!(f.relay.posts().len(), 2);
}

#[tokio::test]
async fn a_failed_post_is_retried_with_the_same_key_and_the_relay_records_one_post() {
    let (mut f, card_id) = ready().await;
    f.relay = FakeRelay::new().failing_first(1);
    start_run(&f.env(), &person(), card_id).await.unwrap();

    let first = drain_outbox(&f.env(), COMMUNITY, &f.relay).await.unwrap();
    assert_eq!((first.posted, first.failed), (0, 1));
    assert!(first.first_error.unwrap().contains("the relay is down"));
    // The status failed, so the task of the same card did not overtake it.
    assert_eq!(f.relay.calls().len(), 1);
    {
        let mut tx = f.store.begin(COMMUNITY).await.unwrap();
        let rows = tx.outbox().all().await.unwrap();
        assert_eq!(rows[0].attempts, 1);
        assert!(rows[0]
            .last_error
            .as_deref()
            .unwrap()
            .contains("relay is down"));
        assert!(rows.iter().all(|r| r.done_at.is_none()));
    }

    let second = drain_outbox(&f.env(), COMMUNITY, &f.relay).await.unwrap();
    assert_eq!((second.posted, second.failed), (2, 0));
    let posts = f.relay.posts();
    assert_eq!(posts.len(), 2, "one post per key");
    let calls = f.relay.calls();
    assert_eq!(
        calls[0], calls[1],
        "the retry used the key of the failed call"
    );
}

#[tokio::test]
async fn a_post_repeated_after_a_crash_is_the_same_relay_event() {
    // The post reached the relay, then the process died before the row was marked done.
    let (f, card_id) = ready().await;
    start_run(&f.env(), &person(), card_id).await.unwrap();
    drain_outbox(&f.env(), COMMUNITY, &f.relay).await.unwrap();
    let before = f.relay.posts();

    let mut tx = f.store.begin(COMMUNITY).await.unwrap();
    let rows = tx.outbox().all().await.unwrap();
    drop(tx);
    // A second relay handle that already holds the events answers the repeat with their ids.
    use buzz_hive_core::ports::relay::{Delivery, Relay};
    let task = &rows[1];
    let Posting::StageTask {
        channel,
        agent,
        text,
        ..
    } = &task.posting
    else {
        panic!("{:?}", task.posting)
    };
    let delivery = Delivery {
        key: task.key.clone(),
        at: task.created_at,
    };
    let repeat = f
        .relay
        .post_stage_task(*channel, agent, text, &delivery)
        .await
        .unwrap();
    assert_eq!(repeat, before[1].event_id);
    assert_eq!(f.relay.posts(), before);
}

#[tokio::test]
async fn a_second_start_of_a_running_card_is_refused() {
    let (f, card_id) = ready().await;
    start_run(&f.env(), &person(), card_id).await.unwrap();
    let message = refused(start_run(&f.env(), &person(), card_id).await.unwrap_err());
    assert!(message.contains("already has a run"), "{message}");
    let mut tx = f.store.begin(COMMUNITY).await.unwrap();
    assert_eq!(tx.outbox().all().await.unwrap().len(), 2);
}

#[tokio::test]
async fn a_run_after_an_ended_one_gets_the_next_visit_and_posts_again() {
    let (f, card_id) = ready().await;
    let (first, _) = start_run(&f.env(), &person(), card_id).await.unwrap();
    {
        let mut tx = f.store.begin(COMMUNITY).await.unwrap();
        let mut ended = first.clone();
        ended.state = RunState::Ended;
        tx.runs().update(&ended).await.unwrap();
        tx.commit().await.unwrap();
    }
    let (second, _) = start_run(&f.env(), &person(), card_id).await.unwrap();
    assert_eq!(second.visit, 2);
    drain_outbox(&f.env(), COMMUNITY, &f.relay).await.unwrap();
    let keys: Vec<String> = f.relay.posts().into_iter().map(|p| p.key).collect();
    assert_eq!(keys.len(), 4, "{keys:?}");
    assert!(keys.contains(&format!("kiln:{card_id}:research:2")));
}

#[tokio::test]
async fn a_start_with_no_update_channel_names_the_setting() {
    let f = Fixture::new();
    let card = card_on(&f, "demo").await;
    let message = refused(start_run(&f.env(), &person(), card).await.unwrap_err());
    assert!(message.contains("update_channel"), "{message}");
}

#[tokio::test]
async fn a_start_with_an_unknown_agent_names_the_agent() {
    let f = Fixture::new().with_directory(FakeDirectory::new().with_channel(CHANNEL, &[HIVE]));
    set_channel(&f).await;
    let card = card_on(&f, "demo").await;
    let message = refused(start_run(&f.env(), &person(), card).await.unwrap_err());
    assert!(
        message.contains("no Buzz agent is named builder"),
        "{message}"
    );
}

#[tokio::test]
async fn two_agents_with_one_name_are_refused_with_both_pubkeys() {
    let directory = FakeDirectory::new()
        .with_named_agent("Builder", "b1", "aa")
        .with_named_agent("builder", "b2", "aa")
        .with_channel(CHANNEL, &[HIVE, "b1", "b2"]);
    let f = Fixture::new().with_directory(directory);
    set_channel(&f).await;
    let card = card_on(&f, "demo").await;
    let message = refused(start_run(&f.env(), &person(), card).await.unwrap_err());
    assert!(
        message.contains("b1") && message.contains("b2"),
        "{message}"
    );
}

#[tokio::test]
async fn an_agent_outside_the_channel_is_named_in_the_refusal() {
    let directory = FakeDirectory::new()
        .with_named_agent("builder", BUILDER, "aa")
        .with_channel(CHANNEL, &[HIVE]);
    let f = Fixture::new().with_directory(directory);
    set_channel(&f).await;
    let card = card_on(&f, "demo").await;
    let message = refused(start_run(&f.env(), &person(), card).await.unwrap_err());
    assert!(
        message.contains("add builder to the update channel"),
        "{message}"
    );
}

#[tokio::test]
async fn a_card_with_no_workflow_or_a_stage_with_no_agent_is_refused() {
    let (f, _) = ready().await;
    let backlog = card_on(&f, "").await;
    let message = refused(start_run(&f.env(), &person(), backlog).await.unwrap_err());
    assert!(message.contains("no workflow"), "{message}");
    let noagent = card_on(&f, "noagent").await;
    let message = refused(start_run(&f.env(), &person(), noagent).await.unwrap_err());
    assert!(message.contains("names no agent"), "{message}");
    let missing = card_on(&f, "gone").await;
    let message = refused(start_run(&f.env(), &person(), missing).await.unwrap_err());
    assert!(
        message.contains("workflow gone does not exist"),
        "{message}"
    );
}

#[tokio::test]
async fn a_refused_start_leaves_no_run_post_or_card_change() {
    let f = Fixture::new();
    let card = card_on(&f, "demo").await;
    start_run(&f.env(), &person(), card).await.unwrap_err();
    let mut tx = f.store.begin(COMMUNITY).await.unwrap();
    assert!(tx.runs().list(None).await.unwrap().is_empty());
    assert!(tx.outbox().all().await.unwrap().is_empty());
    assert_eq!(
        tx.cards().get(card).await.unwrap().unwrap().state,
        CardState::Idle
    );
}
