//! The intake loops over an in-memory store and a fake code host, in paused time: when a rule's
//! first pass runs, what a failure or a rate limit does to the wait, and what a changed or
//! deleted rule does to its loop. The random source is chosen by the test.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use buzz_hive::intake::{IntakeSupervisor, Random, Shared, COMMENT_INTERVAL_S};
use buzz_hive_core::background::intake::Gate;
use buzz_hive_core::bus::Bus;
use buzz_hive_core::domain::card::{Card, CardState};
use buzz_hive_core::domain::intake::{IntakeRule, Issue, IssueState, RepoRef, RuleFilter};
use buzz_hive_core::ports::code_host::HostError;
use buzz_hive_core::ports::store::{CommunityId, Store};
use buzz_hive_core::testing::{FakeCodeHost, FakeDirectory, MemStore};
use buzz_hive_core::workflow::source::Sources;
use chrono::Utc;
use tokio::sync::{watch, Notify};
use uuid::Uuid;

const COMMUNITY: CommunityId = CommunityId::from_u128(1);

fn repo(name: &str) -> RepoRef {
    RepoRef::parse(&format!("example-org/{name}")).unwrap()
}

fn issue(repo: &RepoRef, number: u64) -> Issue {
    Issue {
        repo: repo.clone(),
        number,
        title: format!("Issue {number}"),
        body: String::new(),
        labels: Vec::new(),
        assignees: Vec::new(),
        author: "octo".into(),
        state: IssueState::Open,
        url: format!("https://github.example.org/{repo}/issues/{number}"),
        etag: None,
    }
}

fn rule(name: &str, interval_s: u32) -> IntakeRule {
    IntakeRule {
        id: Uuid::new_v4(),
        name: name.into(),
        repo: repo(name),
        filter: RuleFilter::default(),
        workflow: String::new(),
        enabled: true,
        interval_s,
    }
}

struct Harness {
    mem: Arc<MemStore>,
    host: FakeCodeHost,
    wake: Arc<Notify>,
    unit: Arc<Mutex<f64>>,
    stop: watch::Sender<bool>,
    handle: tokio::task::JoinHandle<()>,
}

/// Starts the supervisor over `rules`. Every random draw is `unit`.
async fn start(rules: &[IntakeRule], unit: f64) -> Harness {
    let mem = Arc::new(MemStore::new());
    let host = FakeCodeHost::new();
    let mut tx = mem.begin(COMMUNITY).await.unwrap();
    for rule in rules {
        tx.intake().save(rule).await.unwrap();
        host.put_repo(&rule.repo);
    }
    tx.commit().await.unwrap();
    let unit = Arc::new(Mutex::new(unit));
    let random: Random = {
        let unit = unit.clone();
        Arc::new(move || *unit.lock().unwrap())
    };
    let wake = Arc::new(Notify::new());
    let shared = Arc::new(Shared {
        store: mem.clone(),
        directory: Arc::new(FakeDirectory::new()),
        catalog: Arc::new(Sources(Vec::new())),
        host: Arc::new(host.clone()),
        bus: Bus::new(0),
        community: COMMUNITY,
        hive: "ee".into(),
        outbox: Arc::new(Notify::new()),
        gate: Gate::default(),
        random,
    });
    let (stop, stopped) = watch::channel(false);
    let supervisor = IntakeSupervisor {
        shared,
        wake: wake.clone(),
    };
    let handle = tokio::spawn(supervisor.run(stopped));
    Harness {
        mem,
        host,
        wake,
        unit,
        stop,
        handle,
    }
}

impl Harness {
    /// Lets time pass, and every task that is ready run.
    async fn pass(&self, seconds: u64) {
        // Move in steps so a task that wakes and sleeps again is seen at each of its times.
        for _ in 0..seconds {
            tokio::time::advance(Duration::from_secs(1)).await;
            for _ in 0..8 {
                tokio::task::yield_now().await;
            }
        }
    }

