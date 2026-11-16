//! The timer loop over an in-memory store and paused time: what it does when timers are
//! overdue at start, when one cannot be carried out, when the store fails, and on a stop.
//!
//! Time here is two clocks. Tokio's is paused, and moves when every task is waiting. The
//! store's is a `ManualClock`, and moves only when a test says so, so a timer pushed later is
//! not due again until the test moves it.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use buzz_hive::timers::{TimerLoop, CATCH_UP_WINDOW, PARKED, PARK_AFTER};
use buzz_hive_core::bus::Bus;
use buzz_hive_core::domain::card::{Card, CardState};
use buzz_hive_core::domain::run::Run;
use buzz_hive_core::domain::timer::TimerKind;
use buzz_hive_core::orchestrator::timers::{due, next_in};
use buzz_hive_core::ports::store::{CommunityId, Job, JobState, Store, StoreError, Tx};
use buzz_hive_core::testing::{FakeDirectory, ManualClock, MemStore};
use buzz_hive_core::workflow::source::Sources;
use tokio::sync::{watch, Notify};
use uuid::Uuid;

const COMMUNITY: CommunityId = CommunityId::from_u128(1);

/// A store that can be told to fail, and counts how often it was asked.
struct Flaky {
    inner: Arc<MemStore>,
    failing: AtomicBool,
    asked: AtomicUsize,
}

#[async_trait::async_trait]
impl Store for Flaky {
    async fn begin(&self, community: CommunityId) -> Result<Box<dyn Tx>, StoreError> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        if self.failing.load(Ordering::SeqCst) {
            return Err(StoreError::Unavailable("down".into()));
        }
        self.inner.begin(community).await
    }
}

struct Harness {
    mem: Arc<MemStore>,
    flaky: Arc<Flaky>,
    clock: ManualClock,
    wake: Arc<Notify>,
    stop: watch::Sender<bool>,
    handle: tokio::task::JoinHandle<()>,
}

/// Starts the loop on an empty store. No package is in the catalog.
fn start() -> Harness {
    let clock = ManualClock::new();
    let mem = Arc::new(MemStore::with_clock(Arc::new(clock.clone())));
    let flaky = Arc::new(Flaky {
        inner: mem.clone(),
        failing: AtomicBool::new(false),
        asked: AtomicUsize::new(0),
    });
    let wake = Arc::new(Notify::new());
    let (stop, rx) = watch::channel(false);
    let timers = TimerLoop {
        store: flaky.clone(),
        directory: Arc::new(FakeDirectory::new()),
        catalog: Arc::new(Sources(Vec::new())),
        bus: Bus::new(0),
        community: COMMUNITY,
        hive: "ee".into(),
        wake: wake.clone(),
        outbox: Arc::new(Notify::new()),
    };
    let handle = tokio::spawn(timers.run(rx));
    Harness {
        mem,
        flaky,
        clock,
        wake,
        stop,
        handle,
    }
}

impl Harness {
    /// Timers whose run is gone: carrying one out only removes it, so it never fails.
    async fn arm_healthy(&self, count: usize) {
        let mut tx = self.mem.begin(COMMUNITY).await.unwrap();
        for _ in 0..count {
            tx.timers()
                .arm(Uuid::new_v4(), "stage", TimerKind::Timeout, Duration::ZERO)
                .await
                .unwrap();
        }
        tx.commit().await.unwrap();
    }

    /// An overdue timer of a live run whose package is not in the catalog, which fails every
    /// time it is carried out.
    async fn arm_stuck(&self) {
        let card = Card {
            id: Uuid::new_v4(),
            title: "stuck".into(),
            description: String::new(),
            state: CardState::Running,
            workflow: "gone".into(),
            archived: false,
            created_by: "aa".into(),
        };
        self.mem.seed_card(COMMUNITY, card.clone());
        let run = Run::new(card.id, "gone", "hash");
        let mut tx = self.mem.begin(COMMUNITY).await.unwrap();
        tx.runs().insert(&run).await.unwrap();
        tx.timers()
            .arm(run.id, "stage", TimerKind::Timeout, Duration::ZERO)
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }

    /// Timers that are due now and have not been carried out.
    async fn overdue(&self) -> usize {
        due(self.mem.as_ref(), COMMUNITY, 10_000)
            .await
            .unwrap()
            .len()
    }

    async fn next(&self) -> Option<Duration> {
        next_in(self.mem.as_ref(), COMMUNITY).await.unwrap()
    }

    /// Stops the loop and waits for it to end.
    async fn finish(self) {
        self.stop.send_replace(true);
        self.handle.await.unwrap();
    }
}

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

#[tokio::test(start_paused = true)]
async fn overdue_timers_at_start_are_spread_over_the_catch_up_window() {
    let h = start();
    h.arm_healthy(40).await;
    // The first pass starts within 2 seconds, so by 3 seconds it has run for at least one.
    tokio::time::sleep(secs(3)).await;
    let left = h.overdue().await;
    assert!(
        left > 0 && left < 40,
        "some are fired and some wait: {left} left"
    );
    tokio::time::sleep(secs(2) + CATCH_UP_WINDOW).await;
    assert_eq!(h.overdue().await, 0);
    h.finish().await;
}

