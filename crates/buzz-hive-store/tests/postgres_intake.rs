//! Intake rules and read issues in Postgres: what a rule keeps, what an issue keeps across a
//! new read, and that two passes meeting one new issue make one card between them.

mod support;

use std::time::Duration;

use buzz_hive_core::domain::card::{Card, CardState};
use buzz_hive_core::domain::intake::{IntakeRule, Issue, IssueState, RepoRef, RuleFilter};
use buzz_hive_core::ports::store::{CommunityId, Store};
use buzz_hive_store::pg::PgStore;
use chrono::{TimeZone, Utc};
use uuid::Uuid;

async fn store(prefix: &str) -> (PgStore, support::Scratch, sqlx::PgPool) {
    let (relay_pool, guard) = support::scratch_relay_db(prefix).await;
    let pool = buzz_hive_store::migrate::connect(&support::url_for(&guard))
        .await
        .unwrap();
    buzz_hive_store::migrate::migrate(&pool).await.unwrap();
    (PgStore::new(pool), guard, relay_pool)
}

fn repo() -> RepoRef {
    RepoRef::parse("example-org/api").unwrap()
}

fn issue(number: u64) -> Issue {
    Issue {
        repo: repo(),
        number,
        title: format!("Issue {number}"),
        body: "Body".into(),
        labels: vec!["bug".into(), "triage".into()],
        assignees: vec!["octo".into()],
        author: "octo".into(),
        state: IssueState::Open,
        url: format!("https://github.example.org/example-org/api/issues/{number}"),
        etag: None,
    }
}

fn rule(n: u128) -> IntakeRule {
    IntakeRule {
        id: Uuid::from_u128(n),
        name: format!("rule {n}"),
        repo: repo(),
        filter: RuleFilter {
            label: Some("bug".into()),
            unassigned: true,
            authors: vec!["octo".into()],
        },
        workflow: "demo".into(),
        enabled: true,
        interval_s: 120,
    }
}

fn card(n: u128) -> Card {
    Card {
        id: Uuid::from_u128(n),
        title: format!("card {n}"),
        description: String::new(),
        state: CardState::Idle,
        workflow: String::new(),
        archived: false,
        created_by: "aa".into(),
    }
}

