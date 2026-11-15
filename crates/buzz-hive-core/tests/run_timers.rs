//! Timeouts, retries, stopping a run and restarting the service, with a clock the test moves.

mod common;

use std::time::Duration;

use buzz_hive_core::access::ActorKind;
use buzz_hive_core::domain::card::CardState;
use buzz_hive_core::domain::events::Event;
use buzz_hive_core::domain::run::RunState;
use buzz_hive_core::domain::timer::TimerKind;
use buzz_hive_core::orchestrator::cards::{create_card, NewCard};
use buzz_hive_core::orchestrator::run::{
    pause_all, pause_card, reset_card, resume_card, retry_run, stop_run,
};
use buzz_hive_core::orchestrator::settings::{set_setting, Scope};
use buzz_hive_core::orchestrator::{timers, OpError};
use buzz_hive_core::ports::store::Store;
use buzz_hive_core::rules::run::{step, Execution, Facts, Input};
use buzz_hive_core::testing::ManualClock;
use common::{catalog, ctx_for, person, Fixture, BUILDER, CHANNEL, COMMUNITY};
use serde_json::json;
use uuid::Uuid;

const MINUTE: Duration = Duration::from_secs(60);

async fn set(f: &Fixture, key: &str, value: serde_json::Value) {
    set_setting(&f.env(), &person(), &Scope::User, key, Some(value))
        .await
        .unwrap();
}

/// A fixture whose update channel is set and whose stages time out after `timeout`, retry
/// `retries` times and wait `backoff` before the first retry.
async fn configured(timeout: &str, retries: u64, backoff: &str) -> (Fixture, ManualClock) {
    let (f, clock) = Fixture::timed();
    set(&f, "update_channel", json!(CHANNEL.to_string())).await;
    set(&f, "execution.timeout", json!(timeout)).await;
    set(&f, "execution.retries.max", json!(retries)).await;
    set(&f, "execution.retries.backoff", json!(backoff)).await;
    (f, clock)
}

/// A card on `workflow` whose run has started and whose posts reached the relay.
async fn begun(f: &Fixture, workflow: &str) -> Uuid {
    let (card, _) = create_card(
        &f.store,
        &person(),
        NewCard {
            title: workflow.into(),
            workflow: workflow.into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    buzz_hive_core::orchestrator::run::start_run(&f.env(), &person(), card.id)
        .await
        .unwrap();
    f.drain().await.unwrap();
    card.id
}

/// Fires every timer that is due, and returns the events they committed.
async fn fire_due(f: &Fixture) -> Vec<Event> {
    let mut events = Vec::new();
    for timer in timers::due(&f.store, COMMUNITY, 100).await.unwrap() {
        events.extend(timers::fire(&f.env(), COMMUNITY, &timer).await.unwrap());
    }
    events
}

async fn card_state(f: &Fixture, card: Uuid) -> CardState {
    let mut tx = f.store.begin(COMMUNITY).await.unwrap();
    tx.cards().get(card).await.unwrap().unwrap().state
}

async fn next_in(f: &Fixture) -> Option<Duration> {
    timers::next_in(&f.store, COMMUNITY).await.unwrap()
}

fn of_kind<'a>(events: &'a [Event], kind: &str) -> Vec<&'a Event> {
    events.iter().filter(|e| e.kind == kind).collect()
}