#[tokio::test(start_paused = true)]
async fn every_batch_of_overdue_timers_is_spread_not_only_the_first() {
    let h = start();
    // More than two batches of 50.
    h.arm_healthy(120).await;
    // The first batch ends within 7 seconds of the start. A batch fired with no spread would
    // let the other two follow at once, and nothing would be left.
    tokio::time::sleep(secs(7) + Duration::from_millis(500)).await;
    assert!(
        h.overdue().await > 0,
        "later batches are still being spread"
    );
    tokio::time::sleep(secs(30)).await;
    assert_eq!(h.overdue().await, 0, "all of them are drained");
    h.finish().await;
}

#[tokio::test(start_paused = true)]
async fn a_timer_that_cannot_be_carried_out_does_not_hold_back_the_others() {
    let h = start();
    h.arm_stuck().await;
    h.arm_healthy(5).await;
    tokio::time::sleep(secs(2) + CATCH_UP_WINDOW + secs(1)).await;
    // The healthy ones are gone, and the stuck one is set again for later.
    assert_eq!(h.overdue().await, 0);
    let next = h.next().await.expect("the stuck timer is still set");
    assert!(
        next >= secs(2) && next <= secs(2) + Duration::from_millis(500),
        "after one failure it waits 2 seconds and a little more: {next:?}"
    );
    h.finish().await;
}

#[tokio::test(start_paused = true)]
async fn a_stuck_timer_waits_longer_each_time_and_is_parked_after_ten_failures() {
    let h = start();
    h.arm_stuck().await;
    // The first try comes after the start jitter and the spread.
    tokio::time::sleep(secs(2) + CATCH_UP_WINDOW + secs(1)).await;
    for failures in 1..=PARK_AFTER {
        let next = h.next().await.expect("the stuck timer is still set");
        if failures < PARK_AFTER {
            // 2 seconds, doubling, up to 5 minutes, with up to a quarter added.
            let base = secs(2 << (failures - 1)).min(secs(300));
            assert!(
                next >= base && next <= base + base / 4,
                "after {failures} failures: {next:?}, expected about {base:?}"
            );
        } else {
            assert!(
                next >= PARKED && next <= PARKED + secs(1),
                "parked after {failures} failures: {next:?}"
            );
            break;
        }
        // Bring it due, and give the loop time to wake and try it again.
        h.clock.advance(next);
        tokio::time::sleep(next.min(secs(300)) + secs(1)).await;
    }
    h.finish().await;
}

#[tokio::test(start_paused = true)]
async fn a_wake_fires_a_new_timer_at_once_even_while_another_is_stuck() {
    let h = start();
    h.arm_stuck().await;
    tokio::time::sleep(secs(2) + CATCH_UP_WINDOW + secs(1)).await;
    assert!(h.next().await.is_some(), "the stuck timer waits");

    // Far from the 5 minute backstop and from the stuck timer's wait.
    h.arm_healthy(1).await;
    h.wake.notify_one();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(h.overdue().await, 0, "the wake was honored");
    h.finish().await;
}

#[tokio::test(start_paused = true)]
async fn a_failing_store_backs_the_loop_off_and_ignores_wakes() {
    let h = start();
    h.arm_healthy(1).await;
    h.flaky.failing.store(true, Ordering::SeqCst);
    // Wait for the first failed pass.
    while h.flaky.asked.load(Ordering::SeqCst) == 0 {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let asked = h.flaky.asked.load(Ordering::SeqCst);
    for _ in 0..5 {
        h.wake.notify_one();
    }
    // Shorter than the first wait of 2 seconds.
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    assert_eq!(
        h.flaky.asked.load(Ordering::SeqCst),
        asked,
        "a wake does not start a pass while the store fails"
    );

    // Once the store is back the next pass carries the timer out.
    h.flaky.failing.store(false, Ordering::SeqCst);
    tokio::time::sleep(secs(30)).await;
    assert_eq!(h.overdue().await, 0);
    h.finish().await;
}

#[tokio::test(start_paused = true)]
async fn a_stop_during_the_spread_ends_the_loop_at_once() {
    let h = start();
    h.arm_healthy(40).await;
    tokio::time::sleep(secs(2) + Duration::from_millis(100)).await;
    assert!(h.overdue().await > 0, "the spread is under way");
    h.stop.send_replace(true);
    tokio::time::timeout(Duration::from_millis(10), h.handle)
        .await
        .expect("the loop ended within the stop")
        .unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_job_that_is_never_reported_ends_failed_when_its_deadline_passes() {
    let h = start();
    tokio::time::sleep(secs(2) + CATCH_UP_WINDOW).await;
    let id = Uuid::from_u128(5);
    let mut tx = h.mem.begin(COMMUNITY).await.unwrap();
    tx.jobs()
        .insert(
            &Job {
                id,
                kind: "chat".into(),
                requester: "aa".into(),
                state: JobState::Running,
                channel: Uuid::from_u128(77),
                root_event_id: Some("root".into()),
                agent: "dd".into(),
            },
            Some(secs(120)),
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    h.wake.notify_one();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let state = |h: &Harness| {
        let mem = h.mem.clone();
        async move {
            let mut tx = mem.begin(COMMUNITY).await.unwrap();
            tx.jobs().get_for_update(id).await.unwrap().unwrap().state
        }
    };
    assert_eq!(state(&h).await, JobState::Running, "not overdue yet");

    // The loop sleeps until the deadline, not for the whole backstop.
    h.clock.advance(secs(121));
    tokio::time::sleep(secs(125)).await;
    assert_eq!(state(&h).await, JobState::Failed);
    h.finish().await;
}