fn at(seconds: i64) -> chrono::DateTime<Utc> {
    Utc.timestamp_opt(1_700_000_000 + seconds, 0).unwrap()
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn a_rule_round_trips_and_lists_oldest_first() {
    let (store, _guard, _relay) = store("hive_intake").await;
    let community = CommunityId::from_u128(1);
    let (one, two) = (rule(1), rule(2));
    let mut tx = store.begin(community).await.unwrap();
    tx.intake().save(&one).await.unwrap();
    tx.commit().await.unwrap();
    let mut tx = store.begin(community).await.unwrap();
    tx.intake().save(&two).await.unwrap();
    tx.commit().await.unwrap();

    let mut tx = store.begin(community).await.unwrap();
    assert_eq!(tx.intake().get(one.id).await.unwrap(), Some(one.clone()));
    assert_eq!(
        tx.intake().list().await.unwrap(),
        [one.clone(), two.clone()]
    );
    assert!(tx.intake().get(Uuid::new_v4()).await.unwrap().is_none());

    let mut edited = one.clone();
    edited.enabled = false;
    edited.filter = RuleFilter::default();
    tx.intake().save(&edited).await.unwrap();
    assert_eq!(tx.intake().get(one.id).await.unwrap(), Some(edited));
    // Another community sees none of it.
    let mut other = store.begin(CommunityId::from_u128(2)).await.unwrap();
    assert!(other.intake().list().await.unwrap().is_empty());
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn a_rule_under_a_minute_is_refused_by_the_table() {
    let (store, _guard, _relay) = store("hive_intake").await;
    let mut short = rule(1);
    short.interval_s = 59;
    let mut tx = store.begin(CommunityId::from_u128(1)).await.unwrap();
    assert!(tx.intake().save(&short).await.is_err());
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn a_new_read_keeps_what_beehive_made_of_the_issue() {
    let (store, _guard, _relay) = store("hive_intake").await;
    let community = CommunityId::from_u128(1);
    let mut tx = store.begin(community).await.unwrap();
    tx.cards().insert(&card(5)).await.unwrap();
    tx.intake().save(&rule(1)).await.unwrap();
    let first = tx.issues().upsert(&issue(7), at(0)).await.unwrap();
    assert_eq!(first.issue, issue(7));
    assert_eq!(first.card_id, None);
    assert_eq!(first.last_comment_at, Some(at(0)));
    tx.issues()
        .attach(&repo(), 7, Uuid::from_u128(5), Some(Uuid::from_u128(1)))
        .await
        .unwrap();
    assert!(tx
        .issues()
        .set_left(&repo(), 7, "closed", at(0))
        .await
        .unwrap());
    tx.issues()
        .set_last_comment(&repo(), 7, at(30))
        .await
        .unwrap();

    let mut changed = issue(7);
    changed.title = "New title".into();
    changed.state = IssueState::Closed;
    changed.etag = Some("W/\"abc\"".into());
    let second = tx.issues().upsert(&changed, at(60)).await.unwrap();
    assert_eq!(second.issue, changed);
    assert_eq!(second.card_id, Some(Uuid::from_u128(5)));
    assert_eq!(second.rule_id, Some(Uuid::from_u128(1)));
    assert_eq!(second.left, "closed");
    assert_eq!(second.last_comment_at, Some(at(30)));

    // A read with no etag keeps the one the issue had.
    let mut plain = changed.clone();
    plain.etag = None;
    let third = tx.issues().upsert(&plain, at(90)).await.unwrap();
    assert_eq!(third.issue.etag.as_deref(), Some("W/\"abc\""));

    assert_eq!(
        tx.issues()
            .by_card(Uuid::from_u128(5))
            .await
            .unwrap()
            .unwrap()
            .issue
            .number,
        7
    );
    assert_eq!(
        tx.issues().of_rule(Uuid::from_u128(1)).await.unwrap().len(),
        1
    );
    // Closed and left, so nothing to poll comments on.
    assert!(tx.issues().watched().await.unwrap().is_empty());
    assert!(tx.issues().set_left(&repo(), 7, "", at(90)).await.unwrap());
    tx.issues().upsert(&issue(7), at(120)).await.unwrap();
    assert_eq!(tx.issues().watched().await.unwrap().len(), 1);

    // Deleting the rule keeps the card and the issue, and forgets the rule.
    assert!(tx.intake().delete(Uuid::from_u128(1)).await.unwrap());
    assert!(!tx.intake().delete(Uuid::from_u128(1)).await.unwrap());
    let kept = tx.issues().get(&repo(), 7).await.unwrap().unwrap();
    assert_eq!(
        (kept.card_id, kept.rule_id),
        (Some(Uuid::from_u128(5)), None)
    );

    assert!(tx
        .issues()
        .attach(&repo(), 99, Uuid::from_u128(5), None)
        .await
        .is_err());
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn a_removed_card_leaves_its_id_on_the_issue() {
    let (store, _guard, _relay) = store("hive_intake").await;
    let community = CommunityId::from_u128(1);
    let mut tx = store.begin(community).await.unwrap();
    tx.cards().insert(&card(5)).await.unwrap();
    tx.issues().upsert(&issue(7), at(0)).await.unwrap();
    tx.issues()
        .attach(&repo(), 7, Uuid::from_u128(5), None)
        .await
        .unwrap();
    tx.cards().delete(Uuid::from_u128(5)).await.unwrap();
    let row = tx.issues().get(&repo(), 7).await.unwrap().unwrap();
    assert_eq!(row.card_id, Some(Uuid::from_u128(5)));
    assert!(tx.cards().get(Uuid::from_u128(5)).await.unwrap().is_none());
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn two_passes_that_meet_one_new_issue_make_one_card() {
    let (store, _guard, _relay) = store("hive_intake").await;
    let store = std::sync::Arc::new(store);
    let community = CommunityId::from_u128(1);
    {
        let mut tx = store.begin(community).await.unwrap();
        tx.cards().insert(&card(1)).await.unwrap();
        tx.cards().insert(&card(2)).await.unwrap();
        tx.commit().await.unwrap();
    }
    // The first pass takes the row and holds it. The second reads the same issue and must wait.
    let mut first = store.begin(community).await.unwrap();
    assert_eq!(
        first
            .issues()
            .upsert(&issue(7), at(0))
            .await
            .unwrap()
            .card_id,
        None
    );

    let second = tokio::spawn({
        let store = std::sync::Arc::clone(&store);
        async move {
            let mut tx = store.begin(community).await.unwrap();
            let row = tx.issues().upsert(&issue(7), at(1)).await.unwrap();
            if row.card_id.is_none() {
                tx.issues()
                    .attach(&repo(), 7, Uuid::from_u128(2), None)
                    .await
                    .unwrap();
            }
            tx.commit().await.unwrap();
            row.card_id
        }
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!second.is_finished(), "the second pass waits for the first");
    first
        .issues()
        .attach(&repo(), 7, Uuid::from_u128(1), None)
        .await
        .unwrap();
    first.commit().await.unwrap();

    // The second pass saw the first one's card, so it made none.
    assert_eq!(second.await.unwrap(), Some(Uuid::from_u128(1)));
    let mut tx = store.begin(community).await.unwrap();
    assert_eq!(
        tx.issues().get(&repo(), 7).await.unwrap().unwrap().card_id,
        Some(Uuid::from_u128(1))
    );
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn an_older_read_neither_overwrites_a_newer_one_nor_changes_its_left_mark() {
    let (store, _guard, _relay) = store("hive_intake").await;
    let mut tx = store.begin(CommunityId::from_u128(1)).await.unwrap();
    let mut newer = issue(7);
    newer.title = "Newer".into();
    tx.issues().upsert(&newer, at(100)).await.unwrap();
    assert!(tx
        .issues()
        .set_left(&repo(), 7, "unmatched", at(100))
        .await
        .unwrap());

    // A pass that read the host before the one above, and commits after it.
    let mut older = issue(7);
    older.title = "Older".into();
    let row = tx.issues().upsert(&older, at(50)).await.unwrap();
    assert_eq!(row.issue.title, "Newer");
    assert_eq!(row.left, "unmatched");
    assert!(!tx.issues().set_left(&repo(), 7, "", at(50)).await.unwrap());
    let held = tx.issues().get(&repo(), 7).await.unwrap().unwrap();
    assert_eq!(
        (held.issue.title.as_str(), held.left.as_str()),
        ("Newer", "unmatched")
    );

    // A read made at the same time or later is written.
    assert!(tx.issues().set_left(&repo(), 7, "", at(100)).await.unwrap());
    older.title = "Latest".into();
    assert_eq!(
        tx.issues()
            .upsert(&older, at(150))
            .await
            .unwrap()
            .issue
            .title,
        "Latest"
    );
    // An issue never read is still told apart from one read since.
    assert!(tx.issues().set_left(&repo(), 8, "", at(0)).await.is_err());
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn the_comment_cursor_does_not_move_back() {
    let (store, _guard, _relay) = store("hive_intake").await;
    let mut tx = store.begin(CommunityId::from_u128(1)).await.unwrap();
    tx.issues().upsert(&issue(7), at(0)).await.unwrap();
    tx.issues()
        .set_last_comment(&repo(), 7, at(30))
        .await
        .unwrap();
    tx.issues()
        .set_last_comment(&repo(), 7, at(10))
        .await
        .unwrap();
    let row = tx.issues().get(&repo(), 7).await.unwrap().unwrap();
    assert_eq!(row.last_comment_at, Some(at(30)));
    tx.issues()
        .set_last_comment(&repo(), 7, at(40))
        .await
        .unwrap();
    let row = tx.issues().get(&repo(), 7).await.unwrap().unwrap();
    assert_eq!(row.last_comment_at, Some(at(40)));
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn two_passes_over_the_same_comments_record_each_once_and_the_cursor_holds() {
    use std::sync::Arc;

    use buzz_hive_core::background::comments::{poll_comments, CURSOR_MARGIN};
    use buzz_hive_core::orchestrator::Env;
    use buzz_hive_core::testing::{FakeCodeHost, FakeDirectory};
    use buzz_hive_core::workflow::load::Catalog;

    let (store, _guard, _relay) = store("hive_intake").await;
    let community = CommunityId::from_u128(1);
    let start = at(0);
    {
        let mut tx = store.begin(community).await.unwrap();
        let mut working = card(1);
        working.state = CardState::Running;
        tx.cards().insert(&working).await.unwrap();
        tx.issues().upsert(&issue(7), start).await.unwrap();
        tx.issues()
            .attach(&repo(), 7, working.id, None)
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }
    let host = FakeCodeHost::new();
    let first = host.put_comment(
        &repo(),
        7,
        "mallory",
        "one",
        start + chrono::Duration::seconds(10),
    );
    let second = host.put_comment(
        &repo(),
        7,
        "mallory",
        "two",
        start + chrono::Duration::seconds(20),
    );
    // Both passes list the comments before either one records them.
    host.hold_comment_reads(Arc::new(tokio::sync::Barrier::new(2)));
    let directory = FakeDirectory::new();
    let catalog = Catalog::default();
    let env = Env {
        store: &store,
        directory: &directory,
        catalog: &catalog,
        hive: "ee",
    };
    let now = start + chrono::Duration::seconds(200);
    let (a, b) = tokio::join!(
        poll_comments(&env, &host, community, now),
        poll_comments(&env, &host, community, now)
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_eq!(a.0.received + b.0.received, 2, "{a:?} {b:?}");

    let mut tx = store.begin(community).await.unwrap();
    let ids: Vec<u64> = tx
        .events()
        .of_card(Uuid::from_u128(1), "comment.received", 0, 100)
        .await
        .unwrap()
        .iter()
        .map(|e| e.payload["comment_id"].as_u64().unwrap())
        .collect();
    let mut sorted = ids.clone();
    sorted.sort();
    assert_eq!(
        sorted,
        [first, second],
        "each comment is recorded once: {ids:?}"
    );

    // The cursor is at the newest comment, which is older than the margin by now, and the pass
    // that came second did not pull it back.
    let held = tx.issues().get(&repo(), 7).await.unwrap().unwrap();
    assert!(now - CURSOR_MARGIN > start + chrono::Duration::seconds(20));
    assert_eq!(
        held.last_comment_at,
        Some(start + chrono::Duration::seconds(20))
    );
}
