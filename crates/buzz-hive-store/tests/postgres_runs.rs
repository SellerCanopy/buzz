//! Runs, stage threads and the outbox keep their rules in Postgres: one active run per card,
//! one outbox row per key, and a drain that skips rows another worker holds.

mod support;

use buzz_hive_core::domain::card::{Card, CardState};
use buzz_hive_core::domain::run::{Run, RunState, RunStep};
use buzz_hive_core::ports::store::{CommunityId, Posting, StageThread, Store, StoreError};
use buzz_hive_store::pg::PgStore;
use std::time::Duration;
use uuid::Uuid;

const LEASE: Duration = Duration::from_secs(60);

async fn store(prefix: &str) -> (PgStore, support::Scratch, sqlx::PgPool) {
    let (relay_pool, guard) = support::scratch_relay_db(prefix).await;
    let pool = buzz_hive_store::migrate::connect(&support::url_for(&guard))
        .await
        .unwrap();
    buzz_hive_store::migrate::migrate(&pool).await.unwrap();
    (PgStore::new(pool), guard, relay_pool)
}

fn card(n: u128) -> Card {
    Card {
        id: Uuid::from_u128(n),
        title: format!("card {n}"),
        description: String::new(),
        state: CardState::Idle,
        workflow: "demo".into(),
        archived: false,
        created_by: "aa".into(),
    }
}