    async fn settle(&self) {
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }
    }

    /// How many times the host was asked for `name`'s issues.
    fn reads(&self, name: &str) -> usize {
        let wanted = format!("list_issues example-org/{name} ");
        self.host
            .calls()
            .iter()
            .filter(|c| c.starts_with(&wanted))
            .count()
    }

    async fn failed(&self) -> Vec<serde_json::Value> {
        let mut tx = self.mem.begin(COMMUNITY).await.unwrap();
        tx.events()
            .after(0, 1000)
            .await
            .unwrap()
            .into_iter()
            .filter(|e| e.kind == "intake.failed")
            .map(|e| e.payload)
            .collect()
    }

    async fn finish(self) {
        self.stop.send_replace(true);
        self.handle.await.unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn the_first_pass_waits_a_random_part_of_a_fifth_of_the_interval() {
    // 600 seconds, a fifth is 120, and a draw of one half starts the pass at 60.
    let h = start(&[rule("api", 600)], 0.5).await;
    h.pass(59).await;
    assert_eq!(h.reads("api"), 0, "nothing is read before the delay");
    h.pass(2).await;
    assert_eq!(h.reads("api"), 1);
    h.finish().await;
}

#[tokio::test(start_paused = true)]
async fn rules_that_restart_together_do_not_read_together() {
    let rules: Vec<_> = ["a", "b", "c"].iter().map(|n| rule(n, 600)).collect();
    // With draws of zero every rule would read at once; the draws a fleet makes are spread.
    let h = start(&rules, 0.0).await;
    h.settle().await;
    assert_eq!(
        h.host.calls().len(),
        3,
        "a draw of zero starts every rule now"
    );
    h.finish().await;

    let h = start(&rules, 0.9).await;
    h.settle().await;
    assert!(
        h.host.calls().is_empty(),
        "a late draw holds every rule back"
    );
    h.finish().await;
}

#[tokio::test(start_paused = true)]
async fn a_pass_repeats_every_interval_plus_a_part_of_a_fifth() {
    let h = start(&[rule("api", 600)], 0.5).await;
    // First pass at 60, the next 600 + 60 later, at 720.
    h.pass(61).await;
    assert_eq!(h.reads("api"), 1);
    h.pass(658).await;
    assert_eq!(h.reads("api"), 1, "at 719 seconds");
    h.pass(3).await;
    assert_eq!(h.reads("api"), 2);
    h.finish().await;
}

#[tokio::test(start_paused = true)]
async fn a_rate_limited_pass_waits_what_the_host_asked_and_holds_the_other_rules() {
    let a = rule("a", 60);
    let b = rule("b", 60);
    let h = start(&[a, b], 0.0).await;
    h.host.fail_next(HostError::RateLimited {
        retry_after: Duration::from_secs(300),
    });
    // Loops were already started with a draw of zero: one read hit the limit, and the other
    // was held by the gate, so only one read has happened.
    h.settle().await;
    assert_eq!(h.host.calls().len(), 1, "{:?}", h.host.calls());
    h.pass(290).await;
    assert_eq!(
        h.host.calls().len(),
        1,
        "nothing is read while the limit holds"
    );
    h.pass(15).await;
    let calls = h.host.calls();
    assert!(
        calls.len() >= 2,
        "both rules read once the limit lifts: {calls:?}"
    );
    let failed = h.failed().await;
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0]["error"], "rate limited");
    h.finish().await;
}

