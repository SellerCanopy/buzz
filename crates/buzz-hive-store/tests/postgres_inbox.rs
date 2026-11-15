//! Needs you items in Postgres: written with the run, listed open first, resolved once, and
//! gone with their card.

mod support;

use buzz_hive_core::domain::card::{Card, CardState};
use buzz_hive_core::domain::inbox::{InboxItem, InboxKind};
use buzz_hive_core::domain::run::Run;
use buzz_hive_core::ports::store::{CommunityId, Store};
use buzz_hive_store::pg::PgStore;
use uuid::Uuid;

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

/// A card with a run, and the run's id.
async fn run_of(store: &PgStore, community: CommunityId, n: u128) -> Uuid {
    let mut tx = store.begin(community).await.unwrap();
    tx.cards().insert(&card(n)).await.unwrap();
    let run = Run::new(Uuid::from_u128(n), "demo", "h1");
    tx.runs().insert(&run).await.unwrap();
    tx.commit().await.unwrap();
    run.id
}

fn item(n: u128, run: Uuid, kind: InboxKind) -> InboxItem {
    InboxItem::open(
        Uuid::from_u128(n),
        run,
        "gate",
        2,
        kind,
        "gate waits",
        serde_json::json!({"artifacts": [{"name": "plan", "version": 3}]}),
    )
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn an_item_round_trips_and_lists_open_first() {
    let (store, _guard, _relay) = store("hive_inbox").await;
    let community = CommunityId::from_u128(1);
    let first_run = run_of(&store, community, 1).await;
    let second_run = run_of(&store, community, 2).await;

    let older = item(1, first_run, InboxKind::Approval);
    let newer = item(2, second_run, InboxKind::Question);
    let mut tx = store.begin(community).await.unwrap();
    tx.inbox().insert(&older).await.unwrap();
    tx.inbox().insert(&newer).await.unwrap();
    tx.commit().await.unwrap();

    let mut tx = store.begin(community).await.unwrap();
    let found = tx.inbox().get(older.id).await.unwrap().unwrap();
    assert_eq!(found, older);
    assert_eq!(found.visit, 2);
    assert_eq!(found.detail["artifacts"][0]["version"], 3);
    assert!(tx.inbox().get(Uuid::new_v4()).await.unwrap().is_none());

    // Open items first, each group newest first.
    assert!(tx.inbox().resolve(newer.id, "reply", "aa").await.unwrap());
    let all = tx.inbox().list(None, true).await.unwrap();
    assert_eq!(
        all.iter().map(|i| i.id).collect::<Vec<_>>(),
        [older.id, newer.id]
    );
    let open = tx.inbox().list(None, false).await.unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].id, older.id);
    let of_card = tx.inbox().list(Some(newer.card_id), true).await.unwrap();
    assert_eq!(of_card.len(), 1);
    assert_eq!(of_card[0].answer.as_deref(), Some("reply"));
    assert_eq!(of_card[0].resolved_by.as_deref(), Some("aa"));
    assert!(of_card[0].resolved_at.is_some());
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn an_item_resolves_once() {
    let (store, _guard, _relay) = store("hive_inbox").await;
    let community = CommunityId::from_u128(1);
    let run = run_of(&store, community, 1).await;
    let one = item(1, run, InboxKind::Route);
    let mut tx = store.begin(community).await.unwrap();
    tx.inbox().insert(&one).await.unwrap();
    assert!(tx.inbox().resolve(one.id, "skip", "aa").await.unwrap());
    assert!(!tx.inbox().resolve(one.id, "stop", "bb").await.unwrap());
    let after = tx.inbox().get(one.id).await.unwrap().unwrap();
    assert_eq!(after.answer.as_deref(), Some("skip"));
    assert_eq!(after.resolved_by.as_deref(), Some("aa"));
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn resolving_a_card_closes_only_its_open_items() {
    let (store, _guard, _relay) = store("hive_inbox").await;
    let community = CommunityId::from_u128(1);
    let run = run_of(&store, community, 1).await;
    let other = run_of(&store, community, 2).await;
    let mut tx = store.begin(community).await.unwrap();
    let kept = item(2, other, InboxKind::Approval);
    let done = item(1, run, InboxKind::Question);
    let open = item(1, run, InboxKind::Route);
    for i in [&kept, &done, &open] {
        tx.inbox().insert(i).await.unwrap();
    }
    assert!(tx.inbox().resolve(done.id, "reply", "aa").await.unwrap());

    let closed = tx
        .inbox()
        .resolve_card(Uuid::from_u128(1), "superseded", "")
        .await
        .unwrap();
    assert_eq!(closed.len(), 1);
    assert_eq!(closed[0].id, open.id);
    assert_eq!(closed[0].answer.as_deref(), Some("superseded"));
    assert!(closed[0].resolved_at.is_some());
    // The earlier answer stands, and the other card's item is still open.
    let kept_after = tx.inbox().get(kept.id).await.unwrap().unwrap();
    assert!(kept_after.is_open());
    let done_after = tx.inbox().get(done.id).await.unwrap().unwrap();
    assert_eq!(done_after.answer.as_deref(), Some("reply"));
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn items_belong_to_one_community_and_go_with_their_card() {
    let (store, _guard, _relay) = store("hive_inbox").await;
    let one = CommunityId::from_u128(1);
    let two = CommunityId::from_u128(2);
    let run = run_of(&store, one, 1).await;
    let mine = item(1, run, InboxKind::Failure);
    let mut tx = store.begin(one).await.unwrap();
    tx.inbox().insert(&mine).await.unwrap();
    tx.commit().await.unwrap();

    let mut tx = store.begin(two).await.unwrap();
    assert!(tx.inbox().get(mine.id).await.unwrap().is_none());
    assert!(tx.inbox().list(None, true).await.unwrap().is_empty());
    drop(tx);

    let mut tx = store.begin(one).await.unwrap();
    tx.cards().delete(Uuid::from_u128(1)).await.unwrap();
    tx.commit().await.unwrap();
    let mut tx = store.begin(one).await.unwrap();
    assert!(tx.inbox().list(None, true).await.unwrap().is_empty());
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn an_item_for_a_run_that_does_not_exist_is_refused() {
    let (store, _guard, _relay) = store("hive_inbox").await;
    let community = CommunityId::from_u128(1);
    let run = run_of(&store, community, 1).await;
    let mut orphan = item(1, run, InboxKind::Route);
    orphan.run_id = Uuid::new_v4();
    let mut tx = store.begin(community).await.unwrap();
    assert!(tx.inbox().insert(&orphan).await.is_err());
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn usage_counts_agent_events_by_week_and_run_times() {
    use buzz_hive_core::access::ActorKind;
    use buzz_hive_core::domain::events::{Actor, Event};
    use chrono::Datelike;

    let (store, _guard, _relay) = store("hive_usage").await;
    let community = CommunityId::from_u128(1);
    let mut tx = store.begin(community).await.unwrap();
    tx.cards().insert(&card(1)).await.unwrap();
    // The first run ended after 90 seconds, and a card holds one run that has not ended.
    let mut first = Run::new(Uuid::from_u128(1), "demo", "h1");
    first.state = buzz_hive_core::domain::run::RunState::Ended;
    first.ended_at = Some(first.started_at + chrono::Duration::seconds(90));
    tx.runs().insert(&first).await.unwrap();
    let mut second = Run::new(Uuid::from_u128(1), "demo", "h1");
    second.id = Uuid::from_u128(99);
    tx.runs().insert(&second).await.unwrap();

    let agent = Actor {
        pubkey: "bb".into(),
        kind: ActorKind::Agent,
    };
    let person = Actor {
        pubkey: "aa".into(),
        kind: ActorKind::Person,
    };
    for (kind, who) in [
        ("card.created", &agent),
        ("card.created", &person),
        ("run.started", &agent),
        ("run.started", &agent),
        ("run.started", &person),
    ] {
        tx.events()
            .append(Event::new(
                kind,
                Some(Uuid::from_u128(1)),
                None,
                who.clone(),
                serde_json::json!({}),
            ))
            .await
            .unwrap();
    }
    tx.commit().await.unwrap();

    let mut tx = store.begin(community).await.unwrap();
    let usage = tx.usage().usage(8, "hive").await.unwrap();
    assert_eq!(usage.runs, 2);
    assert_eq!(usage.ended_runs, 1);
    assert_eq!(usage.average_run_seconds, 90);
    assert_eq!(usage.longest_run_seconds, 90);
    assert_eq!(usage.weeks.len(), 8);
    let this_week = usage.weeks.last().unwrap();
    assert_eq!(this_week.cards_created_by_agents, 1);
    assert_eq!(this_week.runs_started_by_agents, 2);
    assert!(usage.weeks[..7]
        .iter()
        .all(|w| w.cards_created_by_agents == 0 && w.runs_started_by_agents == 0));
    // Weeks start on a Monday and follow one another.
    assert!(usage
        .weeks
        .iter()
        .all(|w| w.starts.weekday() == chrono::Weekday::Mon));
    assert_eq!(
        usage.weeks[1].starts - usage.weeks[0].starts,
        chrono::Duration::days(7)
    );

    // Another community sees none of it.
    let mut other = store.begin(CommunityId::from_u128(2)).await.unwrap();
    let nothing = other.usage().usage(8, "hive").await.unwrap();
    assert_eq!(nothing.runs, 0);
    assert!(nothing.weeks.iter().all(|w| w.runs_started_by_agents == 0));
}