fn status(text: &str) -> Posting {
    Posting::Status {
        channel: Uuid::from_u128(77),
        text: text.into(),
    }
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn a_run_round_trips_and_a_card_has_one_active_run() {
    let (store, _guard, _relay) = store("hive_runs").await;
    let community = CommunityId::from_u128(1);
    let mut tx = store.begin(community).await.unwrap();
    tx.cards().insert(&card(1)).await.unwrap();
    let mut run = Run::new(Uuid::from_u128(1), "demo", "h1");
    tx.runs().insert(&run).await.unwrap();
    let second = Run::new(Uuid::from_u128(1), "demo", "h1");
    let refused = tx.runs().insert(&second).await;
    assert!(
        matches!(refused, Err(StoreError::Conflict(_))),
        "{refused:?}"
    );
    tx.commit().await.unwrap();

    let mut tx = store.begin(community).await.unwrap();
    assert_eq!(tx.runs().get(run.id).await.unwrap(), Some(run.clone()));
    assert_eq!(
        tx.runs()
            .active_for_card(run.card_id)
            .await
            .unwrap()
            .map(|r| r.id),
        Some(run.id)
    );

    run.state = RunState::Running;
    run.current_stage = Some("build".into());
    run.visit = 1;
    run.requester = Some("bb".into());
    tx.runs().update(&run).await.unwrap();
    let counts = tx.runs().visit_counts(run.card_id).await.unwrap();
    assert_eq!(
        counts,
        [("build".to_owned(), 1)].into(),
        "a run with no steps counts its stage"
    );
    assert!(tx
        .runs()
        .visit_counts(Uuid::from_u128(9))
        .await
        .unwrap()
        .is_empty());

    // The steps and parameters of a run come back as written, and every step's visit counts.
    run.steps = vec![
        RunStep {
            stage: "research".into(),
            visit: 2,
            round: 3,
            result: "passed".into(),
            routed_to: Some("build".into()),
            ..RunStep::default()
        },
        RunStep {
            stage: "build".into(),
            visit: 1,
            round: 1,
            held: true,
            ..RunStep::default()
        },
    ];
    run.parameters = [("depth".to_owned(), "deep".to_owned())].into();
    tx.runs().update(&run).await.unwrap();
    let stored = tx.runs().get(run.id).await.unwrap().unwrap();
    assert_eq!(stored.steps, run.steps);
    assert_eq!(stored.parameters, run.parameters);
    let counts = tx.runs().visit_counts(run.card_id).await.unwrap();
    assert_eq!(
        counts,
        [("build".to_owned(), 1), ("research".to_owned(), 2)].into()
    );

    // Once the run ends the card can start another.
    run.state = RunState::Ended;
    run.ended_at = Some(chrono::Utc::now());
    tx.runs().update(&run).await.unwrap();
    assert_eq!(tx.runs().active_for_card(run.card_id).await.unwrap(), None);
    tx.runs().insert(&second).await.unwrap();
    assert_eq!(tx.runs().list(Some(run.card_id)).await.unwrap().len(), 2);
    assert_eq!(tx.runs().list(None).await.unwrap().len(), 2);
    assert!(tx
        .runs()
        .list(Some(Uuid::from_u128(9)))
        .await
        .unwrap()
        .is_empty());
}

/// An event on `card` of `kind`, with a summary to tell it apart.
fn event(card: u128, kind: &str, summary: &str) -> buzz_hive_core::domain::events::Event {
    buzz_hive_core::domain::events::Event::new(
        kind,
        Some(Uuid::from_u128(card)),
        None,
        buzz_hive_core::domain::events::Actor {
            pubkey: "aa".into(),
            kind: buzz_hive_core::access::ActorKind::Person,
        },
        serde_json::json!({ "type": "note", "summary": summary, "detail": "" }),
    )
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn a_cards_events_of_one_kind_come_back_in_order_and_only_its_own() {
    let (store, _guard, _relay) = store("hive_of_card").await;
    let one = CommunityId::from_u128(1);
    let two = CommunityId::from_u128(2);
    let mut tx = store.begin(one).await.unwrap();
    for n in [1, 2, 3] {
        tx.cards().insert(&card(n)).await.unwrap();
    }
    // Written interleaved, so the order is the log's and not the card's.
    for (card, kind, summary) in [
        (1, "activity.added", "first"),
        (2, "activity.added", "other card"),
        (1, "run.started", "not a note"),
        (1, "activity.added", "second"),
        (1, "activity.added", "third"),
    ] {
        tx.events()
            .append(event(card, kind, summary))
            .await
            .unwrap();
    }
    tx.commit().await.unwrap();
    let summaries = |events: &[buzz_hive_core::domain::events::Event]| -> Vec<String> {
        events
            .iter()
            .map(|e| e.payload["summary"].as_str().unwrap().to_owned())
            .collect()
    };

    let mut tx = store.begin(one).await.unwrap();
    let all = tx
        .events()
        .of_card(Uuid::from_u128(1), "activity.added", 0, 10)
        .await
        .unwrap();
    assert_eq!(summaries(&all), ["first", "second", "third"]);
    assert!(all.windows(2).all(|pair| pair[0].seq < pair[1].seq));
    assert!(all.iter().all(|e| e.card_id == Some(Uuid::from_u128(1))));

    // After a seq, and under a limit, from the oldest.
    let after = tx
        .events()
        .of_card(Uuid::from_u128(1), "activity.added", all[0].seq, 10)
        .await
        .unwrap();
    assert_eq!(summaries(&after), ["second", "third"]);
    let limited = tx
        .events()
        .of_card(Uuid::from_u128(1), "activity.added", 0, 2)
        .await
        .unwrap();
    assert_eq!(summaries(&limited), ["first", "second"]);
    assert!(tx
        .events()
        .of_card(Uuid::from_u128(1), "activity.added", 0, 0)
        .await
        .unwrap()
        .is_empty());

    // The other kind, the other card, a card with no events, and another community.
    let started = tx
        .events()
        .of_card(Uuid::from_u128(1), "run.started", 0, 10)
        .await
        .unwrap();
    assert_eq!(summaries(&started), ["not a note"]);
    let other = tx
        .events()
        .of_card(Uuid::from_u128(2), "activity.added", 0, 10)
        .await
        .unwrap();
    assert_eq!(summaries(&other), ["other card"]);
    assert!(tx
        .events()
        .of_card(Uuid::from_u128(3), "activity.added", 0, 10)
        .await
        .unwrap()
        .is_empty());
    let mut tx = store.begin(two).await.unwrap();
    assert!(tx
        .events()
        .of_card(Uuid::from_u128(1), "activity.added", 0, 10)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn runs_stay_in_their_community() {
    let (store, _guard, _relay) = store("hive_runs_c").await;
    let one = CommunityId::from_u128(1);
    let two = CommunityId::from_u128(2);
    let run = Run::new(Uuid::from_u128(1), "demo", "h1");
    let mut tx = store.begin(one).await.unwrap();
    tx.cards().insert(&card(1)).await.unwrap();
    tx.runs().insert(&run).await.unwrap();
    tx.commit().await.unwrap();

    let mut tx = store.begin(two).await.unwrap();
    assert_eq!(tx.runs().get(run.id).await.unwrap(), None);
    assert!(tx.runs().list(None).await.unwrap().is_empty());
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn a_stage_thread_is_found_by_its_root_in_its_own_community_only() {
    let (store, _guard, _relay) = store("hive_thread_root").await;
    let (one, two) = (CommunityId::from_u128(1), CommunityId::from_u128(2));
    let thread = StageThread {
        card_id: Uuid::from_u128(1),
        stage: "build".into(),
        visit: 1,
        root_event_id: "e1".into(),
        agent_pubkey: "bb".into(),
        channel_id: Uuid::from_u128(77),
    };
    let mut tx = store.begin(one).await.unwrap();
    tx.cards().insert(&card(1)).await.unwrap();
    tx.runs().record_thread(&thread).await.unwrap();
    tx.commit().await.unwrap();

    let mut tx = store.begin(one).await.unwrap();
    assert_eq!(
        tx.runs().thread_by_root("e1").await.unwrap(),
        Some(thread.clone())
    );
    assert_eq!(tx.runs().thread_by_root("e2").await.unwrap(), None);
    // The same root in another community is another thread, or none.
    let mut tx = store.begin(two).await.unwrap();
    assert_eq!(tx.runs().thread_by_root("e1").await.unwrap(), None);
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn a_stage_thread_is_recorded_once() {
    let (store, _guard, _relay) = store("hive_threads").await;
    let community = CommunityId::from_u128(1);
    let mut tx = store.begin(community).await.unwrap();
    tx.cards().insert(&card(1)).await.unwrap();
    let thread = StageThread {
        card_id: Uuid::from_u128(1),
        stage: "build".into(),
        visit: 1,
        root_event_id: "e1".into(),
        agent_pubkey: "bb".into(),
        channel_id: Uuid::from_u128(77),
    };
    tx.runs().record_thread(&thread).await.unwrap();
    // A repeat keeps the first event.
    let again = StageThread {
        root_event_id: "e2".into(),
        ..thread.clone()
    };
    tx.runs().record_thread(&again).await.unwrap();
    assert_eq!(
        tx.runs().thread(thread.card_id, "build", 1).await.unwrap(),
        Some(thread)
    );
    assert_eq!(
        tx.runs()
            .thread(Uuid::from_u128(1), "build", 2)
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn the_outbox_keeps_one_row_per_key_and_drains_in_order() {
    let (store, _guard, _relay) = store("hive_outbox").await;
    let community = CommunityId::from_u128(1);
    let mut tx = store.begin(community).await.unwrap();
    tx.cards().insert(&card(1)).await.unwrap();
    tx.cards().insert(&card(2)).await.unwrap();
    let one = Uuid::from_u128(1);
    let two = Uuid::from_u128(2);
    assert!(tx
        .outbox()
        .enqueue("k1", one, &status("first"))
        .await
        .unwrap());
    assert!(!tx
        .outbox()
        .enqueue("k1", one, &status("again"))
        .await
        .unwrap());
    assert!(tx
        .outbox()
        .enqueue("k2", two, &status("second"))
        .await
        .unwrap());
    tx.commit().await.unwrap();

    let mut tx = store.begin(community).await.unwrap();
    let all = tx.outbox().all().await.unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].posting, status("first"));
    drop(tx);

    // A card in the skip list is passed over.
    let mut tx = store.begin(community).await.unwrap();
    let head = tx.outbox().claim_next(&[], LEASE).await.unwrap().unwrap();
    assert_eq!(head.key, "k1");
    drop(tx);
    let mut tx = store.begin(community).await.unwrap();
    let next = tx
        .outbox()
        .claim_next(&[one], LEASE)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(next.key, "k2");
    tx.outbox()
        .mark_failed(next.id, "relay down")
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let mut tx = store.begin(community).await.unwrap();
    let rows = tx.outbox().all().await.unwrap();
    assert_eq!(rows[1].attempts, 1);
    assert_eq!(rows[1].last_error.as_deref(), Some("relay down"));
    tx.outbox().mark_done(rows[0].id).await.unwrap();
    tx.commit().await.unwrap();

    let mut tx = store.begin(community).await.unwrap();
    assert_eq!(
        tx.outbox()
            .claim_next(&[], LEASE)
            .await
            .unwrap()
            .unwrap()
            .key,
        "k2",
        "a failed post is free to claim again"
    );

    // A row is found by its key whether or not it is done, and only in its community.
    let done = tx.outbox().by_key("k1").await.unwrap().unwrap();
    assert!(done.done_at.is_some());
    assert_eq!(done.posting, status("first"));
    assert!(tx
        .outbox()
        .by_key("k2")
        .await
        .unwrap()
        .unwrap()
        .done_at
        .is_none());
    assert!(tx.outbox().by_key("missing").await.unwrap().is_none());
    drop(tx);

    // A reply in a stage's thread is stored and read back whole.
    let reply = Posting::StageReply {
        channel: Uuid::from_u128(77),
        stage: "build".into(),
        visit: 2,
        agent_name: "builder".into(),
        agent: "b1".into(),
        text: "This is round 2.".into(),
    };
    let mut tx = store.begin(community).await.unwrap();
    assert!(tx.outbox().enqueue("k3", one, &reply).await.unwrap());
    tx.commit().await.unwrap();
    let mut tx = store.begin(community).await.unwrap();
    assert_eq!(
        tx.outbox().by_key("k3").await.unwrap().unwrap().posting,
        reply
    );
    drop(tx);
    let mut other = store.begin(CommunityId::from_u128(2)).await.unwrap();
    assert!(other.outbox().by_key("k1").await.unwrap().is_none());
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn a_second_worker_never_takes_a_later_post_of_a_card_whose_earlier_post_is_in_flight() {
    let (store, _guard, _relay) = store("hive_outbox_lock").await;
    let community = CommunityId::from_u128(1);
    let mut tx = store.begin(community).await.unwrap();
    tx.cards().insert(&card(1)).await.unwrap();
    tx.cards().insert(&card(2)).await.unwrap();
    for (key, owner) in [("k1", 1), ("k2", 1), ("k3", 2)] {
        tx.outbox()
            .enqueue(key, Uuid::from_u128(owner), &status(key))
            .await
            .unwrap();
    }
    tx.commit().await.unwrap();

    // The first worker claims k1 and ends its transaction, as the drain does before it sends.
    let mut first = store.begin(community).await.unwrap();
    assert_eq!(
        first
            .outbox()
            .claim_next(&[], LEASE)
            .await
            .unwrap()
            .unwrap()
            .key,
        "k1"
    );
    first.commit().await.unwrap();

    // k2 is the same card's next post, so it waits behind k1. Card two is free.
    let mut second = store.begin(community).await.unwrap();
    assert_eq!(
        second
            .outbox()
            .claim_next(&[], LEASE)
            .await
            .unwrap()
            .unwrap()
            .key,
        "k3"
    );
    second.commit().await.unwrap();
    let mut third = store.begin(community).await.unwrap();
    assert!(third
        .outbox()
        .claim_next(&[], LEASE)
        .await
        .unwrap()
        .is_none());

    // Once k1's lease has run out, k1 is offered again, and still k2 is not.
    assert_eq!(
        third
            .outbox()
            .claim_next(&[], Duration::ZERO)
            .await
            .unwrap()
            .unwrap()
            .key,
        "k1"
    );
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn a_claim_still_uncommitted_is_skipped_not_waited_for() {
    let (store, _guard, _relay) = store("hive_outbox_uncommitted").await;
    let community = CommunityId::from_u128(1);
    let mut tx = store.begin(community).await.unwrap();
    tx.cards().insert(&card(1)).await.unwrap();
    for key in ["k1", "k2"] {
        tx.outbox()
            .enqueue(key, Uuid::from_u128(1), &status(key))
            .await
            .unwrap();
    }
    tx.commit().await.unwrap();

    let mut first = store.begin(community).await.unwrap();
    assert_eq!(
        first
            .outbox()
            .claim_next(&[], LEASE)
            .await
            .unwrap()
            .unwrap()
            .key,
        "k1"
    );
    // The row is locked by the first transaction. The second neither waits for it nor takes k2.
    let mut second = store.begin(community).await.unwrap();
    let taken = tokio::time::timeout(
        Duration::from_secs(5),
        second.outbox().claim_next(&[], LEASE),
    )
    .await
    .expect("a claim must not wait for another claimer's transaction")
    .unwrap();
    assert!(taken.is_none());
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn one_cards_failure_does_not_block_another() {
    let (store, _guard, _relay) = store("hive_outbox_two_cards").await;
    buzz_hive_core::testing::outbox_scenarios::one_cards_failure_does_not_block_another(
        &store,
        CommunityId::from_u128(1),
    )
    .await;
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn a_stopped_drain_sends_nothing() {
    let (store, _guard, _relay) = store("hive_outbox_stop").await;
    buzz_hive_core::testing::outbox_scenarios::a_stopped_drain_sends_nothing(
        &store,
        CommunityId::from_u128(1),
    )
    .await;
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn settings_remove_and_list_stay_in_their_scope() {
    let (store, _guard, _relay) = store("hive_settings_rl").await;
    let community = CommunityId::from_u128(1);
    let mut tx = store.begin(community).await.unwrap();
    tx.settings()
        .set("engine", "", "a", serde_json::json!(1))
        .await
        .unwrap();
    tx.settings()
        .set("engine", "", "b", serde_json::json!("x"))
        .await
        .unwrap();
    tx.settings()
        .set("workflow", "demo", "a", serde_json::json!(2))
        .await
        .unwrap();
    let listed = tx.settings().list("engine", "").await.unwrap();
    assert_eq!(listed.len(), 2);
    assert_eq!(listed["b"], serde_json::json!("x"));
    tx.settings().remove("engine", "", "a").await.unwrap();
    // Removing what is not set is not an error.
    tx.settings().remove("engine", "", "a").await.unwrap();
    assert_eq!(tx.settings().list("engine", "").await.unwrap().len(), 1);
    assert_eq!(
        tx.settings().list("workflow", "demo").await.unwrap().len(),
        1
    );
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn a_cards_threads_come_back_in_the_order_they_were_recorded() {
    let (store, _guard, _relay) = store("hive_threads_order").await;
    let community = CommunityId::from_u128(1);
    let mut tx = store.begin(community).await.unwrap();
    tx.cards().insert(&card(1)).await.unwrap();
    tx.cards().insert(&card(2)).await.unwrap();
    // Stage names that sort the other way, and a revisit of the first stage last.
    for (card, stage, visit, root) in [
        (1, "plan", 1, "r1"),
        (1, "build", 1, "r2"),
        (2, "plan", 1, "other"),
        (1, "plan", 2, "r3"),
    ] {
        tx.runs()
            .record_thread(&StageThread {
                card_id: Uuid::from_u128(card),
                stage: stage.into(),
                visit,
                root_event_id: root.into(),
                agent_pubkey: "bb".into(),
                channel_id: Uuid::from_u128(77),
            })
            .await
            .unwrap();
    }
    let got = tx.runs().threads(Uuid::from_u128(1)).await.unwrap();
    let roots: Vec<_> = got.iter().map(|t| t.root_event_id.as_str()).collect();
    assert_eq!(roots, ["r1", "r2", "r3"], "card 2's thread is not here");
    assert!(got.iter().all(|t| t.channel_id == Uuid::from_u128(77)));
}