#[tokio::test(start_paused = true)]
async fn a_failing_pass_waits_twice_as_long_each_time_and_is_recorded() {
    let h = start(&[rule("api", 60)], 0.0).await;
    for _ in 0..3 {
        h.host
            .fail_next(HostError::Other("GitHub answered 502".into()));
    }
    h.settle().await;
    assert_eq!(h.reads("api"), 1, "the first pass failed at once");
    // Waits: 120, 240, 480 seconds after the first, second and third failure.
    h.pass(119).await;
    assert_eq!(h.reads("api"), 1);
    h.pass(2).await;
    assert_eq!(h.reads("api"), 2, "at 121");
    h.pass(238).await;
    assert_eq!(h.reads("api"), 2, "at 359");
    h.pass(3).await;
    assert_eq!(h.reads("api"), 3, "at 362");
    let failed = h.failed().await;
    assert_eq!(
        failed
            .iter()
            .map(|p| p["failures"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        [1, 2, 3]
    );
    assert!(failed[0]["error"].as_str().unwrap().contains("502"));
    // The third failure waits 480 seconds, so the next read is at 842.
    h.pass(470).await;
    assert_eq!(h.reads("api"), 3, "at 832");
    h.pass(10).await;
    assert_eq!(h.reads("api"), 4, "at 842");
    // That read succeeded, so the next comes a plain interval later, at 902.
    h.pass(50).await;
    assert_eq!(h.reads("api"), 4, "at 892");
    h.pass(15).await;
    assert_eq!(h.reads("api"), 5, "at 907");
    h.finish().await;
}

#[tokio::test(start_paused = true)]
async fn a_saved_rule_restarts_its_loop_and_a_deleted_or_disabled_one_stops_it() {
    let original = rule("api", 600);
    let h = start(std::slice::from_ref(&original), 0.0).await;
    h.settle().await;
    assert_eq!(h.reads("api"), 1);

    // A changed interval starts the loop again, which reads at once with a draw of zero.
    let mut changed = original.clone();
    changed.interval_s = 300;
    let mut tx = h.mem.begin(COMMUNITY).await.unwrap();
    tx.intake().save(&changed).await.unwrap();
    tx.commit().await.unwrap();
    h.wake.notify_one();
    h.settle().await;
    assert_eq!(h.reads("api"), 2);
    h.pass(305).await;
    assert_eq!(h.reads("api"), 3, "and then reads on its new interval");

    // Switched off, it reads no more.
    changed.enabled = false;
    let mut tx = h.mem.begin(COMMUNITY).await.unwrap();
    tx.intake().save(&changed).await.unwrap();
    tx.commit().await.unwrap();
    h.wake.notify_one();
    h.settle().await;
    h.pass(1000).await;
    assert_eq!(h.reads("api"), 3);

    // Switched on and then deleted, it reads once and then no more.
    changed.enabled = true;
    let mut tx = h.mem.begin(COMMUNITY).await.unwrap();
    tx.intake().save(&changed).await.unwrap();
    tx.commit().await.unwrap();
    h.wake.notify_one();
    h.settle().await;
    assert_eq!(h.reads("api"), 4);
    let mut tx = h.mem.begin(COMMUNITY).await.unwrap();
    assert!(tx.intake().delete(changed.id).await.unwrap());
    tx.commit().await.unwrap();
    h.wake.notify_one();
    h.settle().await;
    h.pass(1000).await;
    assert_eq!(h.reads("api"), 4);
    h.finish().await;
}

#[tokio::test(start_paused = true)]
async fn a_new_issue_becomes_a_card_on_the_first_pass() {
    let api = rule("api", 600);
    let h = start(std::slice::from_ref(&api), 0.0).await;
    h.host.put_issue(issue(&api.repo, 7));
    h.pass(601).await;
    let mut tx = h.mem.begin(COMMUNITY).await.unwrap();
    let cards = tx.cards().list(false).await.unwrap();
    assert_eq!(
        cards.len(),
        1,
        "one card for issue 7, however many passes ran"
    );
    assert_eq!(cards[0].title, "Issue 7");
    h.finish().await;
}

#[tokio::test(start_paused = true)]
async fn the_comments_of_a_working_card_are_read_on_a_timer() {
    let api = rule("api", 3600);
    let h = start(std::slice::from_ref(&api), 0.5).await;
    let it = issue(&api.repo, 7);
    h.host.put_issue(it.clone());
    let card = Card {
        id: Uuid::new_v4(),
        title: "Issue 7".into(),
        description: String::new(),
        state: CardState::Running,
        workflow: "rpi".into(),
        archived: false,
        created_by: "aa".into(),
    };
    h.mem.seed_card(COMMUNITY, card.clone());
    let mut tx = h.mem.begin(COMMUNITY).await.unwrap();
    tx.issues().upsert(&it, Utc::now()).await.unwrap();
    tx.issues()
        .attach(&it.repo, 7, card.id, Some(api.id))
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let reads = |h: &Harness| {
        h.host
            .calls()
            .iter()
            .filter(|c| c.starts_with("list_comments"))
            .count()
    };
    h.pass(u64::from(COMMENT_INTERVAL_S) / 2 - 1).await;
    assert_eq!(reads(&h), 0);
    h.pass(3).await;
    assert_eq!(reads(&h), 1, "one read for the one working card");
    *h.unit.lock().unwrap() = 0.0;
    // The wait after the first read was drawn at one half: 120 plus 24 seconds.
    h.pass(u64::from(COMMENT_INTERVAL_S) + 30).await;
    assert_eq!(reads(&h), 2);
    h.finish().await;
}

#[tokio::test(start_paused = true)]
async fn a_stop_ends_every_loop() {
    let h = start(&[rule("a", 600), rule("b", 600)], 0.5).await;
    h.settle().await;
    // `finish` waits for the supervisor, which waits for its loops: it must not hang.
    tokio::time::timeout(Duration::from_secs(30), h.finish())
        .await
        .expect("the supervisor stopped");
}