#[tokio::test]
async fn a_timeout_fires_at_its_boundary_and_not_before() {
    let (f, clock) = configured("10m", 0, "1m").await;
    let card = begun(&f, "solo").await;
    assert_eq!(next_in(&f).await, Some(10 * MINUTE));

    clock.advance(10 * MINUTE - Duration::from_secs(1));
    assert!(timers::due(&f.store, COMMUNITY, 10)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(next_in(&f).await, Some(Duration::from_secs(1)));
    assert_eq!(card_state(&f, card).await, CardState::Running);

    clock.advance(Duration::from_secs(1));
    let due = timers::due(&f.store, COMMUNITY, 10).await.unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(
        (due[0].stage.as_str(), due[0].kind),
        ("only", TimerKind::Timeout)
    );

    let events = fire_due(&f).await;
    let ended = of_kind(&events, "run.ended");
    assert_eq!(ended.len(), 1);
    assert_eq!(ended[0].payload["outcome"], "failed");
    assert_eq!(ended[0].payload["reason"], "only timed out after 10m");
    assert_eq!(card_state(&f, card).await, CardState::Failed);
    assert_eq!(next_in(&f).await, None);
}

#[tokio::test]
async fn retries_stop_at_the_maximum_and_each_one_replies_in_the_existing_thread() {
    let (f, clock) = configured("10m", 2, "1m").await;
    let card = begun(&f, "solo").await;

    // First failure: scheduled, not failed.
    clock.advance(10 * MINUTE);
    let events = fire_due(&f).await;
    let scheduled = of_kind(&events, "retry.scheduled");
    assert_eq!(scheduled.len(), 1);
    assert_eq!(scheduled[0].payload["attempt"], 1);
    assert_eq!(scheduled[0].payload["wait_s"], 60);
    assert_eq!(card_state(&f, card).await, CardState::Running);
    assert_eq!(next_in(&f).await, Some(MINUTE));

    // Nothing happens until the wait is over.
    clock.advance(MINUTE - Duration::from_secs(1));
    assert!(fire_due(&f).await.is_empty());
    clock.advance(Duration::from_secs(1));
    fire_due(&f).await;
    f.drain().await.unwrap();
    // The timeout starts again for the new attempt.
    assert_eq!(next_in(&f).await, Some(10 * MINUTE));

    // Second failure waits twice as long.
    clock.advance(10 * MINUTE);
    let events = fire_due(&f).await;
    assert_eq!(
        of_kind(&events, "retry.scheduled")[0].payload["wait_s"],
        120
    );
    clock.advance(2 * MINUTE);
    fire_due(&f).await;
    f.drain().await.unwrap();

    // Third failure is past the maximum: the run fails.
    clock.advance(10 * MINUTE);
    let events = fire_due(&f).await;
    assert!(of_kind(&events, "retry.scheduled").is_empty());
    assert_eq!(
        of_kind(&events, "run.ended")[0].payload["outcome"],
        "failed"
    );
    assert_eq!(card_state(&f, card).await, CardState::Failed);
    f.drain().await.unwrap();

    // One stage task opened the thread, and both retries replied in it.
    let posts = f.relay.posts();
    let roots: Vec<_> = posts
        .iter()
        .filter(|p| p.mention.is_some() && p.root.is_none())
        .collect();
    assert_eq!(roots.len(), 1, "{posts:#?}");
    let replies: Vec<_> = posts.iter().filter(|p| p.root.is_some()).collect();
    assert_eq!(replies.len(), 2, "{posts:#?}");
    for reply in replies {
        assert_eq!(reply.root.as_ref(), Some(&roots[0].event_id));
        assert_eq!(reply.mention.as_deref(), Some(BUILDER));
        assert!(
            reply.text.contains("failed: only timed out after 10m"),
            "{}",
            reply.text
        );
        assert!(reply.text.contains("try again"), "{}", reply.text);
    }
}

#[tokio::test]
async fn the_wait_doubles_and_stops_at_fifteen_minutes() {
    let (f, clock) = configured("1m", 10, "10m").await;
    begun(&f, "solo").await;
    let mut waits = Vec::new();
    for _ in 0..3 {
        clock.advance(MINUTE);
        let events = fire_due(&f).await;
        let wait = of_kind(&events, "retry.scheduled")[0].payload["wait_s"]
            .as_u64()
            .unwrap();
        waits.push(wait);
        clock.advance(Duration::from_secs(wait));
        fire_due(&f).await;
    }
    assert_eq!(waits, [600, 900, 900]);
}

#[tokio::test]
async fn a_report_clears_the_stages_timers() {
    let (f, _clock) = configured("10m", 1, "1m").await;
    let card = begun(&f, "solo").await;
    assert!(next_in(&f).await.is_some());
    f.report(&ctx_for(BUILDER, ActorKind::Agent), card, "passed", "done")
        .await
        .unwrap();
    assert_eq!(card_state(&f, card).await, CardState::Done);
    assert_eq!(next_in(&f).await, None);
}

#[tokio::test]
async fn a_report_that_moves_to_another_stage_replaces_the_timeout() {
    let (f, clock) = configured("10m", 0, "1m").await;
    let card = begun(&f, "three").await;
    clock.advance(4 * MINUTE);
    f.report(&ctx_for(BUILDER, ActorKind::Agent), card, "passed", "")
        .await
        .unwrap();
    // Stage b's timeout counts from when b began, not from when a did.
    assert_eq!(next_in(&f).await, Some(10 * MINUTE));
}

#[tokio::test]
async fn pausing_clears_the_timeout_and_resuming_starts_it_again() {
    let (f, clock) = configured("10m", 0, "1m").await;
    let card = begun(&f, "solo").await;
    pause_card(&f.env(), &person(), card).await.unwrap();
    assert_eq!(next_in(&f).await, None);
    clock.advance(60 * MINUTE);
    assert!(fire_due(&f).await.is_empty());
    resume_card(&f.env(), &person(), card).await.unwrap();
    assert_eq!(next_in(&f).await, Some(10 * MINUTE));
}

#[tokio::test]
async fn stopping_a_run_ends_it_tells_the_agent_and_clears_its_timers() {
    let (f, _clock) = configured("10m", 1, "1m").await;
    let card = begun(&f, "solo").await;
    let run = {
        let mut tx = f.store.begin(COMMUNITY).await.unwrap();
        tx.runs().active_for_card(card).await.unwrap().unwrap()
    };
    // An agent may stop a run as well as a person.
    let (stopped, events) = stop_run(&f.env(), &ctx_for(BUILDER, ActorKind::Agent), run.id)
        .await
        .unwrap();
    assert_eq!(stopped.state, RunState::Ended);
    assert_eq!(
        of_kind(&events, "run.ended")[0].payload["outcome"],
        "stopped"
    );
    assert_eq!(card_state(&f, card).await, CardState::Stopped);
    assert_eq!(next_in(&f).await, None);
    f.drain().await.unwrap();
    let posts = f.relay.posts();
    let note = posts.last().unwrap();
    assert!(note.text.contains("The run was stopped"), "{}", note.text);
    assert!(note.root.is_some());

    // The agent's late report is refused, and a second stop has nothing to stop.
    assert!(f
        .report(&ctx_for(BUILDER, ActorKind::Agent), card, "passed", "")
        .await
        .is_err());
    let again = stop_run(&f.env(), &person(), run.id).await.unwrap_err();
    assert!(matches!(again, OpError::FailedPrecondition(_)), "{again:?}");
}

#[tokio::test]
async fn a_stopped_stage_that_never_posted_is_stopped_without_a_note() {
    let (f, _clock) = configured("10m", 0, "1m").await;
    let card = begun(&f, "held").await;
    let agent = ctx_for(BUILDER, ActorKind::Agent);
    f.report(&agent, card, "passed", "").await.unwrap();
    // The run is held before b, whose task was never posted.
    let run = {
        let mut tx = f.store.begin(COMMUNITY).await.unwrap();
        tx.runs().active_for_card(card).await.unwrap().unwrap()
    };
    assert_eq!(run.state, RunState::Paused);
    stop_run(&f.env(), &person(), run.id).await.unwrap();
    assert_eq!(card_state(&f, card).await, CardState::Stopped);
}

#[tokio::test]
async fn a_failed_or_stopped_card_retries_as_a_new_run_with_its_own_thread() {
    let (f, clock) = configured("10m", 0, "1m").await;
    let card = begun(&f, "solo").await;
    let run = {
        let mut tx = f.store.begin(COMMUNITY).await.unwrap();
        tx.runs().active_for_card(card).await.unwrap().unwrap()
    };
    // A run still in progress cannot be retried.
    let refused = retry_run(&f.env(), &person(), run.id).await.unwrap_err();
    assert!(
        matches!(refused, OpError::FailedPrecondition(_)),
        "{refused:?}"
    );

    clock.advance(10 * MINUTE);
    fire_due(&f).await;
    assert_eq!(card_state(&f, card).await, CardState::Failed);

    let (next, _) = retry_run(&f.env(), &person(), run.id).await.unwrap();
    assert_ne!(next.id, run.id);
    assert_eq!(next.current_stage.as_deref(), Some("only"));
    assert_eq!(next.visit, 2);
    assert_eq!(card_state(&f, card).await, CardState::Running);
    assert_eq!(next_in(&f).await, Some(10 * MINUTE));
    f.drain().await.unwrap();
    let roots = f
        .relay
        .posts()
        .into_iter()
        .filter(|p| p.mention.is_some() && p.root.is_none())
        .count();
    assert_eq!(roots, 2);

    // The old run is no longer the newest, so it cannot be retried again.
    let stale = retry_run(&f.env(), &person(), run.id).await.unwrap_err();
    assert!(matches!(stale, OpError::FailedPrecondition(_)), "{stale:?}");
}

#[tokio::test]
async fn resetting_a_card_stops_its_run_and_leaves_it_idle() {
    let (f, _clock) = configured("10m", 0, "1m").await;
    let card = begun(&f, "solo").await;
    let agent = reset_card(&f.env(), &ctx_for(BUILDER, ActorKind::Agent), card)
        .await
        .unwrap_err();
    assert!(matches!(agent, OpError::PermissionDenied(_)), "{agent:?}");

    let (reset, _) = reset_card(&f.env(), &person(), card).await.unwrap();
    assert_eq!(reset.state, CardState::Idle);
    assert_eq!(card_state(&f, card).await, CardState::Idle);
    assert_eq!(next_in(&f).await, None);
    let mut tx = f.store.begin(COMMUNITY).await.unwrap();
    assert!(tx.runs().active_for_card(card).await.unwrap().is_none());
}

#[tokio::test]
async fn pausing_everything_holds_each_running_run_and_is_a_persons_call() {
    let (f, _clock) = configured("10m", 0, "1m").await;
    let one = begun(&f, "solo").await;
    let two = begun(&f, "solo").await;
    let refused = pause_all(&f.env(), &ctx_for(BUILDER, ActorKind::Agent))
        .await
        .unwrap_err();
    assert!(
        matches!(refused, OpError::PermissionDenied(_)),
        "{refused:?}"
    );
    let (paused, _) = pause_all(&f.env(), &person()).await.unwrap();
    assert_eq!(paused, 2);
    for card in [one, two] {
        assert_eq!(card_state(&f, card).await, CardState::Waiting);
    }
    assert_eq!(next_in(&f).await, None);
    assert_eq!(pause_all(&f.env(), &person()).await.unwrap().0, 0);
}

#[tokio::test]
async fn a_retry_for_a_stage_the_run_has_left_does_nothing() {
    let catalog = catalog();
    let wf = catalog.get("demo").unwrap();
    let mut run = buzz_hive_core::domain::run::Run::new(Uuid::from_u128(5), "demo", &wf.hash());
    let mut facts = Facts::default();
    facts.execution.insert(
        "research".into(),
        Execution {
            timeout: 10 * MINUTE,
            retries: 3,
            backoff: MINUTE,
        },
    );
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
    // Nothing failed yet, so there is nothing to retry; and `build` is not the current stage.
    for stage in ["research", "build"] {
        let stepped = step(
            &run,
            wf,
            &facts,
            Input::RetryNow {
                stage: stage.into(),
            },
        );
        assert!(stepped.effects.is_empty(), "{stage}: {:?}", stepped.effects);
        assert_eq!(stepped.run, run);
    }
    let failed = step(
        &run,
        wf,
        &facts,
        Input::StageFailed {
            stage: "build".into(),
            reason: "x".into(),
        },
    );
    assert!(failed.effects.is_empty());
}

// A restart keeps the store and loses everything else, so it is a new fixture over the same
// store and relay.
fn restarted(f: &Fixture) -> Fixture {
    Fixture {
        store: f.store.clone(),
        directory: f.directory.clone(),
        catalog: catalog(),
        relay: f.relay.clone(),
    }
}

#[tokio::test]
async fn a_restart_with_an_undrained_outbox_posts_each_stage_task_once() {
    let (f, _clock) = configured("10m", 0, "1m").await;
    let (card, _) = create_card(
        &f.store,
        &person(),
        NewCard {
            title: "restart".into(),
            workflow: "solo".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    buzz_hive_core::orchestrator::run::start_run(&f.env(), &person(), card.id)
        .await
        .unwrap();
    // The service stops before it drained anything, and comes back twice over.
    assert!(f.relay.posts().is_empty());
    let after = restarted(&f);
    after.drain().await.unwrap();
    restarted(&f).drain().await.unwrap();
    let posts = f.relay.posts();
    let tasks = posts.iter().filter(|p| p.mention.is_some()).count();
    assert_eq!((posts.len(), tasks), (2, 1), "{posts:#?}");
}

#[tokio::test]
async fn a_restart_keeps_a_pending_timer_and_fires_an_overdue_one() {
    let (f, clock) = configured("10m", 0, "1m").await;
    let pending = begun(&f, "solo").await;
    clock.advance(4 * MINUTE);

    // After the restart the timer is still there, with the time left.
    let after = restarted(&f);
    assert_eq!(next_in(&after).await, Some(6 * MINUTE));
    assert!(fire_due(&after).await.is_empty());
    assert_eq!(card_state(&after, pending).await, CardState::Running);

    // The service was down past the deadline: the timer is overdue when it returns.
    clock.advance(30 * MINUTE);
    let after = restarted(&f);
    assert_eq!(next_in(&after).await, Some(Duration::ZERO));
    let events = fire_due(&after).await;
    assert_eq!(
        of_kind(&events, "run.ended")[0].payload["outcome"],
        "failed"
    );
    assert_eq!(card_state(&after, pending).await, CardState::Failed);
}

#[tokio::test]
async fn two_instances_firing_one_timer_act_once() {
    let (f, clock) = configured("10m", 1, "1m").await;
    begun(&f, "solo").await;
    clock.advance(10 * MINUTE);
    let due = timers::due(&f.store, COMMUNITY, 10).await.unwrap();
    let first = timers::fire(&f.env(), COMMUNITY, &due[0]).await.unwrap();
    let second = timers::fire(&f.env(), COMMUNITY, &due[0]).await.unwrap();
    assert_eq!(of_kind(&first, "retry.scheduled").len(), 1);
    assert!(second.is_empty());
}

#[test]
fn overdue_timers_are_spread_over_the_window_in_firing_order() {
    let window = Duration::from_secs(5);
    let mut draws = [0.9, 0.1, 0.5, 2.0].into_iter();
    let delays = timers::spread(4, window, || draws.next().unwrap());
    assert_eq!(delays.len(), 4);
    assert!(delays.windows(2).all(|pair| pair[0] <= pair[1]));
    assert!(delays.iter().all(|d| *d <= window));
    assert_eq!(delays[0], window.mul_f64(0.1));
    assert_eq!(delays[3], window);
    assert!(timers::spread(0, window, || 0.5).is_empty());
}
